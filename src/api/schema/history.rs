//! `history.*`: the life of each TODO item, as herdr's server recorded it in
//! the worker store (outside the repository, local to this machine): when a
//! run claimed it, each note, its close with the decision and the follow-up
//! items, and the runs that were aborted or blocked. The records point at
//! runs, attempts and landings by id; the timeline joins them.

use serde::{Deserialize, Serialize};

use super::todo::TodoRunStatus;
use super::workers::WorkerVerdict;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryListParams {
    /// Only the items of this repository: a directory in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryItemParams {
    pub item: String,
    /// The item's repository: a directory in it. Without it, the item's
    /// records in every repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryReconcileParams {
    /// A directory in the repository whose `TODO.md` is compared with its
    /// records.
    pub repo: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEventKind {
    /// A run started on the item; `item_text` is its text in `TODO.md`
    /// then.
    Claimed,
    /// The coordinator's note was appended to the item; `text` is the note.
    Noted,
    /// The item left `TODO.md` for `DECISIONS.md`: `text` is the decision,
    /// `item_text` the item's last text, `follow_ups` the items added since
    /// the claim.
    Closed,
    /// The run was aborted; `text` is why.
    Aborted,
    /// The run was blocked; `text` is why.
    Blocked,
    #[serde(other)]
    Unknown,
}

/// One record of an item's life, in the order they were written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryEvent {
    /// The record's number, growing in the order of writing.
    pub id: i64,
    pub repo: String,
    pub item: String,
    pub kind: HistoryEventKind,
    /// Unix milliseconds.
    pub ts_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<String>,
}

/// One attempt of a run on the item, from the run's records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryAttempt {
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The attempt's commit as the coordinator's decision saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The coordinator's decision: `approve`, `retry` or `abort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// The attempt's latest verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<WorkerVerdict>,
    /// The commit the attempt landed on `master`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_sha: Option<String>,
}

/// A run on the item, with its attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryRun {
    pub run_id: String,
    pub status: TodoRunStatus,
    pub created_ms: u64,
    /// The run's TODO commit (its note or close), when it made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_commit: Option<String>,
    pub attempts: Vec<HistoryAttempt>,
}

/// An item's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryItem {
    pub item: String,
    /// The item's title, from its text at the last claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The records, oldest first.
    pub events: Vec<HistoryEvent>,
    /// The runs the records name, oldest first.
    pub runs: Vec<HistoryRun>,
}

/// An item with its latest record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryItemSummary {
    pub repo: String,
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub last: HistoryEvent,
}

/// A claim no `noted`, `closed`, `aborted` or `blocked` record of its run
/// followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryOpenClaim {
    pub item: String,
    pub run_id: String,
    pub claimed_ms: u64,
    /// The run's status now; absent when the store has no such run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_status: Option<TodoRunStatus>,
}

/// An item herdr saw in `TODO.md` (at a claim) that is gone from it without
/// a `closed` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryDeletedItem {
    pub item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its latest record, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<HistoryEvent>,
}

/// What `history.reconcile` found; it reports, it never changes a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryReconcile {
    pub repo: String,
    pub open_claims: Vec<HistoryOpenClaim>,
    pub deleted_without_close: Vec<HistoryDeletedItem>,
}
