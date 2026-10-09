//! The coordinator's Items button: on the line of a tab whose agent
//! coordinates (`tab.set_role coordinator`), it counts the TODO items with
//! workers on them (`3·1!`: items with a running worker, items with an
//! ended run its owner has not acknowledged), from the snapshot. A click
//! opens a dropdown, drawn as the header lists are, of the items with runs,
//! those in progress first, then the most recently ended, each by its
//! title and id, an "Unassigned" entry for runs without an item, and the
//! finished items (closed, gone from `TODO.md`), the most recently closed
//! first. An item opens its history: its timeline as herdr recorded it
//! (the claim with the item's text, each run's attempts with the worker,
//! herdr's verdict, the coordinator's decision and the commit it landed,
//! the notes, the close with its decision and the follow-up items), then
//! the worker runs no attempt names (worker, start and end, outcome,
//! verdict, turns, commits, questions); a run or an attempt opens its log,
//! a follow-up its own history.
//!
//! Everything is fetched when the user opens the dropdown or an item,
//! never in the background: a background request would hold the machine's
//! command lane, and a click in that moment would be refused as busy. So
//! the dropdown asks one thing at a time: `worker.runs`, then once it
//! answered `history.list`; an item's `history.item` when it opens.
//! Drawing reads only what those replies brought.

use crate::api::schema::{
    AgentStatus, HistoryEventKind, HistoryItem, HistoryItemParams, HistoryItemSummary,
    HistoryListParams, HistoryRun, TodoRunStatus, WorkerItemRuns, WorkerRun, WorkerRunOutcome,
    WorkerRunsParams, WorkerVerdict,
};

use super::*;

/// Items the list shows at most, the most recent kept.
const MAX_ITEMS: usize = 20;

/// Finished items the list shows at most, the most recently closed kept;
/// `herdr history` lists them all.
const MAX_FINISHED: usize = 20;

