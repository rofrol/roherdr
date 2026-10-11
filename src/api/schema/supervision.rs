//! `supervise.report`: the server's supervision findings over the
//! coordinators and their workers. A read-only sweep (every five minutes,
//! and when a coordinator's pane reports a turn or changes state) checks
//! named predicates against facts the server already has; each predicate
//! that holds is a finding, stored once per fingerprint while it holds. The
//! sweep never acts: it does not wait, retry, prompt, kill, restart, clear a
//! state or mark work done.

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

/// `supervise.report`: the findings that hold (open or acknowledged), or
/// every finding with `all`, and what the latest sweep saw of each
/// coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuperviseReportParams {
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub all: bool,
}

/// The predicate a finding names: a positive fact and the event that did
/// not follow it. Quiet output is never one: it only means progress cannot
/// be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionPredicate {
    /// A coordinator's agent is idle while its repository has runnable
    /// items, no open decision and no wait the server knows of.
    CoordinatorIdle,
    /// A pane's agent shows `working` after the turn end its integration
    /// reported (Claude's `Stop`) with no turn start since
    /// (`UserPromptSubmit`).
    WorkingAfterStop,
    /// A coordinator's turn ended with background tasks running and no wait
    /// edge the server knows of (a running herdr-job job of its pane, a
    /// worker of its tenure, a run of its repository).
    UntrackedShell,
    /// The file system of a coordinator's repository has less free space
    /// than the build guard's 15 GiB.
    DiskLow,
    #[serde(other)]
    Unknown,
}

/// `critical` ranks above `warning`; a finding whose severity rises is
/// escalated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionSeverity {
    Warning,
    Critical,
    #[serde(other)]
    Unknown,
}

/// `open` until the user (or anyone answering its `?` entry) acknowledges
/// it; `resolved` once a sweep finds its predicate no longer holding, which
/// is final: the same fingerprint later opens a new finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionStatus {
    Open,
    Acknowledged,
    Resolved,
    #[serde(other)]
    Unknown,
}

/// What a sweep saw that makes a predicate hold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionEvidence {
    /// When the sweep saw it, Unix milliseconds.
    pub observed_ms: u64,
    /// The events the facts come from, such as `pane w1:p1 state change 41`
    /// or a tenure's start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_ids: Vec<String>,
    /// The facts, one per line, such as `agent_status: idle`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
}

/// One finding: a predicate that held for one subject from `first_seen_ms`
/// to `last_seen_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionFinding {
    /// `f-` and the finding's number.
    pub finding_id: String,
    /// 16 hex characters of the SHA-256 of the predicate and its subject;
    /// one finding holds per fingerprint at a time.
    pub fingerprint: String,
    pub predicate: SupervisionPredicate,
    pub severity: SupervisionSeverity,
    pub status: SupervisionStatus,
    /// One line saying what holds.
    pub summary: String,
    /// The event whose absence the finding names.
    pub expected_event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The coordination tenure it concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_ids: Vec<String>,
    /// The latest sweep's evidence.
    pub evidence: SupervisionEvidence,
    /// Unix milliseconds.
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ms: Option<u64>,
    /// Why it was resolved: what the sweep saw instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

/// What the latest sweep saw of one active coordination tenure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisedCoordinator {
    pub coordinator_id: String,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Its pane's agent status; absent for a headless tenure or a pane the
    /// server does not have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<AgentStatus>,
    /// The tenure's workers whose process runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workers: Vec<String>,
    /// The repository's run in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run: Option<String>,
    /// The findings that hold for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
    /// `cannot establish progress` when no finding holds: the sweep never
    /// claims progress from what it saw, nor a stall from quiet output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `supervise.report`'s reply.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionReport {
    /// When the latest sweep of this server ran; absent before its first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swept_ms: Option<u64>,
    /// What started it: `timer` or the event, such as `pane w1:p1 turn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coordinators: Vec<SupervisedCoordinator>,
    /// The latest seen first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<SupervisionFinding>,
}
