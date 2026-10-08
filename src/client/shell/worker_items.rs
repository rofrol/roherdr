//! The coordinator's Items button: on the line of a tab whose agent
//! coordinates (`tab.set_role coordinator`), it counts the TODO items with
//! workers on them (`3·1!`: items with a running worker, items with an
//! ended run its owner has not acknowledged), from the snapshot. A click
//! opens a dropdown, drawn as the header lists are, of the items with runs,
//! those in progress first, then the most recently ended, each by its
//! title and id, and an "Unassigned" entry for runs without an item. An
//! item opens its runs (worker, start and end, outcome, turns, commits,
//! questions); a run opens its log.
//!
//! The runs are fetched (`worker.runs`) when the dropdown opens, never in
//! the background: a background request would hold the machine's command
//! lane, and a click in that moment would be refused as busy. Drawing reads
//! only what that reply brought.

use crate::api::schema::{WorkerItemRuns, WorkerRun, WorkerRunOutcome, WorkerRunsParams};

use super::*;

/// Items the list shows at most, the most recent kept.
const MAX_ITEMS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerItemsOverlay {
    /// The button it opened from, which it hangs under.
    pub(super) button: Rect,
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) fetch: ItemsFetch,
    /// The item whose runs it shows; none for the list of items.
    pub(super) open: Option<ItemKey>,
    pub(super) highlighted: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ItemsFetch {
    Loading,
    Failed(String),
    Loaded {
        items: Vec<WorkerItemRuns>,
        unassigned: Vec<WorkerRun>,
    },
}

/// An entry of the list: an item of a repository, or the runs without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ItemKey {
    Item { item: String, repo: Option<String> },
    Unassigned,
}

/// What a row does when chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ItemsRowAction {
    /// Nothing: a message such as "loading".
    None,
    Open(ItemKey),
    Back,
    Log(String),
}

/// A row as the dropdown draws it ([`super::render::LogRow`]) and what it does.
pub(super) struct ItemsRow {
    pub(super) row: super::render::LogRow,
    pub(super) action: ItemsRowAction,
}

/// The Items button's text: `Items` and its badge.
pub(super) fn items_button_text(badge: &str) -> String {
    if badge.is_empty() {
        "Items".to_owned()
    } else {
        format!("Items {badge}")
    }
}

/// The badge of the counts: `3·1!`, `3`, `1!`, or empty when both are zero.
pub(super) fn items_badge(counts: &crate::protocol::ClientShellWorkerItemCounts) -> String {
    let mut parts = Vec::new();
    if counts.in_progress > 0 {
        parts.push(counts.in_progress.to_string());
    }
    if counts.attention > 0 {
        parts.push(format!("{}!", counts.attention));
    }
    parts.join("·")
}

/// What the button says on hover.
pub(super) fn items_tooltip(counts: &crate::protocol::ClientShellWorkerItemCounts) -> String {
    format!(
        "TODO items with workers: {} in progress · {} ended, not acknowledged by the coordinator",
        counts.in_progress, counts.attention
    )
}

/// How a run's outcome reads, and the agent state whose glyph it takes.
fn outcome(outcome: WorkerRunOutcome) -> (&'static str, crate::api::schema::AgentStatus) {
    use crate::api::schema::AgentStatus;
    match outcome {
        WorkerRunOutcome::Running => ("running", AgentStatus::Working),
        WorkerRunOutcome::Finished => ("finished", AgentStatus::Done),
        WorkerRunOutcome::Failed => ("failed", AgentStatus::Blocked),
        WorkerRunOutcome::Exited => ("exited", AgentStatus::Idle),
        WorkerRunOutcome::Lost => ("lost", AgentStatus::Idle),
        WorkerRunOutcome::Degraded => ("record incomplete", AgentStatus::Blocked),
        WorkerRunOutcome::Unknown => ("unknown", AgentStatus::Idle),
    }
}

fn is_running(run: &WorkerRun) -> bool {
    run.outcome == WorkerRunOutcome::Running
}

/// When the run last changed: its end, else its start.
fn last_ms(run: &WorkerRun) -> u64 {
    run.ended_ms.or(run.started_ms).unwrap_or(0)
}

/// The items with runs, those with a running worker first, each group the
/// most recent first; at most [`MAX_ITEMS`].
fn sorted_items(items: &[WorkerItemRuns]) -> Vec<&WorkerItemRuns> {
    let mut sorted = items.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|group| {
        let running = group.runs.iter().any(is_running);
        let last = group.runs.iter().map(last_ms).max().unwrap_or(0);
        (!running, std::cmp::Reverse(last))
    });
    sorted.truncate(MAX_ITEMS);
    sorted
}

/// `HH:MM`, or the day too when it was not today.
fn clock(unix_ms: u64, now_unix: u64, offset: i64) -> String {
    let day = super::notification_log::notification_day(unix_ms, now_unix, offset);
    let time = super::notification_log::notification_time(unix_ms, offset);
    if day == "Today" {
        time
    } else {
        format!("{day} {time}")
    }
}