/// Lines of an item's text or a record's text a timeline shows; the rest
/// is counted, and `herdr history --item` prints it whole.
const MAX_TEXT_LINES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerItemsOverlay {
    /// The button it opened from, which it hangs under.
    pub(super) button: Rect,
    pub(super) endpoint_id: ClientEndpointId,
    /// The coordinator tab's repository, whose history it asks for.
    pub(super) repo: Option<String>,
    pub(super) fetch: ItemsFetch,
    /// The recorded items (`history.list`), for the finished ones; none
    /// when the machine keeps no history.
    pub(super) history: Option<HistoryFetch>,
    /// The item whose history it shows; none for the list of items.
    pub(super) open: Option<ItemKey>,
    /// The open item's timeline (`history.item`); none for an item without
    /// records.
    pub(super) timeline: Option<TimelineFetch>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HistoryFetch {
    /// Asked once the runs' reply frees the machine's command lane.
    Queued,
    Loading,
    Failed(String),
    Loaded(Vec<HistoryItemSummary>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TimelineFetch {
    Loading,
    Failed(String),
    Loaded(Box<HistoryItem>),
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
    /// Nothing: a message such as "loading", or a line of text.
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
pub(super) fn items_badge(counts: &crate::protocol::ClientShellWorkerItems) -> String {
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
pub(super) fn items_tooltip(counts: &crate::protocol::ClientShellWorkerItems) -> String {
    format!(
        "TODO items with workers: {} in progress · {} ended, not acknowledged by the coordinator",
        counts.in_progress, counts.attention
    )
}

/// How a run's outcome reads, and the agent state whose glyph it takes.
fn outcome(outcome: WorkerRunOutcome) -> (&'static str, AgentStatus) {
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

/// How the latest `worker.verify` verdict of a run reads.
fn verdict(verdict: WorkerVerdict) -> &'static str {
    match verdict {
        WorkerVerdict::Verified => "verified",
        WorkerVerdict::Failed => "verify failed",
        WorkerVerdict::Unavailable => "verify unavailable",
        WorkerVerdict::Unknown => "verify unknown",
    }
}

/// The agent state whose glyph a verdict takes.
fn verdict_status(verdict: Option<WorkerVerdict>) -> AgentStatus {
    match verdict {
        Some(WorkerVerdict::Verified) => AgentStatus::Done,
        Some(WorkerVerdict::Failed) => AgentStatus::Blocked,
        _ => AgentStatus::Idle,
    }
}

/// How a record's kind reads, and the agent state whose glyph it takes.
fn record(kind: HistoryEventKind) -> (&'static str, Option<AgentStatus>) {
    match kind {
        HistoryEventKind::Claimed => ("claimed", None),
        HistoryEventKind::Noted => ("noted", None),
        HistoryEventKind::Closed => ("closed", Some(AgentStatus::Done)),
        HistoryEventKind::Aborted => ("aborted", Some(AgentStatus::Idle)),
        HistoryEventKind::Blocked => ("blocked", Some(AgentStatus::Blocked)),
        HistoryEventKind::Unknown => ("unknown record", None),
    }
}

fn run_status(status: TodoRunStatus) -> &'static str {
    match status {
        TodoRunStatus::Running => "running",
        TodoRunStatus::Waiting => "waiting",
        TodoRunStatus::Blocked => "blocked",
        TodoRunStatus::Done => "done",
        TodoRunStatus::Aborted => "aborted",
        TodoRunStatus::Unknown => "unknown",
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn count(count: usize, one: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {one}s"),
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
fn sorted_items<'a>(items: impl Iterator<Item = &'a WorkerItemRuns>) -> Vec<&'a WorkerItemRuns> {
    let mut sorted = items.collect::<Vec<_>>();
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

/// An item as the list names it: `<title> · <id>`, or its id alone when
/// no title is known (it left `TODO.md` before herdr recorded its text).
fn item_label(title: Option<&str>, item: &str) -> String {
    match title {
        Some(title) => format!("{title} · {item}"),
        None => item.to_owned(),
    }
}

fn message_row(text: &str) -> ItemsRow {
    ItemsRow {
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
    }
}

/// A record's text as dim rows of its day, at most [`MAX_TEXT_LINES`],
/// blank lines left out.
fn text_rows(text: &str, day: &str) -> Vec<ItemsRow> {
    let lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let mut rows = lines
        .iter()
        .take(MAX_TEXT_LINES)
        .map(|line| ItemsRow {
            row: (
                Some(day.to_owned()),
                String::new(),
                format!("  {line}"),
                false,
                None,
                None,
                true,
            ),
            action: ItemsRowAction::None,
        })
        .collect::<Vec<_>>();
    if lines.len() > MAX_TEXT_LINES {
        rows.push(ItemsRow {
            row: (
                Some(day.to_owned()),
                String::new(),
                match lines.len() - MAX_TEXT_LINES {
                    1 => "  … 1 more line (herdr history --item)".to_owned(),
                    more => format!("  … {more} more lines (herdr history --item)"),
                },
                false,
                None,
                None,
                true,
            ),
            action: ItemsRowAction::None,
        });
    }
    rows
}

/// A run's attempts, each with its worker (whose log it opens), herdr's
/// verdict, the coordinator's decision and the commit it landed.
fn attempt_rows(
    run: &HistoryRun,
    day: &str,
    icon: &impl Fn(AgentStatus) -> (&'static str, ratatui::style::Color),
) -> Vec<ItemsRow> {
    run.attempts
        .iter()
        .map(|attempt| {
            let mut text = format!("attempt {}", attempt.attempt);
            if let Some(worker) = &attempt.worker_id {
                text = format!("{text} · {worker}");
            }
            if let Some(found) = attempt.verdict {
                text = format!("{text} · {}", verdict(found));
            }
            if let Some(decision) = &attempt.decision {
                text = format!("{text} · {decision}");
            }
            if let Some(landed) = &attempt.landed_sha {
                text = format!("{text} · landed {}", short(landed));
            }
            let detail = [
                attempt.branch.clone(),
                attempt
                    .commit
                    .as_deref()
                    .map(|commit| format!("commit {}", short(commit))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            ItemsRow {
                row: (
                    Some(day.to_owned()),
                    String::new(),
                    text,
                    false,
                    Some(icon(verdict_status(attempt.verdict))),
                    (!detail.is_empty()).then(|| detail.join(" · ")),
                    false,
                ),
                action: attempt
                    .worker_id
                    .clone()
                    .map_or(ItemsRowAction::None, ItemsRowAction::Log),
            }
        })
        .collect()
}

/// An item's timeline, oldest first, grouped by day: each record with its
/// text, a claim followed by its run's attempts, a close by its follow-up
/// items (each opens its own history, by `title` when known); runs no
/// record names come last.
fn timeline_rows(
    history: &HistoryItem,
    title: impl Fn(&str) -> Option<String>,
    icon: &impl Fn(AgentStatus) -> (&'static str, ratatui::style::Color),
    now_unix: u64,
    offset: i64,
) -> Vec<ItemsRow> {
    use super::notification_log::{notification_day, notification_time};
    let mut rows = Vec::new();
    let mut shown: Vec<&str> = Vec::new();
    let run_of = |run_id: &str| history.runs.iter().find(|run| run.run_id == run_id);
    for event in &history.events {
        let day = notification_day(event.ts_ms, now_unix, offset);
        let (said, status) = record(event.kind);
        let mut text = said.to_owned();
        if let Some(run_id) = &event.run_id {
            text = format!("{text} · run {run_id}");
            if event.kind == HistoryEventKind::Claimed {
                if let Some(run) = run_of(run_id) {
                    text = format!("{text} · {}", run_status(run.status));
                }
            }
        }
        rows.push(ItemsRow {
            row: (
                Some(day.clone()),
                notification_time(event.ts_ms, offset),
                text,
                false,
                status.map(icon),
                None,
                false,
            ),
            action: ItemsRowAction::None,
        });
        let body = match event.kind {
            HistoryEventKind::Claimed => event.item_text.as_deref(),
            _ => event.text.as_deref(),
        };
        rows.extend(body.map(|body| text_rows(body, &day)).unwrap_or_default());
        if event.kind == HistoryEventKind::Claimed {
            if let Some(run) = event.run_id.as_deref().and_then(run_of) {
                if !shown.contains(&run.run_id.as_str()) {
                    shown.push(&run.run_id);
                    rows.extend(attempt_rows(run, &day, icon));
                }
            }
        }
        if event.kind == HistoryEventKind::Closed {
            rows.extend(event.follow_ups.iter().map(|item| ItemsRow {
                row: (
                    Some(day.clone()),
                    String::new(),
                    format!("follow-up · {}", item_label(title(item).as_deref(), item)),
                    false,
                    None,
                    None,
                    false,
                ),
                action: ItemsRowAction::Open(ItemKey::Item {
                    item: item.clone(),
                    repo: Some(event.repo.clone()),
                }),
            }));
        }
    }
    for run in &history.runs {
        if shown.contains(&run.run_id.as_str()) {
            continue;
        }
        let day = notification_day(run.created_ms, now_unix, offset);
        rows.push(ItemsRow {
            row: (
                Some(day.clone()),
                notification_time(run.created_ms, offset),
                format!("run {} · {}", run.run_id, run_status(run.status)),
                false,
                None,
                None,
                false,
            ),
            action: ItemsRowAction::None,
        });
        rows.extend(attempt_rows(run, &day, icon));
    }
    rows
}

impl WorkerItemsOverlay {
    /// The recorded items, once `history.list` answered.
    fn recorded(&self) -> &[HistoryItemSummary] {
        match &self.history {
            Some(HistoryFetch::Loaded(items)) => items,
            _ => &[],
        }
    }

    fn summary(&self, item: &str) -> Option<&HistoryItemSummary> {
        self.recorded().iter().find(|summary| summary.item == item)
    }

    /// Whether the item's latest record is its close: it is finished.
    fn is_closed(&self, item: &str) -> bool {
        self.summary(item)
            .is_some_and(|summary| summary.last.kind == HistoryEventKind::Closed)
    }

    /// The item's title: from its runs (`TODO.md` at their start), else
    /// from its records.
    fn title(&self, item: &str) -> Option<String> {
        let runs_title = match &self.fetch {
            ItemsFetch::Loaded { items, .. } => items
                .iter()
                .find(|group| group.item == item)
                .and_then(|group| group.title.clone()),
            _ => None,
        };
        runs_title.or_else(|| self.summary(item).and_then(|summary| summary.title.clone()))
    }

    /// The dropdown's rows: the items, or the open item's history. Pure:
    /// `now_unix` and `offset` give the clock.
    pub(super) fn rows(
        &self,
        icon: impl Fn(AgentStatus) -> (&'static str, ratatui::style::Color),
        now_unix: u64,
        offset: i64,
    ) -> Vec<ItemsRow> {
        let (items, unassigned) = match &self.fetch {
            ItemsFetch::Loading => return vec![message_row("loading…")],
            ItemsFetch::Failed(error) => {
                return vec![message_row(&format!("could not list the runs: {error}"))]
            }
            ItemsFetch::Loaded { items, unassigned } => (items, unassigned),
        };
        match &self.open {
            None => self.list_rows(items, unassigned, &icon, now_unix, offset),
            Some(open) => self.open_rows(open, items, unassigned, &icon, now_unix, offset),
        }
    }

    /// The items in progress (with runs, or records without a close), the
    /// unassigned runs, then the finished items by the day they closed.
    fn list_rows(
        &self,
        items: &[WorkerItemRuns],
        unassigned: &[WorkerRun],
        icon: &impl Fn(AgentStatus) -> (&'static str, ratatui::style::Color),
        now_unix: u64,
        offset: i64,
    ) -> Vec<ItemsRow> {
        let mut rows = sorted_items(items.iter().filter(|group| !self.is_closed(&group.item)))
            .into_iter()
            .map(|group| {
                let latest = group.runs.iter().max_by_key(|run| last_ms(run));
                let (said, status) =
                    latest.map_or(("", AgentStatus::Idle), |run| outcome(run.outcome));
                ItemsRow {
                    row: (
                        None,
                        latest.map_or_else(String::new, |run| {
                            super::notification_log::wait_duration(last_ms(run), now_unix)
                        }),
                        item_label(self.title(&group.item).as_deref(), &group.item),
                        false,
                        Some(icon(status)),
                        Some(format!("{} · last {said}", count(group.runs.len(), "run"))),
                        false,
                    ),
                    action: ItemsRowAction::Open(ItemKey::Item {
                        item: group.item.clone(),
                        repo: group.repo.clone(),
                    }),
                }
            })
            .collect::<Vec<_>>();
        // Items with records but no worker run, not closed: a run blocked
        // or aborted before its worker started.
        rows.extend(
            self.recorded()
                .iter()
                .filter(|summary| summary.last.kind != HistoryEventKind::Closed)
                .filter(|summary| !items.iter().any(|group| group.item == summary.item))
                .map(|summary| {
                    let (said, status) = record(summary.last.kind);
                    ItemsRow {
                        row: (
                            None,
                            super::notification_log::wait_duration(summary.last.ts_ms, now_unix),
                            item_label(summary.title.as_deref(), &summary.item),
                            false,
                            Some(icon(status.unwrap_or(AgentStatus::Idle))),
                            Some(format!("no runs · last {said}")),
                            false,
                        ),
                        action: ItemsRowAction::Open(ItemKey::Item {
                            item: summary.item.clone(),
                            repo: Some(summary.repo.clone()),
                        }),
                    }
                }),
        );
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
                        count(unassigned.len(), "run"),
                        unassigned.iter().filter(|run| is_running(run)).count()
                    )),
                    false,
                ),
                action: ItemsRowAction::Open(ItemKey::Unassigned),
            });
        }
        match &self.history {
            Some(HistoryFetch::Queued | HistoryFetch::Loading) => {
                rows.push(message_row("loading the finished items…"));
            }
            Some(HistoryFetch::Failed(error)) => {
                rows.push(message_row(&format!(
                    "could not list the finished items: {error}"
                )));
            }
            Some(HistoryFetch::Loaded(_)) | None => {}
        }
        let mut finished = self
            .recorded()
            .iter()
            .filter(|summary| summary.last.kind == HistoryEventKind::Closed)
            .collect::<Vec<_>>();
        finished.sort_by_key(|summary| std::cmp::Reverse((summary.last.ts_ms, summary.last.id)));
        finished.truncate(MAX_FINISHED);
        rows.extend(finished.into_iter().map(|summary| {
            let day =
                super::notification_log::notification_day(summary.last.ts_ms, now_unix, offset);
            let runs = items
                .iter()
                .find(|group| group.item == summary.item)
                .map_or(0, |group| group.runs.len());
            let mut detail = format!("closed · {}", count(runs, "run"));
            if !summary.last.follow_ups.is_empty() {
                detail = format!(
                    "{detail} · {}",
                    count(summary.last.follow_ups.len(), "follow-up")
                );
            }
            ItemsRow {
                row: (
                    Some(format!("Finished · {day}")),
                    super::notification_log::notification_time(summary.last.ts_ms, offset),
                    item_label(self.title(&summary.item).as_deref(), &summary.item),
                    false,
                    Some(icon(AgentStatus::Done)),
                    Some(detail),
                    false,
                ),
                action: ItemsRowAction::Open(ItemKey::Item {
                    item: summary.item.clone(),
                    repo: Some(summary.repo.clone()),
                }),
            }
        }));
        if rows.is_empty() {
            rows.push(message_row("no worker runs yet"));
        }
        rows
    }

    /// The open item's history: its timeline when it has records, then its
    /// worker runs no attempt names, newest first; or the unassigned runs.
    fn open_rows(
        &self,
        open: &ItemKey,
        items: &[WorkerItemRuns],
        unassigned: &[WorkerRun],
        icon: &impl Fn(AgentStatus) -> (&'static str, ratatui::style::Color),
        now_unix: u64,
        offset: i64,
    ) -> Vec<ItemsRow> {
        let (heading, runs) = match open {
            ItemKey::Unassigned => ("Unassigned".to_owned(), unassigned),
            ItemKey::Item { item, repo } => {
                let runs = items
                    .iter()
                    .find(|group| group.item == *item && group.repo == *repo)
                    .or_else(|| items.iter().find(|group| group.item == *item))
                    .map_or(&[][..], |group| group.runs.as_slice());
                let title = self.title(item).or_else(|| match &self.timeline {
                    Some(TimelineFetch::Loaded(history)) => history.title.clone(),
                    _ => None,
                });
                (item_label(title.as_deref(), item), runs)
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
        // The runs' own group, after a timeline's days.
        let mut runs_day = None;
        let mut named: Vec<&str> = Vec::new();
        match &self.timeline {
            None => {}
            Some(TimelineFetch::Loading) => rows.push(message_row("loading the history…")),
            Some(TimelineFetch::Failed(error)) => {
                rows.push(message_row(&format!("could not read the history: {error}")));
            }
            Some(TimelineFetch::Loaded(history)) => {
                rows.extend(timeline_rows(
                    history,
                    |item| self.title(item),
                    icon,
                    now_unix,
                    offset,
                ));
                named = history
                    .runs
                    .iter()
                    .flat_map(|run| &run.attempts)
                    .filter_map(|attempt| attempt.worker_id.as_deref())
                    .collect();
                runs_day = Some("Worker runs".to_owned());
            }
        }
        let mut runs = runs
            .iter()
            .filter(|run| !named.contains(&run.worker_id.as_str()))
            .collect::<Vec<_>>();
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
            // Herdr's verdict on its commit, not the worker's own word.
            if let Some(verification) = &run.verification {
                text = format!("{text} · {}", verdict(verification.verdict));
            }
            if matches!(open, ItemKey::Unassigned) && !run.name.is_empty() {
                text = format!("{text} · {}", run.name);
            }
            let mut detail = format!(
                "{start}–{end} · {} · {}",
                count(run.turns as usize, "turn"),
                count(run.questions as usize, "question"),
            );
            if !run.commits.is_empty() {
                let commits = run.commits.iter().map(|sha| short(sha)).collect::<Vec<_>>();
                detail = format!("{detail} · {}", commits.join(" "));
            }
            ItemsRow {
                row: (
                    runs_day.clone(),
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
    pub(super) fn worker_items_button_at(&self, point: (u16, u16)) -> Option<(Rect, String)> {
        if self.sidebar_collapsed {
            return None;
        }
        self.hits
            .space_tab_items
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .cloned()
    }

    /// Opens the dropdown under `button` and fetches the runs of the
    /// repository of coordinator tab `tab_id`, or closes it.
    pub(super) fn toggle_worker_items(
        &mut self,
        (button, tab_id): (Rect, String),
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        if matches!(self.overlay, Some(ClientShellOverlay::WorkerItems(_))) {
            self.overlay = None;
            return;
        }
        let endpoint_id = self.active_endpoint_id.clone();
        let repo = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .worker_items
                .iter()
                .find(|items| items.tab_id == tab_id)
                .map(|items| items.repo.clone())
        });
        let method = crate::api::schema::Method::WorkerRuns(WorkerRunsParams {
            item: None,
            repo: repo.clone(),
        });
        let mut history = None;
        let fetch = if repo.is_none() {
            ItemsFetch::Failed("this tab is not in a git repository".into())
        } else if self.endpoint_is_online(&endpoint_id) && self.supports_endpoint_method(&method) {
            self.push_endpoint_method_with_kind(
                method,
                PendingEndpointKind::WorkerRuns {
                    endpoint_id: endpoint_id.clone(),
                },
                outcome,
            );
            // A server older than the history does not advertise it: the
            // list then shows the items with runs only.
            let history_method =
                crate::api::schema::Method::HistoryList(HistoryListParams { repo: repo.clone() });
            if self.supports_endpoint_method(&history_method) {
                history = Some(HistoryFetch::Queued);
            }
            ItemsFetch::Loading
        } else {
            ItemsFetch::Failed("this machine does not list worker runs".into())
        };
        self.overlay = Some(ClientShellOverlay::WorkerItems(WorkerItemsOverlay {
            button,
            endpoint_id,
            repo,
            fetch,
            history,
            open: None,
            timeline: None,
            highlighted: None,
        }));
    }

    /// Takes the reply into the open dropdown of the same machine, then
    /// asks for the recorded items: the runs' request has freed the
    /// machine's command lane.
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
        if overlay.history != Some(HistoryFetch::Queued) {
            return (true, Vec::new());
        }
        let method = crate::api::schema::Method::HistoryList(HistoryListParams {
            repo: overlay.repo.clone(),
        });
        let mut outcome = ClientShellInput::default();
        let sent = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::HistoryList {
                endpoint_id: endpoint_id.clone(),
            },
            &mut outcome,
        );
        if let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() {
            overlay.history = Some(if sent {
                HistoryFetch::Loading
            } else {
                HistoryFetch::Failed("the request was not sent".into())
            });
        }
        (true, outcome.actions)
    }

    /// Takes the recorded items into the open dropdown of the same machine.
    pub(super) fn complete_history_list(
        &mut self,
        endpoint_id: ClientEndpointId,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if overlay.endpoint_id != endpoint_id || overlay.history != Some(HistoryFetch::Loading) {
            return (false, Vec::new());
        }
        overlay.history = Some(match result {
            Ok(crate::api::schema::ResponseResult::HistoryList { items }) => {
                HistoryFetch::Loaded(items)
            }
            Ok(_) => HistoryFetch::Failed("unexpected reply".into()),
            Err(error) => HistoryFetch::Failed(error.message),
        });
        (true, Vec::new())
    }

    /// Takes an item's timeline into the dropdown, while it still shows
    /// that item.
    pub(super) fn complete_history_item(
        &mut self,
        endpoint_id: ClientEndpointId,
        item: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        let shows_it =
            matches!(&overlay.open, Some(ItemKey::Item { item: open, .. }) if *open == item);
        if overlay.endpoint_id != endpoint_id || !shows_it {
            return (false, Vec::new());
        }
        overlay.timeline = Some(match result {
            Ok(crate::api::schema::ResponseResult::HistoryItem { item }) => {
                TimelineFetch::Loaded(Box::new(item))
            }
            Ok(_) => TimelineFetch::Failed("unexpected reply".into()),
            Err(error) => TimelineFetch::Failed(error.message),
        });
        (true, Vec::new())
    }

    /// Opens an item's history (or the unassigned runs) in the dropdown and,
    /// for an item with records, asks for its timeline.
    fn open_worker_item(&mut self, key: ItemKey, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() else {
            return;
        };
        overlay.open = Some(key.clone());
        overlay.highlighted = None;
        overlay.timeline = None;
        let ItemKey::Item { item, .. } = key else {
            return;
        };
        // Only an item herdr has records of has a timeline; the list of
        // records came first, so the command lane is free.
        if overlay.summary(&item).is_none() {
            return;
        }
        let endpoint_id = overlay.endpoint_id.clone();
        let method = crate::api::schema::Method::HistoryItem(HistoryItemParams {
            item: item.clone(),
            repo: overlay.repo.clone(),
        });
        if !self.endpoint_is_online(&endpoint_id) || !self.supports_endpoint_method(&method) {
            return;
        }
        let sent = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::HistoryItem { endpoint_id, item },
            outcome,
        );
        if let Some(ClientShellOverlay::WorkerItems(overlay)) = self.overlay.as_mut() {
            overlay.timeline = sent.then_some(TimelineFetch::Loading);
        }
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
                overlay.timeline = None;
                return true;
            }
        }
        false
    }

    /// Does what row `index` does: opens an item's history, goes back, or
    /// opens a run's log (the worker log popup) and closes the dropdown.
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
            ItemsRowAction::Open(key) => self.open_worker_item(key, outcome),
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
    use crate::api::schema::HistoryEvent;
    use crate::protocol::ClientShellWorkerItems;

    #[test]
    fn the_badge_counts_items_in_progress_and_to_acknowledge() {
        let badge = |in_progress, attention| {
            items_badge(&ClientShellWorkerItems {
                in_progress,
                attention,
                ..Default::default()
            })
        };
        assert_eq!(badge(3, 1), "3·1!");
        assert_eq!(badge(3, 0), "3");
        assert_eq!(badge(0, 2), "2!");
        assert_eq!(badge(0, 0), "");
        assert_eq!(items_button_text(&badge(3, 1)), "Items 3·1!");
        assert_eq!(items_button_text(""), "Items");
    }

    fn event(kind: HistoryEventKind, ts_ms: u64) -> crate::api::schema::HistoryEvent {
        crate::api::schema::HistoryEvent {
            id: 1,
            repo: "/repo".into(),
            item: "t-abcd2345".into(),
            kind,
            ts_ms,
            run_id: Some("r-aaaaaaaa".into()),
            attempt: Some(1),
            text: None,
            item_text: None,
            follow_ups: Vec::new(),
        }
    }

    fn overlay(open: Option<ItemKey>, timeline: Option<TimelineFetch>) -> WorkerItemsOverlay {
        WorkerItemsOverlay {
            button: Rect::default(),
            endpoint_id: ClientEndpointId::Local,
            repo: Some("/repo".into()),
            fetch: ItemsFetch::Loaded {
                items: Vec::new(),
                unassigned: Vec::new(),
            },
            history: Some(HistoryFetch::Loaded(vec![HistoryItemSummary {
                repo: "/repo".into(),
                item: "t-abcd2345".into(),
                title: None,
                last: event(HistoryEventKind::Closed, 1_800_000_000_000),
            }])),
            open,
            timeline,
            highlighted: None,
        }
    }

    fn icon(_: AgentStatus) -> (&'static str, ratatui::style::Color) {
        ("*", ratatui::style::Color::Reset)
    }

    #[test]
    fn a_timeline_reads_in_local_time_and_cuts_long_texts() {
        // 2027-01-15 08:00 UTC; the clock two hours east of it.
        let at = 1_800_000_000_000;
        let now = at / 1000 + 60;
        let long_text = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        let history = HistoryItem {
            item: "t-abcd2345".into(),
            title: None,
            events: vec![
                HistoryEvent {
                    item_text: Some(long_text),
                    ..event(HistoryEventKind::Claimed, at)
                },
                HistoryEvent {
                    text: Some("Not now.".into()),
                    ..event(HistoryEventKind::Aborted, at + 3_600_000)
                },
            ],
            runs: Vec::new(),
        };
        let open = ItemKey::Item {
            item: "t-abcd2345".into(),
            repo: Some("/repo".into()),
        };
        let rows = overlay(Some(open), Some(TimelineFetch::Loaded(Box::new(history)))).rows(
            icon,
            now,
            2 * 3600,
        );
        let rows = rows.iter().map(|row| &row.row).collect::<Vec<_>>();
        // Without a title, the heading is the item's id.
        assert_eq!(rows[0].2, "‹ t-abcd2345");
        assert_eq!(
            (rows[1].1.as_str(), rows[1].2.as_str()),
            ("10:00", "claimed · run r-aaaaaaaa")
        );
        assert_eq!(rows[1].0.as_deref(), Some("Today"));
        // The item's text, cut at its twelfth line.
        assert_eq!(rows[2].2, "  line 1");
        assert_eq!(rows[13].2, "  line 12");
        assert_eq!(rows[14].2, "  … 8 more lines (herdr history --item)");
        assert!(rows[2..=14].iter().all(|row| row.6), "text rows are dim");
        assert_eq!(
            (rows[15].1.as_str(), rows[15].2.as_str()),
            ("11:00", "aborted · run r-aaaaaaaa")
        );
        assert_eq!(rows[16].2, "  Not now.");
        assert_eq!(rows.len(), 17);
    }

    #[test]
    fn the_list_groups_finished_items_by_their_local_day() {
        // 2027-01-15 23:30 UTC is already the 16th two hours east.
        let at = 1_800_055_800_000;
        let mut overlay = overlay(None, None);
        overlay.history = Some(HistoryFetch::Loaded(vec![HistoryItemSummary {
            repo: "/repo".into(),
            item: "t-abcd2345".into(),
            title: Some("Done".into()),
            last: event(HistoryEventKind::Closed, at),
        }]));
        let now = at / 1000 + 86_400 * 3;
        let rows = overlay.rows(icon, now, 2 * 3600);
        assert_eq!(rows.len(), 1);
        let row = &rows[0].row;
        assert_eq!(row.0.as_deref(), Some("Finished · Jan 16"));
        assert_eq!(
            (row.1.as_str(), row.2.as_str()),
            ("01:30", "Done · t-abcd2345")
        );
        assert_eq!(
            rows[0].action,
            ItemsRowAction::Open(ItemKey::Item {
                item: "t-abcd2345".into(),
                repo: Some("/repo".into())
            })
        );
    }
}
