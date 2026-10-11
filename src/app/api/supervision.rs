//! The pane facts a supervision sweep starts from
//! (`crate::workers::supervision`): read from the app's state on the
//! server's loop, at most once per sweep, then handed to the sweep's thread,
//! which reads the worker store and the repositories itself.

use crate::app::App;
use crate::workers::supervision::PaneFacts;

use super::super::api_helpers::pane_agent_status;

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
