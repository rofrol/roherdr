//! Each agent's context size (`crate::agent_context`), reconciled with its transcript at the
//! named events that call these: server start and live handoff, the agent's session start,
//! its turn end and its change to idle or done. Never on a timer, never in render.

use std::path::PathBuf;

use crate::agent_context::ContextEvent;
use crate::app::App;

impl App {
    /// Server start or live handoff: every pane's agent session goes on from where its
    /// cursor stopped.
    pub(crate) fn reconcile_agent_contexts(&mut self) {
        let panes = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, workspace)| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.panes.keys().copied())
                    .map(move |pane_id| (ws_idx, pane_id))
            })
            .collect::<Vec<_>>();
        for (ws_idx, pane_id) in panes {
            self.reconcile_agent_context(ws_idx, pane_id, ContextEvent::Reconcile);
        }
    }

    /// Reads what the pane's agent session appended to its transcript and keeps its reading
    /// on the agent's state; a change emits `pane.updated`. Returns whether it changed.
    pub(super) fn reconcile_agent_context(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        event: ContextEvent,
    ) -> bool {
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
        else {
            return false;
        };
        let session = crate::app::creation::terminal_agent_session_info(terminal)
            .filter(|session| session.kind == crate::agent_resume::AgentSessionRefKind::Id)
            .filter(|session| crate::agent_context::supports(&session.agent));
        let reported = terminal
            .prompt_turns
            .transcript()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let reading = session.and_then(|session| {
            let key = crate::usage::estimate::SessionKey::Id(session.value.clone());
            self.agent_contexts.reconcile(
                &session.agent,
                &session.value,
                reported.as_deref(),
                || {
                    crate::usage::estimate::session_transcript(
                        &session.agent,
                        &key,
                        &crate::usage::estimate::TranscriptRoots::from_env(),
                    )
                },
                event,
                crate::assignments::now_ms(),
            )
        });
        let Some(terminal) = self.pane_terminal_mut(ws_idx, pane_id) else {
            return false;
        };
        if terminal.agent_context == reading {
            return false;
        }
        terminal.agent_context = reading;
        terminal.revision = terminal.revision.saturating_add(1);
        self.emit_pane_updated(ws_idx, pane_id);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::app::App;
    use crate::detect::Agent;

    fn request(method: &str, params: serde_json::Value) -> crate::api::schema::Request {
        serde_json::from_value(serde_json::json!({
            "id": method,
            "method": method,
            "params": params,
        }))
        .unwrap()
    }

    fn usage_line(id: &str, input: u64, write: u64, read: u64) -> String {
        // After any session start a test makes: the request is the session's.
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2099-01-01T00:00:00Z",
            "message": {
                "id": id,
                "model": "claude-opus-5-5",
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": write,
                    "cache_read_input_tokens": read,
                    "output_tokens": 77,
                },
            },
        })
        .to_string()
    }

    #[tokio::test]
    async fn agent_get_reports_the_context_as_of_the_last_turn_end_and_unknown_after_clear() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-app-agent-context-{}-{}",
            std::process::id(),
            crate::assignments::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("context")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        let target = app.public_pane_id(0, pane_id).unwrap();
        let mut seq = 10;
        let mut session_start = |app: &mut App, session: &str, source: &str| {
            seq += 1;
            app.handle_api_request(request(
                "pane.report_agent_session",
                serde_json::json!({
                    "pane_id": target,
                    "source": "herdr:claude",
                    "agent": "claude",
                    "seq": seq,
                    "agent_session_id": session,
                    "agent_session_path": dir.join(format!("{session}.jsonl")),
                    "session_start_source": source,
                    "turn_reports": true,
                }),
            ));
        };
        let turn_end = |app: &mut App| {
            app.handle_api_request(request(
                "pane.report_turn",
                serde_json::json!({
                    "pane_id": target,
                    "source": "herdr:claude",
                    "agent": "claude",
                    "phase": "finished",
                    "background_tasks": 0,
                }),
            ));
        };
        let agent = |app: &mut App| -> serde_json::Value {
            let response =
                app.handle_api_request(request("agent.get", serde_json::json!({"target": target})));
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            response["result"]["agent"].clone()
        };

        session_start(&mut app, "s1", "startup");
        assert_eq!(agent(&mut app)["context"], serde_json::Value::Null);

        let mut transcript = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("s1.jsonl"))
            .unwrap();
        writeln!(transcript, "{}", usage_line("m1", 3, 2_519, 418_000)).unwrap();
        // Mid-turn nothing is read: only the named events reconcile.
        assert_eq!(agent(&mut app)["context"], serde_json::Value::Null);
        turn_end(&mut app);
        let context = agent(&mut app)["context"].clone();
        assert_eq!(
            context,
            serde_json::json!({
                "last_request_input_tokens": 420_522,
                "session_id": "s1",
                "message_id": "m1",
                "at": "2099-01-01T00:00:00Z",
            })
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .agent_context
                .as_ref()
                .map(|context| context.last_request_input_tokens),
            Some(420_522)
        );

        // `/clear`: a new session, unknown until its first request.
        session_start(&mut app, "s2", "clear");
        assert_eq!(agent(&mut app)["context"], serde_json::Value::Null);
        turn_end(&mut app);
        assert_eq!(agent(&mut app)["context"], serde_json::Value::Null);
    }
}
