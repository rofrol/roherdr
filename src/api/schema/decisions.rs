//! `decision.*`: the decision ledger. Each record is one question for the
//! user or one decision of theirs, so a coordinator looks an answer up
//! instead of asking again: open (waiting on the user), decided (with the
//! answer and where it came from) or superseded (a later record replaced
//! it). A TODO item's "Needs a decision" question and a `DECISIONS.md`
//! entry name their record through `item` and `entry`.

use serde::{Deserialize, Serialize};

/// Where a record stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// The question waits on the user.
    Open,
    /// The user decided it; `answer` holds the decision.
    Decided,
    /// A later record (`superseded_by`) replaced it.
    Superseded,
    #[serde(other)]
    Unknown,
}

/// Where a decision came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// The user said it in the conversation (or wrote it in their rules).
    User,
    /// A coordinator passed on the user's decision; `relayed_by` names it.
    Relayed,
    /// The user picked it in a question menu (`AskUserQuestion`).
    Menu,
    #[serde(other)]
    Unknown,
}

/// `decision.add`: records a new question for the user, or with `answer` a
/// decision the user already made.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionAddParams {
    /// A directory in the repository the record belongs to; absent for a
    /// record that holds in every repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The question, or the decision as one sentence.
    pub statement: String,
    /// What it covers (an area, a command, a file); `repository` when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The TODO item (`t-...`) whose question or decision it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The `DECISIONS.md` section that records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The record it replaces, which becomes superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// The user's decision: the record is stored decided. Needs `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DecisionSource>,
    /// The coordinator that relayed it (source `relayed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
}

/// `decision.decide`: records the user's answer to an open record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionDecideParams {
    pub id: String,
    pub answer: String,
    pub source: DecisionSource,
    /// The coordinator that relayed it (source `relayed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
}

/// `decision.list`: the records, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionListParams {
    /// Only the records of the repository of this directory, and those that
    /// hold in every repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Only the records with this status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<DecisionStatus>,
}

/// `decision.get`: one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionGetParams {
    pub id: String,
}

/// One record of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionRecord {
    /// `d-` and 8 lowercase base32 characters.
    pub id: String,
    /// The repository; absent when it holds in every repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub scope: String,
    pub statement: String,
    pub status: DecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DecisionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relayed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Unix milliseconds.
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_ms: Option<u64>,
}
