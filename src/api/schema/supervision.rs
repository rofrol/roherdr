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
    /// The reader's pane (the CLI sends its `HERDR_PANE_ID`). When it holds
    /// the herdr repository's coordination tenure, the findings that hold
    /// are marked seen by that tenure once the reply is made; the reply
    /// shows the marks as they were before this read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// `supervise.acknowledge`: acknowledges an open finding, from the pane that
/// holds the herdr repository's coordination tenure. The user acknowledges
/// with a click in the `?` list of their own client; nobody else does.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuperviseAcknowledgeParams {
    /// `f-` and the finding's number.
    pub finding_id: String,
    /// The acknowledging coordinator's pane (the CLI sends its
    /// `HERDR_PANE_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
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
    /// A coordinator's turn ended with more background tasks running than
    /// herdr-job waits with a live record (its `bg_waits` token; a wait on
    /// another pane's job is one) and no other wait edge the server knows
    /// of (a running herdr-job job of its pane, a worker of its tenure, a
    /// run of its repository).
    UntrackedShell,
    /// The file system of a coordinator's repository has less free space
    /// than the build guard's 15 GiB.
    DiskLow,
    /// Herdr recorded the verdict of a prompt's assignment (a pane worker's `WORKER-DONE` or
    /// `WORKER-BLOCKED` line) and no waiter acknowledged it (`agent.ack_verdict`) while none
    /// waits for it: its waiter was never started, ended on something else, gave up, or was
    /// killed before or after the answer was written.
    VerdictUndelivered,
    /// A pane worker's turn ended (Claude's `Stop`, no background task left) with its
    /// assignment started and no `WORKER` line, and no turn started since: it asks
    /// something, stopped early, or forgot the line.
    StoppedWithoutVerdict,
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

/// `open` until the user (a click in the `?` list of their own client) or
/// the herdr repository's coordinator acknowledges it; `resolved` once a sweep finds its predicate no longer holding, which
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
    /// Who acknowledged it: `user`, or `coordinator <id>` (the herdr
    /// repository's tenure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_by: Option<String>,
    /// When the herdr repository's coordinator last read it in
    /// `supervise.report`, Unix milliseconds; cleared when it escalates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_seen_ms: Option<u64>,
    /// The tenure that read it then; a later tenure has not seen it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_seen_by: Option<String>,
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
    /// The herdr repository's active coordination tenure, whose inbox the
    /// findings reach; a finding it has not seen has no
    /// `coordinator_seen_by` naming it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub herdr_coordinator_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coordinators: Vec<SupervisedCoordinator>,
    /// The latest seen first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<SupervisionFinding>,
}
