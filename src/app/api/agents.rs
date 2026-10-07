use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{
    AgentPromptParams, AgentRenameParams, AgentSendKeysParams, AgentStartParams, AgentTarget,
    PaneReadResult, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

use crate::api::schema::AgentPromptRequestState;
use crate::terminal::prompt_turns::PromptTurnState;

/// The request id, the agent, the followed prompt and the submission's completion.
type QueuedAgentPrompt = (
    String,
    crate::api::schema::AgentInfo,
    crate::api::schema::AgentPromptRequest,
    std::sync::mpsc::Receiver<std::io::Result<()>>,
);

const AGENT_PROMPT_SUBMIT_DELAY: Duration = Duration::from_millis(300);

// Codex's Windows input reader does not surface bracketed paste. It detects the prompt as a
// "paste burst" and, while that burst is buffered, rewrites a following Enter into a newline
// instead of submitting. The burst only flushes after an idle timeout, so any size-based delay is
// a timing guess that fails when ConPTY delivery lags it. Codex flushes a buffered burst
// synchronously when it receives a non-character key, so appending one after the paste gives the
// submission a deterministic paste boundary regardless of prompt size or delivery speed.
#[cfg(windows)]
fn append_codex_paste_boundary(runtime: &crate::terminal::TerminalRuntime, text: &mut Vec<u8>) {
    let keys = match crate::app::api_helpers::encode_api_keys(runtime, &["right".to_string()]) {
        Ok(keys) => keys,
        Err(key) => {
            tracing::warn!(key = %key, "failed to encode Codex paste boundary key");
            return;
        }
    };
    if let Some(key) = keys.into_iter().find(|bytes| !bytes.is_empty()) {
        text.extend_from_slice(&key);
    }
}

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        let prefer = target.prefer_workspace_id.as_deref();
        self.reconcile_managed_agent_target(&target.target, prefer);
        let agent = match self.agent_info_for_target(&target.target, prefer) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent =
            match self.focus_agent_target(&target.target, target.prefer_workspace_id.as_deref()) {
                Ok(agent) => agent,
                Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
            };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(
            &params.target,
            params.prefer_workspace_id.as_deref(),
            params.name,
        ) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let (agent, argv) = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_kind_list(&self, id: String) -> String {
        let path = std::env::var_os("PATH");
        let on_path = |executable: &str| {
            path.as_deref().is_some_and(|path| {
                std::env::split_paths(path)
                    .filter(|dir| dir.is_absolute())
                    .any(|dir| crate::pane::is_executable_file(&dir.join(executable)))
            })
        };
        let kinds = crate::detect::Agent::ALL
            .into_iter()
            .filter(|agent| on_path(crate::detect::interactive_agent_executable(*agent)))
            .map(|agent| crate::detect::agent_label(agent).to_string())
            .collect();
        encode_success(id, ResponseResult::AgentKindList { kinds })
    }

    /// A new tab whose shell is handed the agent's command, like a launch
    /// typed by hand: the agent is not named or managed by Herdr.
    pub(super) fn handle_tab_create_agent(
        &mut self,
        id: String,
        params: crate::api::schema::TabCreateAgentParams,
    ) -> String {
        let Some(kind) = crate::detect::parse_canonical_agent_label(&params.kind) else {
            return encode_error(
                id,
                "unsupported_agent_kind",
                format!("unsupported interactive agent kind {}", params.kind),
            );
        };
        let Some(ws_idx) = self.parse_workspace_id(&params.workspace_id) else {
            return encode_error(
                id,
                "workspace_not_found",
                format!("workspace {} not found", params.workspace_id),
            );
        };
        let response = self.create_tab_in_workspace(
            id,
            ws_idx,
            super::tabs::NewTabPlace::End,
            None,
            params.focus,
            None,
            Default::default(),
        );
        let Ok(crate::api::schema::SuccessResponse {
            result: ResponseResult::TabCreated { root_pane, .. },
            ..
        }) = serde_json::from_str(&response)
        else {
            return response;
        };
        if let Err(err) = self.type_agent_launch(&root_pane.pane_id, kind, &[]) {
            tracing::warn!(err, pane = root_pane.pane_id, "could not launch the agent");
        }
        response
    }

    pub(super) fn handle_agent_handoff(
        &mut self,
        id: String,
        params: crate::api::schema::AgentHandoffParams,
    ) -> String {
        let dirs = crate::agent_handoff::TranscriptDirs::from_env();
        match self.hand_off_agent(id, params, &dirs) {
            Ok((response, _prompt)) => response,
            Err(response) => response,
        }
    }

    /// Opens the new agent's tab and returns the response with the first
    /// prompt typed into it; an error response when the source session or its
    /// transcript is unknown, before any tab exists.
    pub(super) fn hand_off_agent(
        &mut self,
        id: String,
        params: crate::api::schema::AgentHandoffParams,
        dirs: &crate::agent_handoff::TranscriptDirs,
    ) -> Result<(String, String), String> {
        let Some(to) = crate::detect::parse_canonical_agent_label(&params.to)
            .filter(|agent| crate::agent_handoff::HANDOFF_TARGETS.contains(agent))
        else {
            return Err(encode_error(
                id,
                "unsupported_agent_kind",
                format!(
                    "cannot hand a session over to {}: use claude, pi or codex",
                    params.to
                ),
            ));
        };
        let pane_not_found = |id: String| {
            encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            )
        };
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return Err(pane_not_found(id));
        };
        let Some(terminal) = self.state.workspaces[ws_idx]
            .terminal_id(pane_id)
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
        else {
            return Err(pane_not_found(id));
        };
        // The running session, else the one that just exited (for example
        // at its usage limit).
        let session = terminal
            .hook_authority
            .as_ref()
            .and_then(|authority| {
                Some((
                    authority.agent_label.clone(),
                    authority.session_ref.clone()?,
                ))
            })
            .or_else(|| {
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .or(terminal.exited_agent_session())
                    .map(|session| (session.agent.clone(), session.session_ref.clone()))
            });
        let Some((agent, session_ref)) = session else {
            return Err(encode_error(
                id,
                "agent_session_unknown",
                format!(
                    "pane {} has no agent session herdr knows of",
                    params.pane_id
                ),
            ));
        };
        let task = terminal.reported_task().map(str::to_owned);
        let cwd = self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id);
        let transcript =
            match crate::agent_handoff::transcript_path(dirs, &agent, &session_ref, cwd.as_deref())
            {
                Ok(path) => path,
                Err(message) => {
                    return Err(encode_error(id, "agent_transcript_not_found", message))
                }
            };
        let (repository, revision) = cwd
            .as_deref()
            .and_then(crate::workspace::git_repo_and_head)
            .map_or((None, None), |(root, head)| (Some(root), head));
        let session_id = crate::agent_handoff::session_display_id(&session_ref);
        let prompt = crate::agent_handoff::handoff_prompt(&crate::agent_handoff::HandoffSource {
            agent: &agent,
            session_id: &session_id,
            transcript: &transcript,
            task: task.as_deref(),
            repository: repository.as_deref(),
            revision: revision.as_deref(),
            time: time::OffsetDateTime::now_utc(),
        });
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return Err(pane_not_found(id));
        };
        let response = self.create_tab_in_workspace(
            id,
            ws_idx,
            super::tabs::NewTabPlace::After(tab_idx),
            cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
            params.focus,
            None,
            Default::default(),
        );
        let Ok(crate::api::schema::SuccessResponse {
            result: ResponseResult::TabCreated { root_pane, .. },
            ..
        }) = serde_json::from_str(&response)
        else {
            return Err(response);
        };
        if let Err(err) =
            self.type_agent_launch(&root_pane.pane_id, to, std::slice::from_ref(&prompt))
        {
            tracing::warn!(err, pane = root_pane.pane_id, "could not launch the agent");
        }
        Ok((response, prompt))
    }

    fn type_agent_launch(
        &mut self,
        pane_id: &str,
        kind: crate::detect::Agent,
        args: &[String],
    ) -> Result<(), String> {
        let (ws_idx, pane) = self
            .parse_current_public_pane_id(pane_id)
            .ok_or("the new pane is gone")?;
        let terminal_id = self.state.workspaces[ws_idx]
            .terminal_id(pane)
            .cloned()
            .ok_or("the new pane has no terminal")?;
        let runtime = self
            .terminal_runtimes
            .get(&terminal_id)
            .ok_or("the new pane has no runtime")?;
        // A shell that has not reported itself yet is the configured one.
        let shell = crate::app::agents::available_shell_name(runtime).or_else(|| {
            let configured = Some(self.state.default_shell.clone())
                .filter(|shell| !shell.is_empty())
                .or_else(|| std::env::var("SHELL").ok())?;
            std::path::Path::new(&configured)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        let shell = shell.ok_or("no shell to type the launch into")?;
        let mut argv = vec![crate::detect::interactive_agent_executable(kind).to_string()];
        argv.extend_from_slice(args);
        let command = crate::platform::interactive_shell_command(&argv, &shell)
            .ok_or("the launch cannot be encoded for this shell")?;
        let bytes = crate::app::api_helpers::encode_api_submission(runtime, &command);
        runtime
            .try_send_bytes(Bytes::from(bytes))
            .map_err(|err| err.to_string())
    }

    pub(crate) fn handle_deferred_agent_api_request(
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let crate::api::schema::Method::AgentPrompt(params) = request.method else {
            return false;
        };
        let validated = match self.validate_agent_prompt(request.id, &params) {
            Ok(validated) => validated,
            Err(response) => {
                let _ = respond_to.send(response);
                return true;
            }
        };
        // Start the completion waiter before submitting, so a refused thread
        // fails the request while nothing has been sent yet.
        let (handoff_tx, handoff_rx) = std::sync::mpsc::channel::<QueuedAgentPrompt>();
        let waiter_respond_to = respond_to.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-agent-prompt", move || {
            let Ok((id, agent, prompt_request, completion)) = handoff_rx.recv() else {
                return;
            };
            let response = match completion.recv() {
                Ok(Ok(())) => encode_success(
                    id,
                    ResponseResult::AgentPrompted {
                        agent,
                        prompt_request: Some(prompt_request),
                    },
                ),
                Ok(Err(err)) if err.kind() == std::io::ErrorKind::TimedOut => {
                    encode_error(id, "timeout", err.to_string())
                }
                Ok(Err(err)) => encode_error(id, "agent_prompt_failed", err.to_string()),
                Err(_) => encode_error(id, "agent_prompt_failed", "pty actor closed"),
            };
            let _ = waiter_respond_to.send(response);
        });
        if let Err(err) = spawned {
            tracing::warn!(err = %err, "failed to spawn agent prompt thread");
            let _ = respond_to.send(encode_error(
                validated.id,
                "agent_prompt_failed",
                format!("could not start prompt completion waiter: {err}"),
            ));
            return true;
        }
        match self.submit_agent_prompt(validated, &params) {
            Ok(submission) => {
                let _ = handoff_tx.send(submission);
            }
            Err(response) => {
                let _ = respond_to.send(response);
            }
        }
        true
    }

    /// Runs every check that can reject a prompt without touching the pane.
    fn validate_agent_prompt(
        &self,
        id: String,
        params: &AgentPromptParams,
    ) -> Result<ValidatedAgentPrompt, String> {
        if params.text.is_empty() {
            return Err(encode_error(
                id,
                "empty_agent_prompt",
                "agent prompt must not be empty",
            ));
        }
        let resolved = match self
            .resolve_agent_target_preferring(&params.target, params.prefer_workspace_id.as_deref())
        {
            Ok(resolved) => resolved,
            Err(err) => return Err(encode_error_body(id, self.agent_target_error_body(err))),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return Err(agent_not_found(id, &params.target));
        };
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if terminal.state == crate::detect::AgentState::Blocked {
            return Err(encode_error(
                id,
                "agent_blocked",
                format!(
                    "agent {} is blocked and requires interactive input",
                    params.target
                ),
            ));
        }
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return Err(agent_not_ready(id, &params.target));
        };
        if params.follow_turn
            && !terminal
                .prompt_turns
                .supported_for(terminal.effective_agent_label())
        {
            return Err(encode_error(
                id,
                "turn_tracking_unsupported",
                format!(
                    "agent {} does not report its turns; its integration may be missing or older",
                    params.target
                ),
            ));
        }
        if terminal.managed_agent_launch_pending() {
            return Err(agent_not_ready(id, &params.target));
        }
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return Err(encode_error(
                id,
                "agent_not_ready",
                format!(
                    "agent {} is no longer the pane foreground process",
                    params.target
                ),
            ));
        }
        let Some(agent) = self.agent_info(resolved.ws_idx, resolved.pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        Ok(ValidatedAgentPrompt {
            id,
            ws_idx: resolved.ws_idx,
            pane_id: resolved.pane_id,
            expected_agent,
            agent,
        })
    }

    fn submit_agent_prompt(
        &mut self,
        validated: ValidatedAgentPrompt,
        params: &AgentPromptParams,
    ) -> Result<QueuedAgentPrompt, String> {
        let ValidatedAgentPrompt {
            id,
            ws_idx,
            pane_id,
            expected_agent,
            agent,
        } = validated;
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return Err(agent_not_found(id, &params.target));
        };
        #[cfg(windows)]
        let submit_deadline = params
            .wait
            .as_ref()
            .and_then(|wait| wait.submission_deadline);
        #[cfg(not(windows))]
        let submit_deadline = None;
        if expected_agent == crate::detect::Agent::GithubCopilot {
            // Copilot ignores synthetic Enter after focus loss until it receives focus gained.
            let focus = match crate::ghostty::encode_focus(crate::ghostty::FocusEvent::Gained) {
                Ok(focus) => focus,
                Err(err) => {
                    return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
                }
            };
            if let Err(err) = runtime.try_send_bytes(Bytes::from(focus)) {
                return Err(encode_error(id, "agent_prompt_failed", err.to_string()));
            }
        }
        let (text, enter) =
            crate::app::api_helpers::encode_api_submission_parts(runtime, &params.text);
        #[cfg(windows)]
        let text = if expected_agent == crate::detect::Agent::Codex {
            let mut text = text;
            append_codex_paste_boundary(runtime, &mut text);
            text
        } else {
            text
        };
        let completion = runtime
            .queue_user_input_submission(
                Bytes::from(text),
                Bytes::from(enter),
                AGENT_PROMPT_SUBMIT_DELAY,
                submit_deadline,
            )
            .map_err(|err| encode_error(id.clone(), "agent_prompt_failed", err.to_string()))?;
        // Registered before the app handles anything else, so the agent's turn report for
        // this prompt always finds it.
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
            .ok_or_else(|| agent_not_found(id.clone(), &params.target))?;
        let prompt_request = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .map(|terminal| {
                let request_id = next_prompt_request_id();
                let label = terminal.effective_agent_label().map(str::to_string);
                if terminal.prompt_turns.supported_for(label.as_deref()) {
                    terminal
                        .prompt_turns
                        .accept(request_id.clone(), &params.text);
                    crate::api::schema::AgentPromptRequest {
                        request_id,
                        state: crate::api::schema::AgentPromptRequestState::Accepted,
                    }
                } else {
                    crate::api::schema::AgentPromptRequest {
                        request_id,
                        state: crate::api::schema::AgentPromptRequestState::Unsupported,
                    }
                }
            })
            .ok_or_else(|| agent_not_found(id.clone(), &params.target))?;
        self.clear_awaiting_reply_on_pane_input(ws_idx, pane_id);
        Ok((id, agent, prompt_request, completion))
    }

    pub(super) fn handle_agent_prompt_status(
        &mut self,
        id: String,
        params: crate::api::schema::AgentPromptStatusParams,
    ) -> String {
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            let panes = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.iter())
                .map(|(pane_id, pane)| (*pane_id, &pane.attached_terminal_id));
            for (pane_id, terminal_id) in panes {
                let Some(state) = self
                    .state
                    .terminals
                    .get(terminal_id)
                    .and_then(|terminal| terminal.prompt_turns.state_of(&params.request_id))
                else {
                    continue;
                };
                let state = match state {
                    PromptTurnState::Accepted => AgentPromptRequestState::Accepted,
                    PromptTurnState::Working => AgentPromptRequestState::Working,
                    PromptTurnState::Finished => AgentPromptRequestState::Finished,
                };
                return encode_success(
                    id,
                    ResponseResult::AgentPromptStatus {
                        pane_id: self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
                        prompt_request: crate::api::schema::AgentPromptRequest {
                            request_id: params.request_id,
                            state,
                        },
                    },
                );
            }
        }
        encode_error(
            id,
            "prompt_request_not_found",
            format!(
                "no followed prompt {}; its pane closed, or the agent does not report turns",
                params.request_id
            ),
        )
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self
            .resolve_agent_target_preferring(&params.target, params.prefer_workspace_id.as_deref())
        {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let snapshot = crate::app::api_helpers::read_terminal_snapshot(
            pane,
            params.source,
            params.format,
            params.lines,
        );

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text: snapshot.text,
                    revision: 0,
                    truncated: snapshot.truncated,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self
            .resolve_agent_target_preferring(&target.target, target.prefer_workspace_id.as_deref())
        {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        if terminal.full_lifecycle_hook_authority_active() {
            let explain = serde_json::json!({
                "agent": terminal.effective_agent_label().unwrap_or("unknown"),
                "state": crate::detect::manifest::agent_state_label(terminal.state),
                "manifest_source": null,
                "manifest_version": null,
                "cached_remote_version": null,
                "local_override_shadowing_remote": false,
                "remote_update_status": null,
                "remote_update_error": null,
                "matched_rule": null,
                "visible_idle": false,
                "visible_blocker": false,
                "visible_working": false,
                "screen_detection_skipped": true,
                "screen_detection_skip_reason": "full_lifecycle_hook_authority",
                "skip_state_update": false,
                "skipped_update_reason": null,
                "fallback_reason": null,
                "warning": null,
                "evaluated_rules": [],
            });
            return encode_success(id, ResponseResult::AgentExplain { explain });
        }
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let value = crate::detect::manifest::explain_to_json_value(&explain);

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send_keys(
        &mut self,
        id: String,
        params: AgentSendKeysParams,
    ) -> String {
        let resolved = match self
            .resolve_agent_target_preferring(&params.target, params.prefer_workspace_id.as_deref())
        {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.effective_known_agent())
        else {
            return agent_not_ready(id, &params.target);
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if !super::super::agents::runtime_hosts_agent(runtime, expected_agent) {
            return agent_not_ready(id, &params.target);
        }
        let encoded = match super::super::api_helpers::encode_api_keys(runtime, &params.keys) {
            Ok(encoded) => encoded,
            Err(key) => {
                return encode_error(id, "invalid_key", format!("unsupported key {key}"));
            }
        };
        let bytes: Vec<u8> = encoded.into_iter().flatten().collect();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "agent_send_keys_failed", err.to_string());
        }
        self.clear_awaiting_reply_on_pane_input(resolved.ws_idx, resolved.pane_id);

        encode_success(id, ResponseResult::Ok {})
    }
}

