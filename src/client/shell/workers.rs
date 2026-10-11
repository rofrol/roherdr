//! Headless workers' lines under their space (see
//! [`super::space_tabs::worker_lines`]): a click opens the worker's tab
//! with its log, the right-click menu offers the log and a takeover.

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

    /// Opens the worker's tab, which shows its log and follows it
    /// ([`super::worker_view`]).
    pub(super) fn open_worker_log(&mut self, worker_id: String, outcome: &mut ClientShellInput) {
        self.open_worker_view(worker_id, outcome);
    }

    /// Asks a server without worker tabs for a popup that shows the
    /// worker's log and follows it; herdr opened it, so a click outside
    /// closes it.
    pub(super) fn open_worker_log_popup(
        &mut self,
        worker_id: String,
        outcome: &mut ClientShellInput,
    ) {
        self.popup_pending = true;
        self.popup_pending_deadline = None;
        self.popup_pending_dismissable = true;
        if !self.push_endpoint_method_with_kind(
            Method::WorkerOpenLog(WorkerTarget { worker_id }),
            PendingEndpointKind::PopupCommand,
            outcome,
        ) {
            self.popup_pending = false;
            self.popup_pending_dismissable = false;
        }
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

    /// Whether a takeover of the worker is offered, in its menu and its
    /// tab's header. Not while a question waits on the user: the interrupt
    /// would throw the answer away.
    pub(super) fn can_take_over_worker(&self, worker_id: &str) -> bool {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        let Some(worker) = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == worker_id)
        else {
            return false;
        };
        let asking = worker.state == "waiting_approval"
            || snapshot
                .worker_questions
                .iter()
                .any(|question| question.worker_id == worker_id);
        self.supports_endpoint_method(&Method::WorkerTakeOver(WorkerTarget {
            worker_id: String::new(),
        })) && worker.session_id.is_some()
            && !worker.takeover
            && !asking
            && worker.state != "lost"
    }

    /// The worker's menu at `(x, y)`; none when the worker is gone.
    pub(super) fn worker_context_menu(
        &self,
        worker_id: String,
        x: u16,
        y: u16,
    ) -> Option<ClientContextMenuOverlay> {
        let snapshot = self.snapshot.as_deref()?;
        if !snapshot
            .workers
            .iter()
            .any(|worker| worker.worker_id == worker_id)
        {
            return None;
        }
        let can_take_over = self.can_take_over_worker(&worker_id);
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

/// A minute and a day in milliseconds, the steps a worker's age moves in.
const MINUTE_MS: u64 = 60_000;
const DAY_MS: u64 = 24 * 60 * MINUTE_MS;

/// The wall clock in Unix milliseconds, which the server's worker times are
/// in.
pub(super) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

/// A duration as a worker's line shows it: `<1m`, `12m`, `1h05`, `2d03h`.
pub(super) fn age_text(elapsed_ms: u64) -> String {
    let minutes = elapsed_ms / MINUTE_MS;
    match minutes {
        0 => "<1m".to_owned(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h{:02}", minutes / 60, minutes % 60),
        _ => format!("{}d{:02}h", minutes / 1440, (minutes % 1440) / 60),
    }
}

/// When [`age_text`] of the time since `since_ms` next changes: at the next
/// whole minute of it, from a day on at the next whole hour.
pub(super) fn age_text_changes_at(since_ms: u64, now_ms: u64) -> u64 {
    let elapsed = now_ms.saturating_sub(since_ms);
    let step = if elapsed < DAY_MS {
        MINUTE_MS
    } else {
        60 * MINUTE_MS
    };
    since_ms.saturating_add((elapsed / step + 1).saturating_mul(step))
}

/// Whether the worker's process is gone: its times stop then.
fn worker_gone(worker: &crate::protocol::ClientShellWorker) -> bool {
    matches!(worker.state.as_str(), "exited" | "lost")
}

/// What a worker's line is named by: its TODO item's title, else its task
/// (the name given at start, else its prompt's first line), else its id.
pub(super) fn worker_title(worker: &crate::protocol::ClientShellWorker) -> String {
    worker
        .item_title
        .as_deref()
        .filter(|title| !title.is_empty())
        .or(Some(worker.name.as_str()).filter(|name| !name.is_empty()))
        .map_or_else(|| format!("worker {}", worker.worker_id), str::to_owned)
}

/// How long the worker has been at it at `now_ms`, waits included: from its
/// start to now, or to its last event once it is gone. None from a server
/// that does not send its start.
pub(super) fn worker_age_ms(
    worker: &crate::protocol::ClientShellWorker,
    now_ms: u64,
) -> Option<u64> {
    let started = worker.started_ms?;
    let end = if worker_gone(worker) {
        worker.last_event_ms.unwrap_or(now_ms)
    } else {
        now_ms
    };
    Some(end.saturating_sub(started))
}

/// What a worker's tooltip and its tab's header say beyond its title and
/// state: its item, its name, turns, the running turn's time, its last
/// activity, its age, branch and slot (the times first, so a narrow header
/// cuts the branch), each part left out when unknown.
pub(super) fn worker_details(
    worker: &crate::protocol::ClientShellWorker,
    now_ms: u64,
) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(item) = &worker.item {
        parts.push(item.clone());
    }
    if worker.item_title.is_some() && !worker.name.is_empty() {
        parts.push(format!("worker {} ({})", worker.worker_id, worker.name));
    } else {
        parts.push(format!("worker {}", worker.worker_id));
    }
    parts.push(match worker.turns {
        1 => "1 turn".to_owned(),
        turns => format!("{turns} turns"),
    });
    if let Some(turn) = worker.turn_started_ms.filter(|_| !worker_gone(worker)) {
        parts.push(format!(
            "this turn {}",
            age_text(now_ms.saturating_sub(turn))
        ));
    }
    if let Some(last) = worker.last_event_ms {
        parts.push(format!(
            "last activity {} ago",
            age_text(now_ms.saturating_sub(last))
        ));
    }
    if let Some(age) = worker_age_ms(worker, now_ms) {
        parts.push(format!("age {}", age_text(age)));
    }
    if let Some(branch) = &worker.branch {
        parts.push(format!("branch {branch}"));
    }
    if let Some(slot) = &worker.slot {
        parts.push(format!("slot {slot}"));
    }
    parts
}

/// When any time a worker's line, tooltip or tab shows next changes, so the
/// client redraws then and not on a timer of its own; none while no shown
/// time can change.
pub(super) fn worker_times_change_at<'a>(
    workers: impl IntoIterator<Item = &'a crate::protocol::ClientShellWorker>,
    now_ms: u64,
) -> Option<u64> {
    workers
        .into_iter()
        .flat_map(|worker| {
            let running = !worker_gone(worker);
            [
                worker.started_ms.filter(|_| running),
                worker.turn_started_ms.filter(|_| running),
                worker.last_event_ms,
            ]
        })
        .flatten()
        .map(|since| age_text_changes_at(since, now_ms))
        .min()
}

