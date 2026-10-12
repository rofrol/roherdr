//! Starts the supervision sweeps (`crate::workers::supervision`): one every
//! [`SWEEP_INTERVAL`], the first at the server's start, and one after an
//! event of a coordinator's pane asked for it. The sweep only reports; the
//! loop never waits for it. Also types the findings' queued notices into
//! their coordinators' panes when an event (a sweep's end, a coordinator's
//! turn end, a pane losing a client's focus) asked for it, never into a
//! pane a client has focused, and records the prompts' replies as they come.

use std::collections::HashSet;
use std::time::Instant;

use crate::workers::supervision::{
    poll_supervision_prompts, start_sweep, take_delivery_request, take_sweep_request,
    SWEEP_INTERVAL,
};

use super::HeadlessServer;

impl HeadlessServer {
    pub(super) fn start_supervision_sweep_if_due(&mut self, now: Instant) {
        let trigger = if self.supervision_due.is_some_and(|due| now >= due) {
            self.supervision_due = Some(now + SWEEP_INTERVAL);
            "timer".to_owned()
        } else {
            match take_sweep_request() {
                Some(trigger) => trigger,
                None => return,
            }
        };
        // A sweep still running covers the timer; an event's request stays
        // queued until it ends (`take_sweep_request`).
        start_sweep(trigger, self.app.supervision_pane_facts());
    }

    pub(super) fn deliver_supervision_notices_if_requested(&mut self) {
        if take_delivery_request() {
            let focused = self.supervision_focused_panes();
            let typed = self.app.deliver_supervision_notices(&focused);
            self.supervision_prompts.extend(typed);
        }
        poll_supervision_prompts(&mut self.supervision_prompts);
    }

    /// The panes (public ids) a client has focused, where the user may be
    /// typing: each shell client's focused pane, whether or not its terminal
    /// window has the system's focus, and each terminal a client attached to
    /// directly.
    fn supervision_focused_panes(&self) -> HashSet<String> {
        let mut focused: HashSet<String> = self
            .shell_focus_targets()
            .into_iter()
            .filter_map(|(_, target)| target)
            .filter_map(|target| {
                self.app
                    .public_pane_id(target.workspace_index, target.pane_id)
            })
            .collect();
        if !self.terminal_attach_owners.is_empty() {
            for (ws_idx, workspace) in self.app.state.workspaces.iter().enumerate() {
                for tab in &workspace.tabs {
                    for (pane_id, pane) in &tab.panes {
                        let attached = self
                            .terminal_attach_owners
                            .contains_key(&pane.attached_terminal_id.to_string());
                        if attached {
                            focused.extend(self.app.public_pane_id(ws_idx, *pane_id));
                        }
                    }
                }
            }
        }
        focused
    }
}