impl WorkerItemsOverlay {
    /// The dropdown's rows: the items, or the open item's runs, newest
    /// first. Pure: `now_unix` and `offset` give the clock.
    pub(super) fn rows(
        &self,
        icon: impl Fn(crate::api::schema::AgentStatus) -> (&'static str, ratatui::style::Color),
        now_unix: u64,
        offset: i64,
    ) -> Vec<ItemsRow> {
        let message = |text: &str| ItemsRow {
            row: (
                None,
                String::new(),
                text.to_owned(),
                false,
                None,
                None,
                true,
            ),
            action: ItemsRowAction::None,
        };
        let (items, unassigned) = match &self.fetch {
            ItemsFetch::Loading => return vec![message("loading…")],
            ItemsFetch::Failed(error) => {
                return vec![message(&format!("could not list the runs: {error}"))]
            }
            ItemsFetch::Loaded { items, unassigned } => (items, unassigned),
        };
        let Some(open) = &self.open else {
            let mut rows = sorted_items(items)
                .into_iter()
                .map(|group| {
                    let latest = group.runs.iter().max_by_key(|run| last_ms(run));
                    let (said, status) = latest
                        .map_or(("", crate::api::schema::AgentStatus::Idle), |run| {
                            outcome(run.outcome)
                        });
                    let title = group.title.as_deref().unwrap_or("(not in TODO.md)");
                    let runs = match group.runs.len() {
                        1 => "1 run".to_owned(),
                        count => format!("{count} runs"),
                    };
                    ItemsRow {
                        row: (
                            None,
                            latest.map_or_else(String::new, |run| {
                                super::notification_log::wait_duration(last_ms(run), now_unix)
                            }),
                            format!("{title} · {}", group.item),
                            false,
                            Some(icon(status)),
                            Some(format!("{runs} · last {said}")),
                            false,
                        ),
                        action: ItemsRowAction::Open(ItemKey::Item {
                            item: group.item.clone(),
                            repo: group.repo.clone(),
                        }),
                    }
                })
                .collect::<Vec<_>>();
            if !unassigned.is_empty() {
                rows.push(ItemsRow {
                    row: (
                        None,
                        String::new(),
                        "Unassigned".to_owned(),
                        false,
                        None,
                        Some(format!(
                            "{} without an item · {} running",
                            match unassigned.len() {
                                1 => "1 run".to_owned(),
                                count => format!("{count} runs"),
                            },
                            unassigned.iter().filter(|run| is_running(run)).count()
                        )),
                        false,
                    ),
                    action: ItemsRowAction::Open(ItemKey::Unassigned),
                });
            }
            if rows.is_empty() {
                rows.push(message("no worker runs yet"));
            }
            return rows;
        };
        let (heading, runs) = match open {
            ItemKey::Unassigned => ("Unassigned".to_owned(), unassigned.as_slice()),
            ItemKey::Item { item, repo } => {
                match items
                    .iter()
                    .find(|group| group.item == *item && group.repo == *repo)
                {
                    Some(group) => (
                        format!(
                            "{} · {item}",
                            group.title.as_deref().unwrap_or("(not in TODO.md)")
                        ),
                        group.runs.as_slice(),
                    ),
                    None => (item.clone(), &[][..]),
                }
            }
        };
        let mut rows = vec![ItemsRow {
            row: (
                None,
                String::new(),
                format!("‹ {heading}"),
                false,
                None,
                None,
                false,
            ),
            action: ItemsRowAction::Back,
        }];
        let mut runs = runs.iter().collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_ms.unwrap_or(0)));
        rows.extend(runs.into_iter().map(|run| {
            let (said, status) = outcome(run.outcome);
            let start = run
                .started_ms
                .map_or_else(|| "?".to_owned(), |ms| clock(ms, now_unix, offset));
            let end = match run.ended_ms {
                Some(ms) => clock(ms, now_unix, offset),
                None => "now".to_owned(),
            };
            let mut text = format!("{} · {said}", run.worker_id);
            if matches!(open, ItemKey::Unassigned) && !run.name.is_empty() {
                text = format!("{text} · {}", run.name);
            }
            let mut detail = format!(
                "{start}–{end} · {} turn{} · {} question{}",
                run.turns,
                if run.turns == 1 { "" } else { "s" },
                run.questions,
                if run.questions == 1 { "" } else { "s" },
            );
            if !run.commits.is_empty() {
                let commits = run
                    .commits
                    .iter()
                    .map(|sha| sha.chars().take(8).collect::<String>())
                    .collect::<Vec<_>>();
                detail = format!("{detail} · {}", commits.join(" "));
            }
            ItemsRow {
                row: (
                    None,
                    String::new(),
                    text,
                    false,
                    Some(icon(status)),
                    Some(detail),
                    false,
                ),
                action: ItemsRowAction::Log(run.worker_id.clone()),
            }
        }));
        rows
    }
}

