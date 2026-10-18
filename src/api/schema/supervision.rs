//! `supervise.report`: the server's supervision findings over the
//! coordinators and their workers. A read-only sweep (every five minutes,
//! and when a coordinator's pane reports a turn or changes state) checks
//! named predicates against facts the server already has; each predicate
//! that holds is a finding, stored once per fingerprint while it holds. The
//! sweep never acts: it does not wait, retry, kill, restart, clear a state or
//! mark work done. Herdr delivers each finding's notice to the coordinator
//! that owns it (and a digest to the herdr repository's coordinator) by
//! typing it into its pane at a turn boundary (`supervise.ack`).

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

/// `supervise.ack`: the coordinator a finding's notice was delivered to
/// acknowledges it by the finding's id, from the pane that holds its tenure.
/// It acknowledges that tenure's notices of the finding (queued or
/// delivered); from the herdr repository's coordinator it also acknowledges
/// the finding as `supervise.acknowledge` does. Refused with
/// `finding_ack_refused` from a pane without a tenure, and from a tenure no
/// notice of the finding was addressed to (other than the herdr one).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuperviseAckParams {
    /// `f-` and the finding's number.
    pub finding_id: String,
    /// The acknowledging coordinator's pane (the CLI sends its
    /// `HERDR_PANE_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// Why a coordinator receives a finding's notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionDeliveryRole {
    /// The finding names its tenure (it sent the prompt, or it is the
    /// coordinator the predicate is about).
    Owner,
    /// The herdr repository's coordinator, which fixes the processes, gets
    /// every other owner's findings as a digest.
    Digest,
    /// No active tenure that herdr can prompt owns the finding: the herdr
    /// repository's coordinator gets it, and the user is notified at once.
    Unowned,
    #[serde(other)]
    Unknown,
}

/// One notice of a finding's state (its id and severity) to one coordinator:
/// queued at once, typed into the coordinator's pane with `agent.prompt` only
/// at a turn boundary (its agent rests), then acknowledged by the coordinator
/// (`supervise.ack`). Dispatch and acknowledgement are recorded apart; no
/// timer moves any of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionDelivery {
    /// The recipient's coordination tenure.
    pub coordinator_id: String,
    pub role: SupervisionDeliveryRole,
    /// The idempotency key: the finding's id and severity, one notice per
    /// finding state and recipient.
    pub state_key: String,
    /// Unix milliseconds.
    pub queued_ms: u64,
    /// When herdr typed the notice into the recipient's pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// The `agent.prompt` request that typed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_request_id: Option<String>,
    /// Why the latest dispatch failed: the prompt refused before typing
    /// (still queued), or the typing failed (not sent again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_error: Option<String>,
    /// When the recipient's turn after the notice started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_started_ms: Option<u64>,
    /// When the recipient acknowledged it (`supervise.ack`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_ms: Option<u64>,
    /// When herdr alerted the user that the recipient's turn after the notice
    /// ended without the acknowledgement (once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alerted_ms: Option<u64>,
    /// When herdr dropped it undelivered, and why (the finding resolved, the
    /// tenure ended).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_reason: Option<String>,
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
    /// A pane worker's turn closed (no background task left, or an error ended it) after herdr
    /// accepted its assignment, and neither a verdict, a newer prompt, a stop without a verdict
    /// nor a question of the pane accounts for the assignment, and no turn started since: herdr
    /// did not find its start in the transcript, or the transcript was missing or unreadable.
    UnaccountedAssignment,
    /// The retracted holds of one predicate and rule version reached the configured count
    /// (`[supervision] review_after_retractions`): the predicate's rule needs a review. Herdr
    /// raises it for the herdr repository's coordinator, with the retracted holds'
    /// fingerprints and request ids as evidence (`supervise.fixtures` exports them whole); it
    /// resolves when the rule's version for that predicate changes. Older clients read it as
    /// `unknown`.
    PredicateReviewDue,
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
    /// Its notices to coordinators, the oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<SupervisionDelivery>,
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

