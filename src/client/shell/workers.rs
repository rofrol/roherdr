//! Headless workers' lines under their space (see
//! [`super::space_tabs::worker_lines`]): a click opens the worker's log, the
//! right-click menu offers the log and a takeover.

use super::*;
use crate::api::schema::{Method, WorkerTarget};

impl ClientShellState {
    /// The worker whose line is at `point`.
    pub(super) fn worker_line_at(&self, point: (u16, u16)) -> Option<String> {
        if self.sidebar_collapsed {
            return None;
        }
        self.hits
            .space_tabs
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .and_then(|(_, id)| super::space_tabs::worker_line_worker(id))
            .map(str::to_owned)
    }

    /// Asks the server for a popup that shows the worker's log and follows
    /// it.
    pub(super) fn open_worker_log(&mut self, worker_id: String, outcome: &mut ClientShellInput) {
        self.push_endpoint_method(Method::WorkerOpenLog(WorkerTarget { worker_id }), outcome);
        outcome.repaint = true;
    }

    pub(super) fn take_over_worker(&mut self, worker_id: String, outcome: &mut ClientShellInput) {
        self.push_endpoint_method(Method::WorkerTakeOver(WorkerTarget { worker_id }), outcome);
        outcome.repaint = true;
    }

    pub(super) fn open_worker_context_menu(&mut self, worker_id: String, x: u16, y: u16) {
        if let Some(menu) = self.worker_context_menu(worker_id, x, y) {
            self.overlay = Some(ClientShellOverlay::ContextMenu(menu));
        }
    }

    /// The worker's menu at `(x, y)`; none when the worker is gone. A
    /// takeover is not offered while a question waits on the user: the
    /// interrupt would throw the answer away.
    pub(super) fn worker_context_menu(
        &self,
        worker_id: String,
        x: u16,
        y: u16,
    ) -> Option<ClientContextMenuOverlay> {
        let snapshot = self.snapshot.as_deref()?;
        let worker = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == worker_id)?;
        let asking = worker.state == "waiting_approval"
            || snapshot
                .worker_questions
                .iter()
                .any(|question| question.worker_id == worker_id);
        let can_take_over = self.supports_endpoint_method(&Method::WorkerTakeOver(WorkerTarget {
            worker_id: String::new(),
        })) && worker.session_id.is_some()
            && !worker.takeover
            && !asking
            && worker.state != "lost";
        Some(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Worker {
                worker_id,
                can_take_over,
            },
            x,
            y,
            highlighted: 0,
        })
    }
}