impl ClientShellState {
    /// The Items button at `point`: the coordinator tab's id and the
    /// button's rect.
    pub(super) fn worker_items_button_at(&self, point: (u16, u16)) -> Option<Rect> {
        if self.sidebar_collapsed {
            return None;
        }
        self.hits
            .space_tab_items
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(rect, _)| *rect)
    }

    /// Opens the dropdown under `button` and fetches the runs, or closes it.
    pub(super) fn toggle_worker_items(&mut self, button: Rect, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        if matches!(self.overlay, Some(ClientShellOverlay::WorkerItems(_))) {
            self.overlay = None;
            return;
        }
        let endpoint_id = self.active_endpoint_id.clone();
        let method = crate::api::schema::Method::WorkerRuns(WorkerRunsParams::default());
        let fetch =
            if self.endpoint_is_online(&endpoint_id) && self.supports_endpoint_method(&method) {
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::WorkerRuns {
                        endpoint_id: endpoint_id.clone(),
                    },
                    outcome,
                );
                ItemsFetch::Loading
            } else {
                ItemsFetch::Failed("this machine does not list worker runs".into())
            };
        self.overlay = Some(ClientShellOverlay::WorkerItems(WorkerItemsOverlay {
            button,
            endpoint_id,
            fetch,
            open: None,
            highlighted: None,
        }));
    }

    /// Takes the reply into the open dropdown of the same machine.
    pub(super) fn complete_worker_runs(
        &mut self,
        endpoint_id: ClientEndpointId,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if overlay.endpoint_id != endpoint_id {
            return (false, Vec::new());
        }
        overlay.fetch = match result {
            Ok(crate::api::schema::ResponseResult::WorkerRuns { items, unassigned }) => {
                ItemsFetch::Loaded { items, unassigned }
            }
            Ok(_) => ItemsFetch::Failed("unexpected reply".into()),
            Err(error) => ItemsFetch::Failed(error.message),
        };
        (true, Vec::new())
    }

    /// The open dropdown's rows with this client's icons and clock.
    pub(super) fn worker_items_rows(&self) -> Vec<ItemsRow> {
        let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_ref() else {
            return Vec::new();
        };
        let style = self.config.status_indicators;
        let palette = &self.config.palette;
        overlay.rows(
            |status| {
                (
                    super::agent_icon(status, AgentMark::None, style),
                    super::agent_color(status, AgentMark::None, palette),
                )
            },
            crate::usage::now_unix(),
            super::usage::local_utc_offset_secs(),
        )
    }

    pub(super) fn highlight_worker_items_row(&mut self, index: usize) {
        if let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() {
            overlay.highlighted = Some(index);
        }
    }

    /// Moves the highlight by `delta` rows; with none, down starts at the
    /// first row and up at the last.
    pub(super) fn move_worker_items_selection(&mut self, delta: isize) {
        let count = self.worker_items_rows().len();
        let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() else {
            return;
        };
        if count == 0 {
            return;
        }
        overlay.highlighted = Some(match overlay.highlighted {
            None if delta < 0 => count - 1,
            None => 0,
            Some(index) => index.saturating_add_signed(delta).min(count - 1),
        });
    }

    /// Back from an item's runs to the list of items.
    pub(super) fn worker_items_back(&mut self) -> bool {
        if let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() {
            if overlay.open.take().is_some() {
                overlay.highlighted = None;
                return true;
            }
        }
        false
    }

    /// Does what row `index` does: opens an item, goes back, or opens a
    /// run's log (the worker log popup) and closes the dropdown.
    pub(super) fn activate_worker_items_row(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(row) = self.worker_items_rows().into_iter().nth(index) else {
            return;
        };
        outcome.repaint = true;
        match row.action {
            ItemsRowAction::None => {}
            ItemsRowAction::Back => {
                self.worker_items_back();
            }
            ItemsRowAction::Open(key) => {
                if let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() {
                    overlay.open = Some(key);
                    overlay.highlighted = None;
                }
            }
            ItemsRowAction::Log(worker_id) => {
                self.overlay = None;
                self.open_worker_log(worker_id, outcome);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientShellWorkerItemCounts;

    #[test]
    fn the_badge_counts_items_in_progress_and_to_acknowledge() {
        let badge = |in_progress, attention| {
            items_badge(&ClientShellWorkerItemCounts {
                in_progress,
                attention,
            })
        };
        assert_eq!(badge(3, 1), "3·1!");
        assert_eq!(badge(3, 0), "3");
        assert_eq!(badge(0, 2), "2!");
        assert_eq!(badge(0, 0), "");
        assert_eq!(items_button_text(&badge(3, 1)), "Items 3·1!");
        assert_eq!(items_button_text(""), "Items");
    }
}