/// How many holds of one predicate and rule version ended in each state
/// (`supervision_holds`): `retracted` is a false positive whose expected event followed,
/// `delivered` a finding whose continuation ended without it, `resolved` a predicate that
/// stopped holding some other way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionHoldCount {
    pub predicate: SupervisionPredicate,
    pub rule_version: u32,
    #[serde(default)]
    pub retracted: u64,
    #[serde(default)]
    pub delivered: u64,
    #[serde(default)]
    pub resolved: u64,
}

/// A finding kept back (pending) because an allowed continuation is live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionPendingHold {
    pub fingerprint: String,
    pub predicate: SupervisionPredicate,
    pub rule_version: u32,
    /// The assignment whose continuation is awaited.
    pub request_id: String,
    /// The jobs the hold rests on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_ids: Vec<String>,
    /// Unix milliseconds.
    pub opened_ms: u64,
}

/// `supervise.fixtures`: the recorded holds of one predicate and rule version. Reads only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuperviseFixturesParams {
    pub predicate: SupervisionPredicate,
    /// The rule version; the predicate's current one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

/// One recorded hold with the facts it was recorded with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionFixture {
    pub fingerprint: String,
    /// The assignment whose continuation was awaited.
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_ids: Vec<String>,
    /// The counter-evidence entry that held it.
    pub counter_evidence: String,
    /// Unix milliseconds.
    pub opened_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What a predicate lacks, as an annotation records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionMissing {
    /// A fact the sweep cannot see: the case can be shown, but no predicate
    /// can express it.
    Fact,
    /// A rule: the predicate has nothing that decides the case.
    Rule,
    #[serde(other)]
    Unknown,
}

/// What a predicate lacks, as `supervise.annotate` asks for it: a fact or a
/// rule only, so the request never names a case the server would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SupervisionMissingRequest {
    /// A fact the sweep cannot see: the case can be shown, but no predicate
    /// can express it.
    Fact,
    /// A rule: the predicate has nothing that decides the case.
    Rule,
}

impl From<SupervisionMissingRequest> for SupervisionMissing {
    fn from(missing: SupervisionMissingRequest) -> Self {
        match missing {
            SupervisionMissingRequest::Fact => Self::Fact,
            SupervisionMissingRequest::Rule => Self::Rule,
        }
    }
}

/// `supervise.annotate`: records that the predicate that raised a finding
/// cannot express a case, so a review of the predicate sees it
/// (`supervise.fixtures`). It changes nothing of the finding: not its state,
/// its delivery, its acknowledgement or the holds, and it is not a dispute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuperviseAnnotateParams {
    /// `f-` and the finding's number.
    pub finding_id: String,
    pub missing: SupervisionMissingRequest,
    /// Where the missing case can be seen: a job id, a pane, a log path, a
    /// commit.
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// An annotation on a finding: the case the predicate that raised it cannot
/// express, with the predicate and rule version it was made at. It says only
/// that the predicate lacks something; it is neither a retracted case (one
/// that must not fire) nor a known-true one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionAnnotation {
    /// `f-` and the finding's number.
    pub finding_id: String,
    /// The finding's fingerprint, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub predicate: SupervisionPredicate,
    /// The rule version the finding was raised at; absent for a finding
    /// recorded before herdr stored its rule version, and for a predicate
    /// without a rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_version: Option<u32>,
    /// The jobs the finding named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_ids: Vec<String>,
    /// The pane the finding named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub missing: SupervisionMissing,
    /// Where the missing case can be seen.
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When it was added, Unix milliseconds.
    pub added_ms: u64,
}

/// `supervise.fixtures`' reply: the cases a fix of the predicate must respect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SupervisionFixtures {
    pub predicate: SupervisionPredicate,
    pub rule_version: u32,
    /// The retracted holds: cases that must not fire.
    pub retracted: Vec<SupervisionFixture>,
    /// The delivered holds: known-true cases that must still fire.
    pub delivered: Vec<SupervisionFixture>,
    /// The annotations of the predicate at `rule_version`, the oldest first:
    /// cases the predicate cannot express, which are neither retracted nor
    /// known-true. Absent from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotated: Vec<SupervisionAnnotation>,
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
    /// Holds per predicate and rule version by end state; absent from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hold_counts: Vec<SupervisionHoldCount>,
    /// The holds pending now; absent from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_holds: Vec<SupervisionPendingHold>,
}
