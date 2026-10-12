//! The pane facts a supervision sweep starts from
//! (`crate::workers::supervision`): read from the app's state on the
//! server's loop, at most once per sweep, then handed to the sweep's thread,
//! which reads the worker store and the repositories itself. Also
//! `supervise.report` and `supervise.acknowledge`, which run on the app
//! because they name the caller's pane by its canonical public id.

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    ResponseResult, SuperviseAckParams, SuperviseAcknowledgeParams, SuperviseReportParams,
};
use crate::app::App;
use crate::workers::supervision::PaneFacts;
use crate::workers::WorkerError;

use super::super::api_helpers::pane_agent_status;
use crate::api::schema::agents::{background_task_count, BACKGROUND_WAITS_TOKEN};

impl App {
    /// Every agent pane's facts, by its canonical public id.
    pub(crate) fn supervision_pane_facts(&self) -> Vec<PaneFacts> {
        let mut panes = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    if !terminal.is_agent_terminal() {
                        continue;
                    }
                    let Some(public) = self.public_pane_id(ws_idx, *pane_id) else {
                        continue;
                    };
                    panes.push(PaneFacts {
                        pane_id: public,
                        agent: terminal.effective_agent_label().map(str::to_owned),
                        status: pane_agent_status(terminal.state, pane.seen),
                        state_seq: terminal.last_agent_state_change_seq,
                        turn_closed: terminal.turn_closed(),
                        background_tasks: terminal.turn_background_tasks(),
                        wait_edges: terminal
                            .metadata_tokens
                            .get(BACKGROUND_WAITS_TOKEN)
                            .and_then(background_task_count),
                        asks_user: !terminal.open_questions().is_empty(),
                        limited: terminal.limit().is_some(),
                        running_jobs: Vec::new(),
                    });
                }
            }
        }
        // The herdr-job jobs whose tabs run, by the pane that owns them.
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                let Some(job) = &tab.job else {
                    continue;
                };
                if tab.status != Some(crate::api::schema::TabStatus::Running) {
                    continue;
                }
                let Some(owner) = job.owner_pane.as_deref() else {
                    continue;
                };
                let owner = self.canonical_pane_id(owner);
                if let Some(pane) = panes.iter_mut().find(|pane| pane.pane_id == owner) {
                    pane.running_jobs.push(job.id.clone());
                }
            }
        }
        panes
    }
}

