//! Starts the supervision sweeps (`crate::workers::supervision`): one every
//! [`SWEEP_INTERVAL`], the first at the server's start, and one after an
//! event of a coordinator's pane asked for it. The sweep only reports; the
//! loop never waits for it.

use std::time::Instant;

use crate::workers::supervision::{start_sweep, take_sweep_request, SWEEP_INTERVAL};

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
}