impl ClientShellState {
    /// Whether a worker time shown in the last frame has changed since:
    /// then the frame is drawn again. [`Self::compose`] sets the moment.
    pub(crate) fn tick_worker_times(&mut self) -> bool {
        self.tick_worker_times_at(unix_now_ms())
    }

    pub(super) fn tick_worker_times_at(&mut self, now_ms: u64) -> bool {
        if self
            .worker_times_change_at
            .is_some_and(|change_at| now_ms >= change_at)
        {
            self.worker_times_change_at = None;
            return true;
        }
        false
    }

    /// The moment [`Self::tick_worker_times`] waits for: the earliest
    /// change of a worker time any connection's snapshot shows.
    pub(super) fn note_worker_times(&mut self, now_ms: u64) {
        self.worker_times_change_at = worker_times_change_at(
            self.snapshot
                .iter()
                .chain(
                    self.endpoints
                        .iter()
                        .filter_map(|endpoint| endpoint.snapshot.as_ref()),
                )
                .flat_map(|snapshot| snapshot.workers.iter()),
            now_ms,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = MINUTE_MS;
    const HOUR: u64 = 60 * MINUTE_MS;

    #[test]
    fn an_age_reads_in_minutes_then_hours_then_days() {
        assert_eq!(age_text(0), "<1m");
        assert_eq!(age_text(MINUTE - 1), "<1m");
        assert_eq!(age_text(MINUTE), "1m");
        assert_eq!(age_text(12 * MINUTE + 30_000), "12m");
        assert_eq!(age_text(HOUR - 1), "59m");
        assert_eq!(age_text(HOUR + 5 * MINUTE), "1h05");
        assert_eq!(age_text(23 * HOUR + 59 * MINUTE), "23h59");
        assert_eq!(age_text(2 * DAY_MS + 3 * HOUR + 59 * MINUTE), "2d03h");
    }

    #[test]
    fn an_age_changes_at_the_next_whole_minute_and_from_a_day_on_the_next_hour() {
        assert_eq!(age_text_changes_at(1_000, 1_000), 1_000 + MINUTE);
        assert_eq!(
            age_text_changes_at(1_000, 1_000 + 90_000),
            1_000 + 2 * MINUTE
        );
        assert_eq!(age_text_changes_at(0, DAY_MS + 10 * MINUTE), DAY_MS + HOUR);
        // A start the client's clock has not reached yet.
        assert_eq!(age_text_changes_at(5_000, 1_000), 5_000 + MINUTE);
    }

    fn worker(state: &str) -> crate::protocol::ClientShellWorker {
        crate::protocol::ClientShellWorker {
            worker_id: "w7".into(),
            name: "fix login".into(),
            state: state.into(),
            item: Some("t-abcd2345".into()),
            item_title: Some("A worker's line shows its age".into()),
            slot: Some("worker".into()),
            branch: Some("todo/t-abcd2345-1".into()),
            turns: 2,
            started_ms: Some(0),
            turn_started_ms: Some(10 * MINUTE),
            last_event_ms: Some(14 * MINUTE),
            ..Default::default()
        }
    }

    #[test]
    fn a_worker_is_named_by_its_items_title_then_its_task_then_its_id() {
        let mut named = worker("working");
        assert_eq!(worker_title(&named), "A worker's line shows its age");
        named.item_title = None;
        assert_eq!(worker_title(&named), "fix login");
        named.name.clear();
        assert_eq!(worker_title(&named), "worker w7");
    }

    #[test]
    fn the_details_name_the_item_worker_branch_slot_turns_and_times() {
        let now = 15 * MINUTE + 30_000;
        assert_eq!(
            worker_details(&worker("working"), now).join(" · "),
            "t-abcd2345 · worker w7 (fix login) · 2 turns · this turn 5m \
             · last activity 1m ago · age 15m · branch todo/t-abcd2345-1 · slot worker"
        );
        // A gone worker's age stops at its last event, and it has no turn.
        assert_eq!(worker_age_ms(&worker("exited"), now), Some(14 * MINUTE));
        assert!(!worker_details(&worker("exited"), now)
            .iter()
            .any(|part| part.starts_with("this turn")));
        // From a server that sends none of it: only the worker and its turns.
        let bare = crate::protocol::ClientShellWorker {
            worker_id: "w8".into(),
            state: "working".into(),
            ..Default::default()
        };
        assert_eq!(worker_details(&bare, now), ["worker w8", "0 turns"]);
        assert_eq!(worker_age_ms(&bare, now), None);
    }

    #[test]
    fn the_next_redraw_is_the_first_shown_time_to_change() {
        let now = 15 * MINUTE + 30_000;
        let working = worker("working");
        // Every shown time (started 0, turn 10m, last event 14m) next
        // changes at 16m.
        assert_eq!(worker_times_change_at([&working], now), Some(16 * MINUTE));
        // A gone worker's age and turn are frozen; its last activity still
        // moves.
        let exited = worker("exited");
        assert_eq!(worker_times_change_at([&exited], now), Some(16 * MINUTE));
        let bare = crate::protocol::ClientShellWorker::default();
        assert_eq!(worker_times_change_at([&bare], now), None);
        assert_eq!(worker_times_change_at([], now), None);
    }
}
