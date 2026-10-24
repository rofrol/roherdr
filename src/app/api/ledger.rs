//! Each pane's agent session booked into the persistent usage ledger (`crate::usage::ledger`)
//! at the named events that produce usage and call these: server start and live handoff, the
//! agent's turn end, its change to idle, a session report (the previous session's last
//! responses, then the reported one's), the agent's release and the pane's exit. Never on a
//! timer, never in render.

use std::path::PathBuf;

use crate::agent_resume::AgentSessionRefKind;
use crate::api::schema::{ResponseResult, UsageLedgerParams};
use crate::app::App;
use crate::usage::estimate::{self, SessionKey, TranscriptRoots};

use super::responses::encode_success;

impl App {
    /// Server start or live handoff: every stored transcript that grew while no server read
    /// it, then every pane's agent session, goes on from where its cursor stopped.
    pub(crate) fn book_agent_usage_at_start(&mut self) {
        self.usage_ledger.catch_up(crate::assignments::now_ms());
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
            self.book_agent_usage(ws_idx, pane_id);
        }
    }

    /// Books what the pane's agent session appended to its transcripts under the pane's
    /// space.
    pub(crate) fn book_agent_usage(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(workspace) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let Some(terminal) = workspace
            .pane_state(pane_id)
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
        else {
            return;
        };
        let Some(session) = crate::app::creation::terminal_agent_session_info(terminal) else {
            return;
        };
        if crate::usage::transcript::reader_for(&session.agent).is_none() {
            return;
        }
        let reported = terminal
            .prompt_turns
            .transcript()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let workspace_id = workspace.id.clone();
        let key = match session.kind {
            AgentSessionRefKind::Id => SessionKey::Id(session.value.clone()),
            AgentSessionRefKind::Path => SessionKey::Path(session.value.clone()),
        };
        let stored = self
            .usage_ledger
            .stored_transcripts(&session.agent, &session.value);
        let transcripts = session_transcripts(&session.agent, &key, reported, stored);
        self.usage_ledger.book_session(
            &session.agent,
            &session.value,
            Some(&workspace_id),
            &transcripts,
            crate::assignments::now_ms(),
        );
    }

    pub(super) fn handle_usage_ledger(&self, id: String, params: UsageLedgerParams) -> String {
        let labels = self
            .state
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.id.clone(),
                    workspace.display_name_from_terminals(&self.state.terminals),
                )
            })
            .collect::<Vec<_>>();
        encode_success(
            id,
            ResponseResult::UsageLedger {
                ledger: self.usage_ledger.report(
                    params.workspace_id.as_deref(),
                    &labels,
                    crate::assignments::now_ms(),
                ),
            },
        )
    }
}

/// A session's transcripts, main one first: the one its agent reported, else the one booked
/// before, else the one found where the agent writes them (searched only then); with a
/// Claude session's subagents' transcripts and every one booked before.
fn session_transcripts(
    agent: &str,
    key: &SessionKey,
    reported: Option<PathBuf>,
    stored: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let main = match key {
        SessionKey::Path(path) => Some(PathBuf::from(path)),
        SessionKey::Id(_) => reported
            .or_else(|| stored.first().cloned())
            .or_else(|| estimate::session_transcript(agent, key, &TranscriptRoots::from_env())),
    };
    let mut transcripts: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !transcripts.contains(&path) {
            transcripts.push(path);
        }
    };
    if let Some(main) = main {
        let subagents = if agent == "claude" {
            estimate::claude_subagents(&main.with_extension(""))
        } else {
            Vec::new()
        };
        push(main);
        subagents.into_iter().for_each(&mut push);
    }
    stored.into_iter().for_each(push);
    transcripts
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

    fn usage_line(id: &str, timestamp: &str, input: u64, output: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": timestamp,
            "requestId": format!("req-{id}"),
            "message": {
                "id": id,
                "model": "claude-opus-5-5",
                "usage": {"input_tokens": input, "output_tokens": output},
            },
        })
        .to_string()
    }

    #[tokio::test]
    async fn a_pane_session_is_booked_at_turn_end_and_reported_per_space() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-app-usage-ledger-{}-{}",
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
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("ledger")];
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
        let space = app.public_workspace_id(0);
        let transcript = dir.join("s1.jsonl");
        app.handle_api_request(request(
            "pane.report_agent_session",
            serde_json::json!({
                "pane_id": target,
                "source": "herdr:claude",
                "agent": "claude",
                "seq": 1,
                "agent_session_id": "s1",
                "agent_session_path": transcript,
                "session_start_source": "startup",
                "turn_reports": true,
            }),
        ));
        let ledger = |app: &mut App, params: serde_json::Value| -> serde_json::Value {
            let response = app.handle_api_request(request("usage.ledger", params));
            let response: serde_json::Value = serde_json::from_str(&response).unwrap();
            response["result"]["ledger"].clone()
        };
        assert_eq!(
            ledger(&mut app, serde_json::json!({}))["spaces"],
            serde_json::json!([])
        );

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file, "{}", usage_line("m1", "2026-10-01T10:00:00Z", 10, 20)).unwrap();
        // Mid-turn nothing is read: only the named events book.
        assert_eq!(
            ledger(&mut app, serde_json::json!({}))["spaces"],
            serde_json::json!([])
        );
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
        turn_end(&mut app);
        turn_end(&mut app);
        let report = ledger(&mut app, serde_json::json!({}));
        assert!(report["coverage_start"].as_u64().is_some());
        let spaces = report["spaces"].as_array().unwrap();
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0]["workspace_id"], space.as_str());
        assert_eq!(spaces[0]["label"], "ledger");
        assert_eq!(spaces[0]["tokens"]["input"], 10);
        assert_eq!(spaces[0]["tokens"]["output"], 20);
        assert_eq!(
            spaces[0]["days"][0]["models"][0]["model"],
            "claude-opus-5-5"
        );
        // Another space's filter leaves this one out.
        let other = ledger(&mut app, serde_json::json!({"workspace_id": "w-other"}));
        assert_eq!(other["spaces"], serde_json::json!([]));

        // The pane's exit books what came after the last turn end, and the rows stay.
        writeln!(file, "{}", usage_line("m2", "2026-10-01T10:05:00Z", 1, 2)).unwrap();
        app.handle_internal_event(crate::events::AppEvent::PaneDied {
            pane_id,
            exit_reason: crate::platform::ChildExitReason::Exited,
        });
        let report = ledger(&mut app, serde_json::json!({}));
        assert_eq!(report["spaces"][0]["tokens"]["input"], 11);
        std::fs::remove_dir_all(dir).ok();
    }
}