struct ValidatedAgentPrompt {
    id: String,
    ws_idx: usize,
    pane_id: crate::layout::PaneId,
    expected_agent: crate::detect::Agent,
    agent: crate::api::schema::AgentInfo,
}

/// Unique within a server run, and across runs by the start time.
fn next_prompt_request_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    static RUN: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let run = RUN.get_or_init(crate::app::api_helpers::unix_ms_now);
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("prompt_{run:x}_{seq}")
}

fn agent_not_ready(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_ready",
        format!("agent {target} is not an active named agent"),
    )
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{AgentInfo, AgentStatus, SuccessResponse},
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app
    }

    /// Two workspaces: the first with two agent panes, the second with one.
    /// Returns each agent pane as (workspace index, public pane id).
    fn app_with_agents_in_two_workspaces() -> (App, Vec<(usize, String)>) {
        let mut app = app_with_agent();
        let mut first = Workspace::test_new("first");
        first.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![first, Workspace::test_new("second")];
        app.state.ensure_test_terminals();
        let mut agents = Vec::new();
        for ws_idx in 0..app.state.workspaces.len() {
            for pane_id in app.state.workspaces[ws_idx].tabs[0].layout.pane_ids() {
                let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                    .attached_terminal_id
                    .clone();
                app.state
                    .terminals
                    .get_mut(&terminal_id)
                    .unwrap()
                    .set_detected_state(Some(Agent::Pi), AgentState::Idle);
                agents.push((ws_idx, app.public_pane_id(ws_idx, pane_id).unwrap()));
            }
        }
        (app, agents)
    }

    fn rename_agent(app: &mut App, target: &str, name: &str) -> Result<AgentInfo, String> {
        let response = app.handle_agent_rename(
            "req".into(),
            AgentRenameParams {
                target: target.into(),
                prefer_workspace_id: None,
                name: Some(name.into()),
            },
        );
        agent_info_or_error_code(&response)
    }

    fn get_agent(app: &mut App, target: &str, prefer: Option<&str>) -> Result<AgentInfo, String> {
        let response = app.handle_agent_get(
            "req".into(),
            AgentTarget {
                target: target.into(),
                prefer_workspace_id: prefer.map(str::to_owned),
            },
        );
        agent_info_or_error_code(&response)
    }

    fn agent_info_or_error_code(response: &str) -> Result<AgentInfo, String> {
        let value: serde_json::Value = serde_json::from_str(response).unwrap();
        if let Some(code) = value["error"]["code"].as_str() {
            return Err(code.to_owned());
        }
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        Ok(agent)
    }

    #[test]
    fn agent_names_are_unique_per_workspace() {
        let (mut app, agents) = app_with_agents_in_two_workspaces();
        let [(_, first), (_, second), (_, other_workspace)] = agents.as_slice() else {
            panic!("expected three agents");
        };

        assert!(rename_agent(&mut app, first, "reviewer").is_ok());
        assert!(rename_agent(&mut app, other_workspace, "reviewer").is_ok());
        assert_eq!(
            rename_agent(&mut app, second, "reviewer").unwrap_err(),
            "agent_name_taken"
        );
    }

    #[test]
    fn agent_name_resolves_in_the_preferred_workspace_first() {
        let (mut app, agents) = app_with_agents_in_two_workspaces();
        let [(_, first), (_, second), (_, other_workspace)] = agents.as_slice() else {
            panic!("expected three agents");
        };
        rename_agent(&mut app, first, "reviewer").unwrap();
        rename_agent(&mut app, other_workspace, "reviewer").unwrap();
        rename_agent(&mut app, second, "solo").unwrap();
        let first_ws = app.public_workspace_id(0);
        let second_ws = app.public_workspace_id(1);

        let agent = get_agent(&mut app, "reviewer", Some(&first_ws)).unwrap();
        assert_eq!(&agent.pane_id, first);
        let agent = get_agent(&mut app, "reviewer", Some(&second_ws)).unwrap();
        assert_eq!(&agent.pane_id, other_workspace);

        // Without a preference (`--global`, or a caller outside any pane) the
        // name stays ambiguous, as does an unknown preferred workspace.
        assert_eq!(
            get_agent(&mut app, "reviewer", None).unwrap_err(),
            "agent_target_ambiguous"
        );
        assert_eq!(
            get_agent(&mut app, "reviewer", Some("w_missing")).unwrap_err(),
            "agent_target_ambiguous"
        );

        // A name the preferred workspace lacks still resolves when unique.
        let agent = get_agent(&mut app, "solo", Some(&second_ws)).unwrap();
        assert_eq!(&agent.pane_id, second);

        // Pane ids ignore the preference.
        let agent = get_agent(&mut app, other_workspace, Some(&first_ws)).unwrap();
        assert_eq!(&agent.pane_id, other_workspace);
    }

    #[test]
    fn agent_name_shared_by_other_workspaces_is_ambiguous_from_a_third() {
        let (mut app, agents) = app_with_agents_in_two_workspaces();
        let [(_, first), _, (_, other_workspace)] = agents.as_slice() else {
            panic!("expected three agents");
        };
        rename_agent(&mut app, first, "reviewer").unwrap();
        rename_agent(&mut app, other_workspace, "reviewer").unwrap();
        app.state.workspaces.push(Workspace::test_new("third"));
        app.state.ensure_test_terminals();
        let third_ws = app.public_workspace_id(2);

        assert_eq!(
            get_agent(&mut app, "reviewer", Some(&third_ws)).unwrap_err(),
            "agent_target_ambiguous"
        );
    }

    fn start_deferred_agent_prompt(
        app: &mut App,
        id: &str,
        params: AgentPromptParams,
    ) -> std::sync::mpsc::Receiver<String> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(app.handle_deferred_agent_api_request(
            crate::api::schema::Request {
                id: id.into(),
                method: crate::api::schema::Method::AgentPrompt(params),
            },
            respond_to,
        ));
        response_rx
    }

    fn run_deferred_agent_prompt(app: &mut App, id: &str, params: AgentPromptParams) -> String {
        start_deferred_agent_prompt(app, id, params)
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds after submission")
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_codex_prompt_flushes_paste_burst_before_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                follow_turn: false,
                target: "reviewer".into(),
                prefer_workspace_id: None,
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        // The non-character key must precede Enter so Codex commits the paste burst first.
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B\x1b[C"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
    }

    #[tokio::test]
    async fn a_false_process_exit_makes_a_named_live_agent_unreachable_by_name() {
        // Reproduces the registration loss reported on #3225 by rszrszrsz:
        // a live agent pane with an assigned name stops resolving by that name
        // while its process keeps running, and renaming is the only recovery.
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let observed_at = std::time::Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());

        let found = app.handle_agent_get(
            "req:before".into(),
            AgentTarget {
                target: "reviewer".into(),
                prefer_workspace_id: None,
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&found).is_ok(),
            "the assigned name must resolve while the agent is running: {found}"
        );

        // One process-exit observation, then the same agent is observed alive
        // again on the next probe - the process never actually went away.
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at,
        });
        app.handle_internal_event(crate::events::AppEvent::AgentProcessDetected {
            pane_id,
            agent: Agent::Pi,
            observed_at: observed_at + std::time::Duration::from_secs(1),
        });

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.detected_agent,
            Some(Agent::Pi),
            "the agent process is still there"
        );

        let after = app.handle_agent_get(
            "req:after".into(),
            AgentTarget {
                target: "reviewer".into(),
                prefer_workspace_id: None,
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&after).is_ok(),
            "a live agent must stay reachable by its assigned name: {after}"
        );
    }

    #[tokio::test]
    async fn agent_prompt_sends_text_then_delays_enter() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 2,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let bracketed_started = std::time::Instant::now();
        let response_rx = start_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                follow_turn: false,
                target: public_pane_id,
                prefer_workspace_id: None,
                text: "A != B".into(),
                wait: None,
            },
        );
        assert!(response_rx.try_recv().is_err());
        let response = response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("agent prompt responds after submission");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPrompted { agent, .. } = success.result else {
            panic!("expected prompted response");
        };
        assert_eq!(agent.name.as_deref(), Some("reviewer"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert!(bracketed_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        app.lookup_runtime_sender(0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004l");
        let raw_started = std::time::Instant::now();
        let raw = run_deferred_agent_prompt(
            &mut app,
            "req-raw",
            AgentPromptParams {
                follow_turn: false,
                target: "reviewer".into(),
                prefer_workspace_id: None,
                text: "A != B".into(),
                wait: None,
            },
        );
        let raw: SuccessResponse = serde_json::from_str(&raw).unwrap();
        assert!(matches!(raw.result, ResponseResult::AgentPrompted { .. }));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"A != B"));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
        assert!(raw_started.elapsed() >= AGENT_PROMPT_SUBMIT_DELAY);

        let rejected = run_deferred_agent_prompt(
            &mut app,
            "req-label",
            AgentPromptParams {
                follow_turn: false,
                target: "opencode".into(),
                prefer_workspace_id: None,
                text: "wrong target".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "agent_not_found");
        assert!(rx.try_recv().is_err());
    }

    fn claude_with_runtime(app: &mut App) -> (String, tokio::sync::mpsc::Receiver<Bytes>) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);
        (app.public_pane_id(0, pane_id).unwrap(), rx)
    }

    fn prompt_params(text: &str, follow_turn: bool) -> AgentPromptParams {
        AgentPromptParams {
            follow_turn,
            target: "reviewer".into(),
            prefer_workspace_id: None,
            text: text.into(),
            wait: None,
        }
    }

    fn prompt_request_of(response: &str) -> crate::api::schema::AgentPromptRequest {
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        let ResponseResult::AgentPrompted {
            prompt_request: Some(request),
            ..
        } = success.result
        else {
            panic!("expected a prompt request: {response}");
        };
        request
    }

    fn report_turn(app: &mut App, pane_id: &str, phase: &str, prompt: Option<&str>) {
        let mut params = serde_json::json!({
            "pane_id": pane_id, "source": "herdr:claude", "agent": "claude", "phase": phase,
        });
        if let Some(prompt) = prompt {
            params["prompt"] = prompt.into();
        }
        let response =
            app.handle_pane_report_turn("turn".into(), serde_json::from_value(params).unwrap());
        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "{response}"
        );
    }

    fn prompt_state(app: &mut App, request_id: &str) -> AgentPromptRequestState {
        let response = app.handle_agent_prompt_status(
            "status".into(),
            crate::api::schema::AgentPromptStatusParams {
                request_id: request_id.into(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentPromptStatus { prompt_request, .. } = success.result else {
            panic!("expected a prompt status: {response}");
        };
        prompt_request.state
    }

    #[tokio::test]
    async fn a_prompt_to_an_agent_without_turn_reports_is_unsupported() {
        let mut app = app_with_agent();
        let (_pane, mut rx) = claude_with_runtime(&mut app);

        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("review the diff", false),
        ));
        assert_eq!(request.state, AgentPromptRequestState::Unsupported);
        assert!(!request.request_id.is_empty());
        while rx.try_recv().is_ok() {}

        // Waiting for the turn is refused before anything is typed.
        let refused = run_deferred_agent_prompt(&mut app, "req2", prompt_params("again", true));
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&refused).unwrap();
        assert_eq!(error.error.code, "turn_tracking_unsupported");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_prompt_follows_its_turn_through_the_hook_reports() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        report_turn(&mut app, &pane, "finished", None);

        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("review the diff", true),
        ));
        assert_eq!(request.state, AgentPromptRequestState::Accepted);
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Accepted
        );

        report_turn(&mut app, &pane, "started", Some("review the diff\n"));
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Working
        );
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Finished
        );
    }

    #[tokio::test]
    async fn a_turn_running_before_the_prompt_does_not_finish_it() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        report_turn(&mut app, &pane, "started", Some("the user's own prompt"));

        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("review the diff", true),
        ));
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Accepted
        );

        report_turn(&mut app, &pane, "started", Some("review the diff"));
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Finished
        );
    }

    #[tokio::test]
    async fn a_session_report_with_turn_reports_makes_prompts_followed() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        app.handle_pane_report_agent_session(
            "session".into(),
            crate::api::schema::PaneReportAgentSessionParams {
                pane_id: pane,
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("s1".into()),
                agent_session_path: None,
                session_start_source: None,
                resume_argv: None,
                turn_reports: true,
            },
        );

        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("hello", false),
        ));
        assert_eq!(request.state, AgentPromptRequestState::Accepted);
    }

    #[test]
    fn an_unknown_prompt_request_is_not_found() {
        let mut app = app_with_agent();
        let response = app.handle_agent_prompt_status(
            "status".into(),
            crate::api::schema::AgentPromptStatusParams {
                request_id: "prompt_nope".into(),
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "prompt_request_not_found");
    }

    #[tokio::test]
    async fn agent_prompt_waiter_spawn_failure_fails_before_submitting() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        crate::thread_spawn::test_hook::fail_next_spawns(1);
        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                follow_turn: false,
                target: "reviewer".into(),
                prefer_workspace_id: None,
                text: "hello".into(),
                wait: None,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_prompt_failed");
        std::thread::sleep(AGENT_PROMPT_SUBMIT_DELAY * 2);
        assert!(rx.try_recv().is_err(), "a failed prompt must not be sent");
    }

    #[tokio::test]
    async fn agent_prompt_validation_errors_win_over_waiter_spawn_failure() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        for (target, text, code) in [
            ("reviewer", "", "empty_agent_prompt"),
            ("missing", "hello", "agent_not_found"),
        ] {
            crate::thread_spawn::test_hook::fail_next_spawns(1);
            let response = run_deferred_agent_prompt(
                &mut app,
                "req",
                AgentPromptParams {
                    follow_turn: false,
                    target: target.into(),
                    prefer_workspace_id: None,
                    text: text.into(),
                    wait: None,
                },
            );

            let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(error.error.code, code);
            assert!(
                crate::thread_spawn::spawn_named("probe", || {}).is_err(),
                "a rejected prompt must not start a waiter thread"
            );
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_blocked_agent_without_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::GithubCopilot), AgentState::Blocked);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                follow_turn: false,
                target: "reviewer".into(),
                prefer_workspace_id: None,
                text: "unrelated prompt".into(),
                wait: None,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_blocked");
        assert!(
            tokio::time::timeout(
                AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "blocked prompt wrote or scheduled terminal input"
        );
    }

    #[tokio::test]
    async fn agent_prompt_focuses_copilot_before_submitting() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::GithubCopilot), AgentState::Idle);
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 3,
            );
        runtime.test_process_pty_bytes(b"\x1b[?2004h");
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req",
            AgentPromptParams {
                follow_turn: false,
                target: "reviewer".into(),
                prefer_workspace_id: None,
                text: "A != B".into(),
                wait: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::AgentPrompted { .. }
        ));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[I"));
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"\x1b[200~A != B\x1b[201~")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));
    }

    #[tokio::test]
    async fn agent_send_keys_validates_every_key_before_writing() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("reviewer".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let rejected = app.handle_agent_send_keys(
            "req-invalid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                prefer_workspace_id: None,
                keys: vec!["enter".into(), "not-a-key".into()],
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&rejected).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert!(rx.try_recv().is_err());

        let sent = app.handle_agent_send_keys(
            "req-valid".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                prefer_workspace_id: None,
                keys: vec!["up".into(), "enter".into()],
            },
        );
        let success: SuccessResponse = serde_json::from_str(&sent).unwrap();
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\x1b[A\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn agent_prompt_rejects_managed_agent_while_startup_is_pending() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let now = std::time::Instant::now();
        terminal.begin_managed_agent(
            "reviewer".into(),
            Agent::OpenCode,
            now,
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(10),
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req-pending",
            AgentPromptParams {
                follow_turn: false,
                target: "reviewer".into(),
                prefer_workspace_id: None,
                text: "A != B".into(),
                wait: None,
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_not_ready");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = app_with_agent();
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: app.public_pane_id(0, pane_id).unwrap(),
                prefer_workspace_id: None,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
    }

    #[test]
    fn agent_rename_does_not_replace_the_pane_label() {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_manual_label("shell-pane".into());
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let target = app.public_pane_id(0, pane_id).unwrap();

        for name in [Some("reviewer".to_string()), None] {
            let response = app.handle_agent_rename(
                "req".into(),
                AgentRenameParams {
                    target: target.clone(),
                    prefer_workspace_id: None,
                    name,
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert!(matches!(success.result, ResponseResult::AgentInfo { .. }));
            assert_eq!(
                app.state.terminals[&terminal_id].manual_label.as_deref(),
                Some("shell-pane")
            );
        }
    }
}