impl App {
    /// `supervise.report`, with the reader's pane by its canonical public id:
    /// a read by the herdr repository's coordinator marks what it read seen.
    pub(super) fn handle_supervise_report(
        &mut self,
        id: String,
        params: SuperviseReportParams,
    ) -> String {
        let Some(supervisor) = crate::workers::coordinators() else {
            let error = unavailable();
            return encode_error(id, error.code(), error.to_string());
        };
        let pane = params
            .pane_id
            .as_deref()
            .map(|pane| self.canonical_pane_id(pane));
        match supervisor.supervision_report(params.all, pane.as_deref()) {
            Ok(report) => encode_success(id, ResponseResult::SupervisionReport { report }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// `supervise.acknowledge`: only from the pane that holds the herdr
    /// repository's coordination tenure, named by its canonical public id.
    pub(super) fn handle_supervise_acknowledge(
        &mut self,
        id: String,
        params: SuperviseAcknowledgeParams,
    ) -> String {
        let Some(supervisor) = crate::workers::coordinators() else {
            let error = unavailable();
            return encode_error(id, error.code(), error.to_string());
        };
        let pane = params
            .pane_id
            .as_deref()
            .map(|pane| self.canonical_pane_id(pane));
        match supervisor.supervision_acknowledge_from_pane(&params.finding_id, pane.as_deref()) {
            Ok(finding) => encode_success(id, ResponseResult::SupervisionFinding { finding }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// `supervise.ack`: the recipient coordinator's pane, named by its
    /// canonical public id, acknowledges the notices of a finding addressed
    /// to its tenure.
    pub(super) fn handle_supervise_ack(
        &mut self,
        id: String,
        params: SuperviseAckParams,
    ) -> String {
        let Some(supervisor) = crate::workers::coordinators() else {
            let error = unavailable();
            return encode_error(id, error.code(), error.to_string());
        };
        let pane = params
            .pane_id
            .as_deref()
            .map(|pane| self.canonical_pane_id(pane));
        match supervisor.supervision_ack_from_pane(&params.finding_id, pane.as_deref()) {
            Ok(finding) => encode_success(id, ResponseResult::SupervisionFinding { finding }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// Types the supervision notices whose recipient's agent rests (a turn
    /// boundary) into its pane, one batch per recipient. Runs on the server's
    /// loop when an event asked for it
    /// ([`crate::workers::supervision::take_delivery_request`]); a recipient
    /// that works keeps its notices queued until its next event.
    /// Returns the replies of the prompts typed, which the loop reads as they
    /// come ([`crate::workers::supervision::SupervisionPrompt`]).
    pub(crate) fn deliver_supervision_notices(
        &mut self,
    ) -> Vec<crate::workers::supervision::SupervisionPrompt> {
        let Some(supervisor) = crate::workers::coordinators() else {
            return Vec::new();
        };
        let batches = match supervisor.pending_delivery_batches() {
            Ok(batches) => batches,
            Err(error) => {
                tracing::warn!(%error, "cannot read the queued supervision notices");
                return Vec::new();
            }
        };
        if batches.is_empty() {
            return Vec::new();
        }
        let panes = self.supervision_pane_facts();
        let mut prompts = Vec::new();
        for batch in batches {
            let pane = self.canonical_pane_id(&batch.pane_id);
            let ready = panes
                .iter()
                .find(|facts| facts.pane_id == pane)
                .is_some_and(crate::workers::supervision::at_turn_boundary);
            if !ready {
                continue;
            }
            let text = crate::workers::supervision::notice_text(&batch);
            match self.submit_supervision_notice(&pane, text) {
                Ok(reply) => {
                    supervisor.record_dispatched(&batch.delivery_ids(), &pane);
                    prompts.push(crate::workers::supervision::SupervisionPrompt {
                        delivery_ids: batch.delivery_ids(),
                        reply,
                    });
                }
                Err(refusal) => {
                    supervisor.record_dispatch_refused(&batch.delivery_ids(), &refusal);
                }
            }
        }
        prompts
    }
}

fn unavailable() -> WorkerError {
    WorkerError::Io(std::io::Error::other(
        "supervision findings are unavailable on this server",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{AgentStatus, SupervisionStatus};
    use crate::config::Config;
    use crate::workers::WorkerSupervisor;
    use crate::workspace::Workspace;

    /// An app with one tab whose pane runs in this checkout (herdr's own
    /// repository), and a worker store of its own.
    fn app(name: &str) -> (App, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "herdr-app-supervision-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        crate::workers::set_test_coordinators(WorkerSupervisor::open(
            root.join("workers"),
            "claude".into(),
        ));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = Workspace::test_new("supervision");
        let checkout = std::env::current_dir().unwrap();
        for tab in &workspace.tabs {
            for pane in tab.panes.values() {
                let terminal_id = pane.attached_terminal_id.clone();
                app.state.terminals.insert(
                    terminal_id.clone(),
                    crate::terminal::TerminalState::new(terminal_id, checkout.clone()),
                );
            }
        }
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        (app, root)
    }

    #[test]
    fn the_herdr_coordinators_pane_reads_and_acknowledges_findings() {
        let (mut app, root) = app("ack");
        let pane = app.tab_public_pane_ids(0, 0).remove(0);
        let supervisor = crate::workers::coordinators().unwrap();
        let mut stuck = PaneFacts {
            pane_id: "w9:p9".into(),
            agent: Some("claude".into()),
            status: AgentStatus::Working,
            state_seq: None,
            turn_closed: true,
            background_tasks: Some(0),
            wait_edges: Some(0),
            asks_user: false,
            limited: false,
            running_jobs: Vec::new(),
        };
        supervisor
            .supervision_sweep("timer", vec![stuck.clone()])
            .unwrap();
        let finding = supervisor
            .supervision_report(false, None)
            .unwrap()
            .findings
            .into_iter()
            .find(|finding| finding.pane_id.as_deref() == Some("w9:p9"))
            .unwrap();
        let acknowledge = |app: &mut App| {
            let response = app.handle_supervise_acknowledge(
                "req".into(),
                SuperviseAcknowledgeParams {
                    finding_id: finding.finding_id.clone(),
                    pane_id: Some(pane.clone()),
                },
            );
            serde_json::from_str::<serde_json::Value>(&response).unwrap()
        };
        // Before the pane coordinates herdr: refused.
        let refused = acknowledge(&mut app);
        assert_eq!(refused["error"]["code"], "finding_ack_refused", "{refused}");

        let repo = crate::workers::repository_of_dir(
            &std::env::current_dir().unwrap().display().to_string(),
        )
        .unwrap();
        let tenure = supervisor.coordinator_start(&repo, &pane, None).unwrap();
        // Its read marks the finding seen by its tenure.
        let response = app.handle_supervise_report(
            "req".into(),
            SuperviseReportParams {
                all: false,
                pane_id: Some(pane.clone()),
            },
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["type"], "supervision_report", "{response}");
        assert_eq!(
            value["result"]["report"]["herdr_coordinator_id"],
            tenure.coordinator_id.as_str()
        );
        let listed = supervisor.supervision_report(false, None).unwrap();
        let shown = listed
            .findings
            .iter()
            .find(|shown| shown.finding_id == finding.finding_id)
            .unwrap();
        assert_eq!(
            shown.coordinator_seen_by.as_deref(),
            Some(tenure.coordinator_id.as_str())
        );

        let acknowledged = acknowledge(&mut app);
        assert_eq!(
            acknowledged["result"]["type"], "supervision_finding",
            "{acknowledged}"
        );
        assert_eq!(acknowledged["result"]["finding"]["status"], "acknowledged");
        let holding = supervisor.supervision_report(false, None).unwrap();
        assert!(holding.findings.iter().any(|shown| {
            shown.finding_id == finding.finding_id
                && shown.status == SupervisionStatus::Acknowledged
        }));
        stuck.status = AgentStatus::Idle;
        supervisor.supervision_sweep("timer", vec![stuck]).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
