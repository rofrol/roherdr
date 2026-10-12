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
        let prompt = params.prompt.clone();
        let caller_pane_id = params.caller_pane_id.clone();
        let started = match self.start_agent(params) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };
        // The agent is a worker of the coordinator whose pane started it, before its launch
        // prompt is registered, so that prompt is its assignment.
        self.register_started_worker(started.ws_idx, started.pane_id, caller_pane_id.as_deref());
        // Followed like a prompt `agent.prompt` typed, registered before the app handles
        // anything else, so the agent's report of the turn it starts always finds it.
        let prompt_request = prompt.map(|prompt| {
            self.follow_launch_prompt(
                started.ws_idx,
                started.pane_id,
                &prompt,
                caller_pane_id.as_deref(),
            )
        });

        encode_success(
            id,
            ResponseResult::AgentStarted {
                agent: started.agent,
                argv: started.argv,
                prompt_request,
            },
        )
    }

    /// Follows the first prompt an agent got as its launch argument. Its integration has not
    /// reported anything yet, so the request is `unsupported` until it reports its session or
    /// the turn (see `PromptTurns::unsupported_reason`).
    fn follow_launch_prompt(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        prompt: &str,
        caller_pane_id: Option<&str>,
    ) -> crate::api::schema::AgentPromptRequest {
        let request_id = next_prompt_request_id();
        let error = self
            .pane_terminal_mut(ws_idx, pane_id)
            .and_then(|terminal| {
                terminal.prompt_turns.accept(request_id.clone(), prompt);
                terminal
                    .prompt_turns
                    .unsupported_reason(terminal.effective_agent_label())
            });
        self.register_assignment(ws_idx, pane_id, &request_id, prompt, caller_pane_id, false);
        crate::api::schema::AgentPromptRequest {
            request_id,
            state: if error.is_some() {
                AgentPromptRequestState::Unsupported
            } else {
                AgentPromptRequestState::Accepted
            },
            error,
        }
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

    pub(super) fn type_agent_launch(
        &mut self,
        pane_id: &str,
        kind: crate::detect::Agent,
        args: &[String],
    ) -> Result<(), String> {
        let mut argv = vec![crate::detect::interactive_agent_executable(kind).to_string()];
        argv.extend_from_slice(args);
        self.type_command(pane_id, &argv)
    }

    /// Types `argv` into the pane's shell, encoded for that shell, and
    /// Enter.
    pub(super) fn type_command(&mut self, pane_id: &str, argv: &[String]) -> Result<(), String> {
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
        let command = crate::platform::interactive_shell_command(argv, &shell)
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
        if let Err(response) =
            self.start_agent_prompt(request.id, &params, respond_to.clone(), true)
        {
            let _ = respond_to.send(response);
        }
        true
    }

    /// Types a supervision notice (`crate::workers::supervision`) into the agent of `pane` as
    /// `agent.prompt` does, without registering an assignment: a notice is herdr's, not a
    /// coordinator's task to a worker, so no verdict is expected of it. A refusal before
    /// anything was typed is `Err` with its code and message; otherwise the receiver gets the
    /// prompt's reply once the text was submitted (or its submission failed).
    pub(crate) fn submit_supervision_notice(
        &mut self,
        pane: &str,
        text: String,
    ) -> Result<std::sync::mpsc::Receiver<String>, String> {
        let params = AgentPromptParams {
            target: pane.to_owned(),
            prefer_workspace_id: None,
            text,
            caller_pane_id: None,
            note: true,
            wait: None,
            follow_turn: false,
        };
        let (respond_to, reply) = std::sync::mpsc::channel();
        self.start_agent_prompt(
            "herdr:supervision-notice".into(),
            &params,
            respond_to,
            false,
        )
        .map_err(|response| {
            let value: serde_json::Value =
                serde_json::from_str(&response).unwrap_or(serde_json::Value::Null);
            format!(
                "{}: {}",
                value["error"]["code"]
                    .as_str()
                    .unwrap_or("agent_prompt_failed"),
                value["error"]["message"].as_str().unwrap_or(&response)
            )
        })?;
        Ok(reply)
    }

    /// Validates and submits a prompt; a refusal before anything was typed is returned, the
    /// prompt's reply after its submission goes to `respond_to`. `register` records the
    /// prompt as an assignment of the pane's agent.
    fn start_agent_prompt(
        &mut self,
        id: String,
        params: &AgentPromptParams,
        respond_to: std::sync::mpsc::Sender<String>,
        register: bool,
    ) -> Result<(), String> {
        let validated = self.validate_agent_prompt(id, params)?;
        // Start the completion waiter before submitting, so a refused thread
        // fails the request while nothing has been sent yet.
        let (handoff_tx, handoff_rx) = std::sync::mpsc::channel::<QueuedAgentPrompt>();
        let waiter_respond_to = respond_to;
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
            return Err(encode_error(
                validated.id,
                "agent_prompt_failed",
                format!("could not start prompt completion waiter: {err}"),
            ));
        }
        let submission = self.submit_agent_prompt(validated, params, register)?;
        let _ = handoff_tx.send(submission);
        Ok(())
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
        register: bool,
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
                let unsupported = terminal.prompt_turns.unsupported_reason(label.as_deref());
                // Followed either way: an agent whose integration reports its first turn only
                // now (one that started behind a dialog) follows this request from that report.
                terminal
                    .prompt_turns
                    .accept(request_id.clone(), &params.text);
                crate::api::schema::AgentPromptRequest {
                    request_id,
                    state: if unsupported.is_some() {
                        crate::api::schema::AgentPromptRequestState::Unsupported
                    } else {
                        crate::api::schema::AgentPromptRequestState::Accepted
                    },
                    error: unsupported,
                }
            })
            .ok_or_else(|| agent_not_found(id.clone(), &params.target))?;
        // Registered with the request, before the agent can take the prompt, so its verdict
        // is recorded even when nothing waits for it then.
        if register {
            self.register_assignment(
                ws_idx,
                pane_id,
                &prompt_request.request_id,
                &params.text,
                params.caller_pane_id.as_deref(),
                params.note,
            );
        }
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
                let Some(terminal) = self.state.terminals.get(terminal_id) else {
                    continue;
                };
                let Some(state) = terminal.prompt_turns.state_of(&params.request_id) else {
                    continue;
                };
                let error = terminal
                    .prompt_turns
                    .error_of(&params.request_id)
                    .map(str::to_string);
                let text = terminal
                    .prompt_turns
                    .text_of(&params.request_id)
                    .map(str::to_string);
                // A request the agent's integration cannot follow yet stays `unsupported`
                // until it reports a turn or a session with turn reports.
                let unsupported = (state == PromptTurnState::Accepted)
                    .then(|| {
                        terminal
                            .prompt_turns
                            .unsupported_reason(terminal.effective_agent_label())
                    })
                    .flatten();
                let error = unsupported.or(error);
                let state = match state {
                    PromptTurnState::Accepted if error.is_some() => {
                        AgentPromptRequestState::Unsupported
                    }
                    PromptTurnState::Accepted => AgentPromptRequestState::Accepted,
                    PromptTurnState::Working => AgentPromptRequestState::Working,
                    PromptTurnState::Finished => AgentPromptRequestState::Finished,
                    PromptTurnState::Failed => AgentPromptRequestState::Failed,
                    PromptTurnState::Interrupted => AgentPromptRequestState::Interrupted,
                    PromptTurnState::Exited => AgentPromptRequestState::Exited,
                    PromptTurnState::Unknown => AgentPromptRequestState::Unknown,
                };
                return encode_success(
                    id,
                    ResponseResult::AgentPromptStatus {
                        pane_id: self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
                        prompt_request: crate::api::schema::AgentPromptRequest {
                            request_id: params.request_id,
                            state,
                            error,
                        },
                        text,
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
            .cloned()
        else {
            return agent_not_found(id, &params.target);
        };
        let Some(expected_agent) = self
            .state
            .terminals
            .get(&terminal_id)
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
        // An explicit interrupt: the working prompt's turn, cut off by the next turn's start,
        // ends as interrupted rather than unknown.
        if params
            .keys
            .iter()
            .any(|key| super::super::api_helpers::is_interrupt_api_key(key))
        {
            if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                terminal.prompt_turns.interrupt_sent();
            }
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
    fn agent_background_tasks_tell_an_observed_zero_from_unknown() {
        let (mut app, agents) = app_with_agents_in_two_workspaces();
        let (_, pane) = &agents[0];
        assert_eq!(
            get_agent(&mut app, pane, None).unwrap().background_tasks,
            None
        );
        for (value, expected) in [
            (Some("2"), Some(2)),
            (Some("0"), Some(0)),
            (Some("2 bg"), None),
            (None, None),
        ] {
            let params = serde_json::from_value(serde_json::json!({
                "pane_id": pane,
                "source": "claude-bg",
                "tokens": { "bg_count": value },
            }))
            .unwrap();
            let response = app.handle_api_request(crate::api::schema::Request {
                id: "report".into(),
                method: crate::api::schema::Method::PaneReportMetadata(params),
            });
            assert!(response.contains("\"result\""), "{response}");
            let agent = get_agent(&mut app, pane, None).unwrap();
            assert_eq!(agent.background_tasks, expected, "{value:?}");
            let json = serde_json::to_value(&agent).unwrap();
            assert_eq!(
                json.get("background_tasks")
                    .and_then(|count| count.as_u64()),
                expected.map(u64::from),
                "{value:?}"
            );
        }
    }

    #[test]
    fn background_task_counts_are_bare_decimals() {
        use crate::api::schema::agents::background_task_count;
        assert_eq!(background_task_count("3"), Some(3));
        assert_eq!(background_task_count(" 0 "), Some(0));
        for value in ["", " ", "-1", "+1", "2 bg", "1.5", "99999999999"] {
            assert_eq!(background_task_count(value), None, "{value:?}");
        }
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
                caller_pane_id: None,
                note: false,
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
            agent_pid: None,
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
                caller_pane_id: None,
                note: false,
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
                caller_pane_id: None,
                note: false,
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
                caller_pane_id: None,
                note: false,
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

    /// A supervision notice is typed like `agent.prompt`, but it is herdr's, not a task: no
    /// assignment is registered for it, so its turn never becomes an assignment finding.
    #[tokio::test]
    async fn a_supervision_notice_is_typed_without_registering_an_assignment() {
        let mut app = app_with_agent();
        let (pane, mut rx) = claude_with_runtime(&mut app);
        let reply = app
            .submit_supervision_notice(&pane, "herdr supervision notice".into())
            .expect("an idle agent takes the notice");
        // The prompt's waiter sends the reply once the PTY actor wrote the text and Enter.
        let reply = reply.recv().expect("the notice's prompt replies");
        let request = prompt_request_of(&reply);
        assert!(crate::assignments::get(&request.request_id).is_none());
        assert_eq!(
            rx.try_recv().unwrap(),
            Bytes::from_static(b"herdr supervision notice")
        );
        assert_eq!(rx.try_recv().unwrap(), Bytes::from_static(b"\r"));

        // The same text through `agent.prompt` is an assignment.
        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("herdr supervision notice", false),
        ));
        assert!(crate::assignments::get(&request.request_id).is_some());
        while rx.try_recv().is_ok() {}

        // A refusal before typing is the notice's error, nothing is typed.
        let refused = app
            .submit_supervision_notice("w9:p9", "lost".into())
            .unwrap_err();
        assert!(refused.starts_with("agent_not_found: "), "{refused}");
        assert!(rx.try_recv().is_err());
    }

    /// R2: only a coordinator's prompt to its worker is an assignment awaiting a verdict; a note,
    /// a prompt to a pane that is not the sender's worker and one into a coordinator's pane are
    /// messages, closed at once. A prompt from a pane without a tenure is followed as before.
    #[tokio::test]
    async fn only_a_coordinators_prompt_to_its_worker_awaits_a_verdict() {
        let root = std::env::temp_dir().join(format!(
            "herdr-agent-prompt-messages-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        crate::workers::set_test_coordinators(crate::workers::WorkerSupervisor::open(
            root.join("workers"),
            "claude".into(),
        ));
        let supervisor = crate::workers::coordinators().unwrap();
        let repo = root.join("repo").display().to_string();
        supervisor.coordinator_start(&repo, "w9:p1", None).unwrap();
        let mut app = app_with_agent();
        let (pane, mut rx) = claude_with_runtime(&mut app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let mut prompt = |app: &mut App, caller: Option<&str>, note: bool| {
            let mut params = prompt_params("do the item", false);
            params.caller_pane_id = caller.map(str::to_owned);
            params.note = note;
            let request = prompt_request_of(&run_deferred_agent_prompt(app, "req", params));
            while rx.try_recv().is_ok() {}
            crate::assignments::get(&request.request_id).expect("the prompt is recorded")
        };
        let reason = |assignment: &crate::assignments::Assignment| {
            assignment
                .message
                .as_ref()
                .map(|message| message.reason.clone())
        };

        // Sent from a pane that holds no tenure: followed for its verdict, as before.
        let plain = prompt(&mut app, None, false);
        assert!(plain.is_open(), "{plain:?}");

        // A coordinator's prompt to a pane that is not its worker: a message.
        let stray = prompt(&mut app, Some("w9:p1"), false);
        assert!(!stray.is_open());
        assert!(
            reason(&stray).is_some_and(|reason| reason.contains("not its worker")),
            "{stray:?}"
        );
        assert_eq!(
            crate::assignments::info(&stray).message,
            reason(&stray),
            "the API shows why"
        );

        // A worker it started: an assignment.
        app.register_started_worker(0, pane_id, Some("w9:p1"));
        assert_eq!(
            crate::assignments::worker_pane_owner(&pane).map(|owner| owner.repo),
            Some(repo.clone())
        );
        let task = prompt(&mut app, Some("w9:p1"), false);
        assert!(task.is_open(), "{task:?}");
        // The same pane, marked as a note: a message.
        let note = prompt(&mut app, Some("w9:p1"), true);
        assert!(reason(&note).is_some_and(|reason| reason.contains("note")));

        // Another repository's coordinator prompting it: a message.
        let other = root.join("other").display().to_string();
        supervisor.coordinator_start(&other, "w9:p2", None).unwrap();
        let foreign = prompt(&mut app, Some("w9:p2"), false);
        assert!(
            reason(&foreign).is_some_and(|reason| reason.contains("a worker of coordinator")),
            "{foreign:?}"
        );

        // A worker-role tab nobody owns is claimed by the first coordinator that prompts it.
        crate::assignments::register_worker_pane(&pane, None);
        app.state.workspaces[0].tabs[0].role = Some(crate::api::schema::TabRole::Worker);
        assert!(prompt(&mut app, Some("w9:p2"), false).is_open());
        assert_eq!(
            crate::assignments::worker_pane_owner(&pane).map(|owner| owner.repo),
            Some(other)
        );

        // A prompt into a coordinator's pane: a message, whoever sends it.
        let third = root.join("third").display().to_string();
        supervisor.coordinator_start(&third, &pane, None).unwrap();
        let to_coordinator = prompt(&mut app, Some("w9:p2"), false);
        assert!(
            reason(&to_coordinator)
                .is_some_and(|reason| reason.contains("a message to coordinator")),
            "{to_coordinator:?}"
        );
        let _ = std::fs::remove_dir_all(root);
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
            caller_pane_id: None,
            note: false,
            follow_turn,
            target: "reviewer".into(),
            prefer_workspace_id: None,
            text: text.into(),
            wait: None,
        }
    }

    fn prompt_request_of(response: &str) -> crate::api::schema::AgentPromptRequest {
        let success: SuccessResponse = serde_json::from_str(response)
            .unwrap_or_else(|error| panic!("expected a prompt request ({error}): {response}"));
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

    /// A Claude pane that reports turns, with its transcript at `transcript`.
    fn claude_reporting_turns(
        app: &mut App,
        transcript: &std::path::Path,
    ) -> (String, tokio::sync::mpsc::Receiver<Bytes>) {
        let (pane, rx) = claude_with_runtime(app);
        app.handle_pane_report_agent_session(
            "session".into(),
            crate::api::schema::PaneReportAgentSessionParams {
                pane_id: pane.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("s1".into()),
                agent_session_path: Some(transcript.display().to_string()),
                session_start_source: None,
                resume_argv: None,
                turn_reports: true,
            },
        );
        (pane, rx)
    }

    /// A new directory of its own under the system's temporary directory.
    fn scratch_dir() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-agents-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn append_lines(path: &std::path::Path, lines: &[serde_json::Value]) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn assignment_of(app: &mut App, request_id: &str) -> crate::api::schema::AgentAssignmentInfo {
        let response = app.handle_agent_verdict(
            "verdict".into(),
            crate::api::schema::AgentVerdictParams {
                request_id: request_id.into(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentAssignment { assignment } = success.result else {
            panic!("expected an assignment: {response}");
        };
        assignment
    }

    #[tokio::test]
    async fn a_prompt_taken_in_mid_turn_ends_with_that_turn_and_its_verdict_is_recorded() {
        let dir = scratch_dir();
        let transcript = dir.join("session.jsonl");
        append_lines(
            &transcript,
            &[
                serde_json::json!({"type": "user", "message": {"role": "user", "content": "earlier work"}}),
            ],
        );
        let mut app = app_with_agent();
        let (pane, _rx) = claude_reporting_turns(&mut app, &transcript);
        report_turn(&mut app, &pane, "started", Some("earlier work"));

        // Typed while the agent works: Claude takes it in mid-turn and reports no turn start.
        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("do item two", false),
        ));
        assert_eq!(request.state, AgentPromptRequestState::Accepted);
        assert_eq!(assignment_of(&mut app, &request.request_id).verdict, None);
        append_lines(
            &transcript,
            &[
                serde_json::json!({"type": "queue-operation", "operation": "remove",
                    "content": "do item two", "reason": "absorbed_mid_turn", "commandUuid": "c1"}),
                serde_json::json!({"type": "attachment", "attachment": {"type": "queued_command",
                    "prompt": "do item two", "source_uuid": "c1"}}),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant",
                    "content": [{"type": "text", "text": "Done.\nWORKER-DONE abc1234 | item two"}]}}),
            ],
        );
        report_turn(&mut app, &pane, "finished", None);

        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Finished
        );
        let assignment = assignment_of(&mut app, &request.request_id);
        assert_eq!(assignment.pane_id, pane);
        assert_eq!(assignment.worker.as_deref(), Some("reviewer"));
        let verdict = assignment.verdict.expect("the verdict is recorded");
        assert_eq!(verdict.line, "WORKER-DONE abc1234 | item two");
        assert_eq!(verdict.sha.as_deref(), Some("abc1234"));
        assert_eq!(verdict.kind, crate::api::schema::AgentVerdictKind::Done);

        // Recorded once: a later turn end with another line changes nothing.
        append_lines(
            &transcript,
            &[
                serde_json::json!({"type": "assistant", "message": {"role": "assistant",
                "content": [{"type": "text", "text": "WORKER-BLOCKED something else"}]}}),
            ],
        );
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(
            assignment_of(&mut app, &request.request_id)
                .verdict
                .map(|verdict| verdict.line),
            Some("WORKER-DONE abc1234 | item two".to_string())
        );
    }

    /// A shell pane with a runtime, ready for `agent.start`.
    fn shell_with_runtime(app: &mut App) -> (String, tokio::sync::mpsc::Receiver<Bytes>) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id, runtime);
        (app.public_pane_id(0, pane_id).unwrap(), rx)
    }

    fn start_params(pane: &str, prompt: Option<&str>) -> AgentStartParams {
        AgentStartParams {
            name: "worker".into(),
            kind: "claude".into(),
            pane_id: pane.into(),
            args: Vec::new(),
            timeout_ms: None,
            prompt: prompt.map(str::to_string),
            caller_pane_id: None,
        }
    }

    #[tokio::test]
    async fn agent_start_with_a_prompt_follows_it_like_agent_prompt() {
        let dir = scratch_dir();
        crate::assignments::set_test_dir(Some(dir.join("assignments")));
        let mut app = app_with_agent();
        let (pane, mut rx) = shell_with_runtime(&mut app);

        let response =
            app.handle_agent_start("start".into(), start_params(&pane, Some("do the item")));
        let success: SuccessResponse = serde_json::from_str(&response).expect(&response);
        let ResponseResult::AgentStarted {
            argv,
            prompt_request: Some(request),
            ..
        } = success.result
        else {
            panic!("expected a started agent with a prompt request: {response}");
        };
        // The prompt is the launch's last argument.
        assert_eq!(argv, vec!["claude".to_string(), "do the item".to_string()]);
        let typed = rx.try_recv().unwrap();
        assert!(String::from_utf8_lossy(&typed).contains("do the item"));
        // Nothing reported yet: followed, but not known to be followable.
        assert_eq!(request.state, AgentPromptRequestState::Unsupported);
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Unsupported
        );

        // Claude's `SessionStart` ends the launch, and the turn its first prompt starts
        // follows the request.
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert!(app.state.terminals[&terminal_id].managed_agent_launch_pending());
        app.handle_pane_report_agent_session(
            "session".into(),
            crate::api::schema::PaneReportAgentSessionParams {
                pane_id: pane.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("s1".into()),
                agent_session_path: None,
                session_start_source: Some("startup".into()),
                resume_argv: None,
                turn_reports: true,
            },
        );
        assert!(app.state.terminals[&terminal_id].managed_agent_interactive_ready());
        assert_eq!(
            app.state.terminals[&terminal_id].agent_name.as_deref(),
            Some("worker")
        );
        report_turn(&mut app, &pane, "started", Some("do the item"));
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Working
        );
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Finished
        );
        crate::assignments::set_test_dir(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_due_launch_is_reconciled_when_the_runtime_wakes_for_it() {
        let mut app = app_with_agent();
        let (pane, _rx) = shell_with_runtime(&mut app);
        let started = app.handle_agent_start("start".into(), start_params(&pane, None));
        assert!(started.contains("agent_started"), "{started}");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let due = app.state.next_managed_agent_deadline().unwrap();
        // delay: not a wait, a virtual instant just before the settle delay ends.
        let just_before = due - Duration::from_millis(1);
        assert!(!app.reconcile_due_managed_agents(just_before));
        assert!(app.state.terminals[&terminal_id].managed_agent_launch_pending());
        // The runtime wakes at the deadline and reconciles, without any reader asking.
        assert!(app.reconcile_due_managed_agents(due));
        assert!(app.state.terminals[&terminal_id].managed_agent_interactive_ready());
        assert_eq!(app.state.next_managed_agent_deadline(), None);
    }

    #[tokio::test]
    async fn agent_start_refuses_a_prompt_it_cannot_pass() {
        let mut app = app_with_agent();
        let (pane, mut rx) = shell_with_runtime(&mut app);
        let mut params = start_params(&pane, Some("do the item"));
        params.kind = "opencode".into();
        let refused = app.handle_agent_start("start".into(), params);
        assert!(refused.contains("agent_prompt_unsupported"), "{refused}");
        let empty = app.handle_agent_start("start".into(), start_params(&pane, Some("  ")));
        assert!(empty.contains("empty_agent_prompt"), "{empty}");
        // Nothing was typed and no name was taken.
        assert!(rx.try_recv().is_err());
        let started = app.handle_agent_start("start".into(), start_params(&pane, None));
        let success: SuccessResponse = serde_json::from_str(&started).unwrap();
        let ResponseResult::AgentStarted { prompt_request, .. } = success.result else {
            panic!("expected a started agent: {started}");
        };
        assert_eq!(prompt_request, None);
    }

    #[tokio::test]
    async fn a_verdict_recorded_after_its_waiter_died_is_read_at_once_by_the_next_server() {
        let dir = scratch_dir();
        crate::assignments::set_test_dir(Some(dir.join("assignments")));
        let transcript = dir.join("session.jsonl");
        append_lines(
            &transcript,
            &[
                serde_json::json!({"type": "user", "message": {"role": "user", "content": "earlier work"}}),
            ],
        );
        let mut app = app_with_agent();
        let (pane, _rx) = claude_reporting_turns(&mut app, &transcript);
        report_turn(&mut app, &pane, "started", Some("earlier work"));
        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("do the item", false),
        ));
        // Its waiter (`agent.wait_verdict`) runs, then is killed before the worker finishes.
        drop(crate::assignments::wait_started(&request.request_id));
        append_lines(
            &transcript,
            &[
                serde_json::json!({"type": "attachment", "attachment": {"type": "queued_command",
                    "prompt": "do the item", "source_uuid": "c1"}}),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant",
                    "content": [{"type": "text", "text": "WORKER-DONE abc1234 | the item"}]}}),
            ],
        );
        report_turn(&mut app, &pane, "finished", None);
        // Nothing received it: the supervision sweep reports it.
        assert_eq!(
            crate::assignments::undelivered_unwatched()
                .iter()
                .map(|assignment| assignment.request_id.clone())
                .collect::<Vec<_>>(),
            vec![request.request_id.clone()]
        );

        // A live handoff: the new server knows no followed prompt, but reads the assignments
        // the old one stored.
        crate::assignments::set_test_dir(Some(dir.join("assignments")));
        let mut next = app_with_agent();
        let status = next.handle_agent_prompt_status(
            "status".into(),
            crate::api::schema::AgentPromptStatusParams {
                request_id: request.request_id.clone(),
            },
        );
        assert!(status.contains("prompt_request_not_found"), "{status}");
        let assignment = assignment_of(&mut next, &request.request_id);
        assert_eq!(assignment.pane_id, pane);
        assert_eq!(
            assignment.verdict.map(|verdict| verdict.line),
            Some("WORKER-DONE abc1234 | the item".to_string())
        );
        crate::assignments::set_test_dir(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The capture-doc worker's turn: a pasted prompt, and a `Stop` reported before Claude wrote
    /// the turn's last message to the transcript; `transcript_lines` is all the transcript
    /// shows then. Returns the verdict recorded for its request.
    fn verdict_of_a_turn_ending_before_its_last_message_is_written(
        transcript_lines: impl Fn(&str) -> Vec<serde_json::Value>,
    ) -> Option<String> {
        let dir = scratch_dir();
        let transcript = dir.join("session.jsonl");
        let mut app = app_with_agent();
        let (pane, _rx) = claude_reporting_turns(&mut app, &transcript);
        let task = "AGENTS.md: capture with grim\n\nRewrite the paragraph.\n\
                    Last line: `WORKER-DONE <sha> | <summary>`";
        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params(task, false),
        ));
        let pasted =
            format!("\n\n<pasted_content id=\"7bb5\">\n{task}\n</pasted_content id=\"7bb5\">\n");
        report_turn(&mut app, &pane, "started", Some(&pasted));
        assert_eq!(
            prompt_state(&mut app, &request.request_id),
            AgentPromptRequestState::Working
        );
        append_lines(&transcript, &transcript_lines(&pasted));
        report_turn_with(
            &mut app,
            serde_json::json!({"pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "finished", "background_tasks": 0,
                "final_text": "Rewrote it.\n\nWORKER-DONE 220dd3dd | grim by default"}),
        );
        let _ = std::fs::remove_dir_all(dir);
        assignment_of(&mut app, &request.request_id)
            .verdict
            .map(|verdict| verdict.line)
    }

    #[tokio::test]
    async fn a_turn_end_reported_before_its_last_message_is_written_records_the_verdict() {
        let expected = Some("WORKER-DONE 220dd3dd | grim by default".to_string());
        // The pasted prompt and the turn's tool calls are written, its last message is not.
        assert_eq!(
            verdict_of_a_turn_ending_before_its_last_message_is_written(|pasted| vec![
                serde_json::json!({"type": "user", "message": {"role": "user", "content": pasted}}),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant",
                    "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]}}),
                serde_json::json!({"type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "220dd3dd"}]}}),
            ]),
            expected
        );
        // Not even the prompt is written yet: the agent reported starting the turn with it.
        assert_eq!(
            verdict_of_a_turn_ending_before_its_last_message_is_written(|_| vec![
                serde_json::json!({"type": "attachment", "attachment": {"type": "hook_success"}}),
            ]),
            expected
        );
    }

    /// How the ledger accounts for an assignment after the turn end of a fixture: never
    /// `"silent"`, an open assignment a closed turn left with no mark.
    fn accounting_of(request_id: &str) -> (&'static str, Option<String>) {
        let record = crate::assignments::get(request_id).expect("the assignment is kept");
        let fact = record
            .turn_closed
            .as_ref()
            .map(|closed| closed.fact.clone());
        let outcome = if record.verdict.is_some() {
            "verdict"
        } else if record.superseded.is_some() {
            "superseded"
        } else if record.stopped_ms.is_some() {
            "stopped"
        } else if record
            .turn_closed
            .as_ref()
            .is_some_and(|closed| closed.queued)
        {
            "queued"
        } else if record.turn_closed.is_some() {
            "unaccounted"
        } else {
            "silent"
        };
        (outcome, fact)
    }

    /// One worker turn on a typed prompt: `started` is the prompt the agent reported its turn
    /// starting with (`None`: no start reported), `lines` what the transcript then shows (no
    /// transcript file at all when `None`; a JSON string is written as it is, a record cut
    /// off), `error` the error the turn ended on.
    fn live_turn_accounting(
        task: &str,
        started: Option<&str>,
        lines: Option<Vec<serde_json::Value>>,
        error: Option<&str>,
    ) -> (&'static str, Option<String>) {
        crate::assignments::set_test_dir(None);
        let dir = scratch_dir();
        let transcript = dir.join("session.jsonl");
        if lines.is_some() {
            append_lines(
                &transcript,
                &[
                    serde_json::json!({"type": "user", "message": {"role": "user",
                    "content": "earlier work"}}),
                ],
            );
        }
        let mut app = app_with_agent();
        let (pane, _rx) = claude_reporting_turns(&mut app, &transcript);
        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params(task, false),
        ));
        if let Some(prompt) = started {
            report_turn(&mut app, &pane, "started", Some(prompt));
        }
        if let Some(lines) = &lines {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&transcript)
                .unwrap();
            for line in lines {
                match line.as_str() {
                    Some(raw) => writeln!(file, "{raw}").unwrap(),
                    None => writeln!(file, "{line}").unwrap(),
                }
            }
        }
        let mut finished = serde_json::json!({"pane_id": pane, "source": "herdr:claude",
            "agent": "claude", "phase": "finished", "background_tasks": 0});
        if let Some(error) = error {
            finished["error"] = error.into();
        }
        report_turn_with(&mut app, finished);
        let _ = std::fs::remove_dir_all(dir);
        accounting_of(&request.request_id)
    }

    /// The total accounting at a live `Stop` (the capture-doc worker's missed verdict): every
    /// open assignment a closed turn left has a verdict, a stop or the unaccounted mark that
    /// the supervision sweep reports as `unaccounted_assignment`, whatever the transcript
    /// parser found, and never nothing.
    #[tokio::test]
    async fn a_closed_turn_accounts_for_every_open_assignment_whatever_the_transcript_shows() {
        let task = "AGENTS.md: capture with grim\n\nRewrite the paragraph.\n\
                    Last line: `WORKER-DONE <sha> | <summary>`";
        let pasted =
            format!("\n\n<pasted_content id=\"7bb5\">\n{task}\n</pasted_content id=\"7bb5\">\n");
        let user = |content: &str| serde_json::json!({"type": "user", "message": {"role": "user", "content": content}});
        let done = serde_json::json!({"type": "assistant", "message": {"role": "assistant",
            "content": [{"type": "text", "text": "Done.\nWORKER-DONE 220dd3dd | grim"}]}});
        let turn_end = serde_json::json!({"type": "system", "subtype": "turn_duration"});

        // pasted_content, as the capture-doc worker's record.
        assert_eq!(
            live_turn_accounting(
                task,
                Some(&pasted),
                Some(vec![user(&pasted), done.clone()]),
                None
            )
            .0,
            "verdict"
        );
        // multiline, other line endings.
        let crlf = task.replace('\n', "\r\n");
        assert_eq!(
            live_turn_accounting(
                task,
                Some(&crlf),
                Some(vec![user(&crlf), done.clone()]),
                None
            )
            .0,
            "verdict"
        );
        // queued_command: taken in mid-turn, no turn start of its own.
        assert_eq!(
            live_turn_accounting(
                task,
                None,
                Some(vec![
                    serde_json::json!({"type": "attachment", "attachment": {
                        "type": "queued_command", "prompt": task, "source_uuid": "c1"}}),
                    done.clone(),
                ]),
                None
            )
            .0,
            "verdict"
        );
        // A queued command the parser cannot read: the prompt stays queued, which the sweep
        // reports once the agent rests with its turn closed.
        assert_eq!(
            live_turn_accounting(
                task,
                None,
                Some(vec![
                    serde_json::json!({"type": "attachment", "attachment": {
                        "type": "queued_command", "prompt": {"blocks": [task]}}}),
                    done.clone(),
                ]),
                None
            )
            .0,
            "queued"
        );
        // The prompt's start was reported, the transcript shows another text and no WORKER
        // line: stopped without a verdict.
        assert_eq!(
            live_turn_accounting(
                task,
                Some(task),
                Some(vec![user("[Pasted text #1 +3 lines]"), turn_end.clone()]),
                None
            )
            .0,
            "stopped"
        );
        // A missing transcript is evidence, not silence.
        let (outcome, fact) = live_turn_accounting(task, Some(task), None, None);
        assert_eq!(outcome, "unaccounted");
        let fact = fact.unwrap_or_default();
        assert!(fact.contains("cannot read"), "{fact}");
        assert!(fact.contains("prompt state: working"), "{fact}");
        // A truncated transcript (a record cut off) and a start never reported: unaccounted.
        let (outcome, fact) = live_turn_accounting(
            task,
            Some("[Pasted text #1 +3 lines]"),
            Some(vec![serde_json::json!("{\"type\": \"user\", \"mess")]),
            None,
        );
        assert_eq!(outcome, "queued", "{fact:?}");
        // A turn that ended on an error.
        let (outcome, fact) =
            live_turn_accounting(task, Some(task), Some(vec![user(task)]), Some("overloaded"));
        assert_eq!(outcome, "unaccounted");
        assert!(fact.unwrap_or_default().contains("overloaded"));
        crate::assignments::set_test_dir(None);
    }

    #[tokio::test]
    async fn an_earlier_identical_prompt_before_the_anchor_does_not_count() {
        let dir = scratch_dir();
        let transcript = dir.join("session.jsonl");
        append_lines(
            &transcript,
            &[
                serde_json::json!({"type": "user", "message": {"role": "user", "content": "do the item"}}),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant",
                    "content": [{"type": "text", "text": "WORKER-DONE 1111111 | the old one"}]}}),
            ],
        );
        let mut app = app_with_agent();
        let (pane, _rx) = claude_reporting_turns(&mut app, &transcript);
        let request = prompt_request_of(&run_deferred_agent_prompt(
            &mut app,
            "req",
            prompt_params("do the item", false),
        ));
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(assignment_of(&mut app, &request.request_id).verdict, None);
    }

    #[test]
    fn an_unknown_assignment_is_not_found() {
        let mut app = app_with_agent();
        let response = app.handle_agent_verdict(
            "verdict".into(),
            crate::api::schema::AgentVerdictParams {
                request_id: "prompt_nope".into(),
            },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "prompt_request_not_found");
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
        assert!(request
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("has not reported a turn")));
        assert!(!request.request_id.is_empty());
        while rx.try_recv().is_ok() {}

        // Waiting for the turn is refused before anything is typed.
        let refused = run_deferred_agent_prompt(&mut app, "req2", prompt_params("again", true));
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&refused).unwrap();
        assert_eq!(error.error.code, "turn_tracking_unsupported");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn an_unsupported_prompt_follows_the_turn_its_first_report_starts() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        // An unnamed Claude pane, targeted by its pane id, whose integration has not reported
        // anything yet (it started behind the folder-trust dialog).
        app.state
            .terminals
            .values_mut()
            .for_each(|terminal| terminal.clear_agent_name());
        let mut params = prompt_params("do the item", false);
        params.target = pane.clone();

        let request = prompt_request_of(&run_deferred_agent_prompt(&mut app, "req", params));
        assert_eq!(request.state, AgentPromptRequestState::Unsupported);
        // Its status says so until the integration reports, so a wait on it can end at once.
        let status = prompt_request(&mut app, &request.request_id);
        assert_eq!(status.state, AgentPromptRequestState::Unsupported);
        assert!(status
            .error
            .is_some_and(|reason| reason.contains("has not reported a turn")));

        report_turn(&mut app, &pane, "started", Some("do the item"));
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

    /// A prompt herdr typed and whose turn the agent reported as started.
    fn working_prompt(app: &mut App, pane: &str) -> String {
        report_turn(app, pane, "finished", None);
        let request = prompt_request_of(&run_deferred_agent_prompt(
            app,
            "req",
            prompt_params("review the diff", true),
        ));
        report_turn(app, pane, "started", Some("review the diff"));
        request.request_id
    }

    fn prompt_request(app: &mut App, request_id: &str) -> crate::api::schema::AgentPromptRequest {
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
        prompt_request
    }

    #[tokio::test]
    async fn a_turn_reported_with_an_error_fails_the_prompt() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        let response = app.handle_pane_report_turn(
            "turn".into(),
            serde_json::from_value(serde_json::json!({
                "pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "finished", "error": "server_error: overloaded",
            }))
            .unwrap(),
        );
        assert!(serde_json::from_str::<SuccessResponse>(&response).is_ok());
        let request = prompt_request(&mut app, &request_id);
        assert_eq!(request.state, AgentPromptRequestState::Failed);
        assert_eq!(request.error.as_deref(), Some("server_error: overloaded"));
    }

    fn report_turn_with(app: &mut App, params: serde_json::Value) {
        let response =
            app.handle_pane_report_turn("turn".into(), serde_json::from_value(params).unwrap());
        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "{response}"
        );
    }

    #[tokio::test]
    async fn a_turn_cut_off_by_the_next_one_without_a_signal_ends_as_unknown() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        // No `Stop`, then a new turn: an Esc or a queued message, nothing tells which.
        report_turn(&mut app, &pane, "started", Some("do something else"));
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Unknown
        );
    }

    #[tokio::test]
    async fn a_turn_herdr_interrupts_ends_as_interrupted_when_the_next_one_starts() {
        let mut app = app_with_agent();
        let (pane, mut rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        let sent = app.handle_agent_send_keys(
            "keys".into(),
            AgentSendKeysParams {
                target: "reviewer".into(),
                prefer_workspace_id: None,
                keys: vec!["escape".into()],
            },
        );
        assert!(
            serde_json::from_str::<SuccessResponse>(&sent).is_ok(),
            "{sent}"
        );
        while rx.try_recv().is_ok() {}
        // Esc ends Claude's turn without a `Stop`; the next prompt starts a turn.
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Working
        );
        report_turn(&mut app, &pane, "started", Some("do something else"));
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Interrupted
        );
    }

    #[tokio::test]
    async fn an_end_reported_as_interrupted_interrupts_the_prompt() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        report_turn_with(
            &mut app,
            serde_json::json!({
                "pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "finished", "interrupted": true,
            }),
        );
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Interrupted
        );
    }

    #[test]
    fn a_done_is_held_while_the_turn_leaves_background_work_running() {
        let mut app = app_with_agent();
        app.state.workspaces.push(Workspace::test_new("other"));
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.toast_config.delay_seconds = 0;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let pane = app.public_pane_id(0, pane_id).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let set_state = |app: &mut App, state: AgentState| {
            app.state
                .handle_app_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Claude),
                    state,
                    visible_blocker: false,
                    visible_working: state == AgentState::Working,
                    process_exited: false,
                    observed_at: std::time::Instant::now(),
                });
        };
        let finish = |app: &mut App, tasks: Option<u32>| {
            let mut params = serde_json::json!({
                "pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "finished",
            });
            if let Some(tasks) = tasks {
                params["background_tasks"] = tasks.into();
            }
            let response =
                app.handle_pane_report_turn("turn".into(), serde_json::from_value(params).unwrap());
            assert!(
                serde_json::from_str::<SuccessResponse>(&response).is_ok(),
                "{response}"
            );
        };
        let finished_toast = |app: &mut App| {
            let deliveries = app
                .state
                .drain_due_agent_notifications(std::time::Instant::now());
            let toast = app.state.toast.take();
            assert!(deliveries.len() <= 1, "{deliveries:?}");
            toast.is_some_and(|toast| toast.kind == crate::app::state::ToastKind::Finished)
        };

        // The Stop reports two tasks still running before the screen idles: no done yet.
        set_state(&mut app, AgentState::Working);
        report_turn(&mut app, &pane, "started", Some("run the build"));
        finish(&mut app, Some(2));
        set_state(&mut app, AgentState::Idle);
        assert!(!finished_toast(&mut app));
        assert_eq!(
            app.state.terminals[&terminal_id].last_agent_completion_seq,
            None
        );
        assert!(app.state.workspaces[0].tabs[0].panes[&pane_id].seen);

        // A later report of none releases it, once.
        finish(&mut app, Some(0));
        assert!(finished_toast(&mut app));
        assert!(app.state.terminals[&terminal_id]
            .last_agent_completion_seq
            .is_some());
        assert!(!app.state.workspaces[0].tabs[0].panes[&pane_id].seen);
        finish(&mut app, Some(0));
        assert!(!finished_toast(&mut app));

        // A turn start forgets the last count: a turn ending without one notifies as before.
        set_state(&mut app, AgentState::Working);
        report_turn(&mut app, &pane, "started", Some("<task-notification>"));
        set_state(&mut app, AgentState::Idle);
        assert!(finished_toast(&mut app));
    }

    #[tokio::test]
    async fn screen_activity_after_the_stop_never_reopens_the_turn() {
        // The reported bug: Claude's recap runs as a subagent after the `Stop`
        // (`SubagentStop` two seconds later, its hooks carry `agent_id` and report nothing),
        // and the pane read `working` for sixteen minutes after the turn ended.
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let set_screen = |app: &mut App, state: AgentState| {
            app.state
                .handle_app_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Claude),
                    state,
                    visible_blocker: false,
                    visible_working: state == AgentState::Working,
                    process_exited: false,
                    observed_at: std::time::Instant::now(),
                });
        };
        let finish = |app: &mut App, tasks: Option<u32>| {
            let mut params = serde_json::json!({
                "pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "finished",
            });
            if let Some(tasks) = tasks {
                params["background_tasks"] = tasks.into();
            }
            let response =
                app.handle_pane_report_turn("turn".into(), serde_json::from_value(params).unwrap());
            assert!(
                serde_json::from_str::<SuccessResponse>(&response).is_ok(),
                "{response}"
            );
        };
        let state = |app: &App| app.state.terminals[&terminal_id].state;

        report_turn(&mut app, &pane, "started", Some("add the stats"));
        set_screen(&mut app, AgentState::Working);
        assert_eq!(state(&app), AgentState::Working);

        // The `Stop` with no background task arrives while the screen still shows the spinner.
        finish(&mut app, Some(0));
        assert_eq!(state(&app), AgentState::Idle);
        // The recap subagent's spinner, and any later redraw of it, keeps the pane idle.
        set_screen(&mut app, AgentState::Idle);
        set_screen(&mut app, AgentState::Working);
        assert_eq!(state(&app), AgentState::Idle);
        // A dialog still shows.
        set_screen(&mut app, AgentState::Blocked);
        assert_eq!(state(&app), AgentState::Blocked);
        set_screen(&mut app, AgentState::Working);
        assert_eq!(state(&app), AgentState::Idle);

        // A new `UserPromptSubmit` opens a turn: the screen decides again.
        report_turn(&mut app, &pane, "started", Some("deploy it"));
        assert_eq!(state(&app), AgentState::Working);

        // A turn that ends with background work, or with no count, leaves the screen in charge.
        finish(&mut app, Some(1));
        assert_eq!(state(&app), AgentState::Working);
        report_turn(&mut app, &pane, "started", Some("<task-notification>"));
        finish(&mut app, None);
        assert_eq!(state(&app), AgentState::Working);
    }

    #[test]
    fn a_done_after_held_turns_is_delivered_once() {
        let mut app = app_with_agent();
        app.state.workspaces.push(Workspace::test_new("other"));
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.toast_config.delay_seconds = 0;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let pane = app.public_pane_id(0, pane_id).unwrap();
        let set_state = |app: &mut App, state: AgentState| {
            app.state
                .handle_app_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Claude),
                    state,
                    visible_blocker: false,
                    visible_working: state == AgentState::Working,
                    process_exited: false,
                    observed_at: std::time::Instant::now(),
                });
        };
        let finish = |app: &mut App, tasks: Option<u32>| {
            let mut params = serde_json::json!({
                "pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "finished",
            });
            if let Some(tasks) = tasks {
                params["background_tasks"] = tasks.into();
            }
            let response =
                app.handle_pane_report_turn("turn".into(), serde_json::from_value(params).unwrap());
            assert!(
                serde_json::from_str::<SuccessResponse>(&response).is_ok(),
                "{response}"
            );
        };
        // Each done shown, at once (a plain turn with no toast delay) or from the due queue
        // the server drains in the same loop pass (a released one), is one notification the
        // server records in its history.
        let mut delivered = 0;
        let mut deliver = |app: &mut App| {
            let deliveries = app
                .state
                .drain_due_agent_notifications(std::time::Instant::now());
            assert!(deliveries.len() <= 1, "{deliveries:?}");
            if let Some(toast) = app.state.toast.take() {
                assert_eq!(toast.kind, crate::app::state::ToastKind::Finished);
                delivered += 1;
            }
            delivered
        };
        let turn = |app: &mut App, tasks: Option<u32>| {
            set_state(app, AgentState::Working);
            report_turn(app, &pane, "started", Some("run the build"));
            finish(app, tasks);
            set_state(app, AgentState::Idle);
        };

        // Plain turns: one done each.
        turn(&mut app, None);
        assert_eq!(deliver(&mut app), 1);
        turn(&mut app, Some(0));
        assert_eq!(deliver(&mut app), 2);

        // Turns that leave background work running: none.
        turn(&mut app, Some(2));
        assert_eq!(deliver(&mut app), 2);
        turn(&mut app, Some(1));
        assert_eq!(deliver(&mut app), 2);

        // The final turn end reports none: one done, and a repeat of it adds nothing.
        finish(&mut app, Some(0));
        assert_eq!(deliver(&mut app), 3);
        finish(&mut app, Some(0));
        assert_eq!(deliver(&mut app), 3);
    }

    #[tokio::test]
    async fn a_background_task_notification_turn_finishes_with_the_prompts_stop() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        // A background task the prompt started completes: Claude submits its notification as
        // a prompt while the prompt's work goes on, and its `Stop` comes later.
        report_turn_with(
            &mut app,
            serde_json::json!({
                "pane_id": pane, "source": "herdr:claude", "agent": "claude",
                "phase": "started", "continuation": true,
                "prompt": "<task-notification><tool-use-id>toolu_1</tool-use-id></task-notification>",
            }),
        );
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Working
        );
        report_turn(&mut app, &pane, "finished", None);
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Finished
        );
    }

    #[tokio::test]
    async fn the_agent_process_exiting_ends_its_prompt_as_exited() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at: std::time::Instant::now(),
        });
        assert_eq!(
            prompt_state(&mut app, &request_id),
            AgentPromptRequestState::Exited
        );
    }

    #[tokio::test]
    async fn a_restarted_server_does_not_know_the_old_prompt() {
        let mut app = app_with_agent();
        let (pane, _rx) = claude_with_runtime(&mut app);
        let request_id = working_prompt(&mut app, &pane);
        // Followed prompts live in memory only: a new server starts without them.
        let mut restarted = app_with_agent();
        let response = restarted.handle_agent_prompt_status(
            "status".into(),
            crate::api::schema::AgentPromptStatusParams { request_id },
        );
        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "prompt_request_not_found");
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
                caller_pane_id: None,
                note: false,
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
                    caller_pane_id: None,
                    note: false,
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
                caller_pane_id: None,
                note: false,
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
                caller_pane_id: None,
                note: false,
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
            None,
        );
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(pane_id, runtime);

        let response = run_deferred_agent_prompt(
            &mut app,
            "req-pending",
            AgentPromptParams {
                caller_pane_id: None,
                note: false,
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
