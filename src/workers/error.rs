//! The errors worker requests are refused with, each with its API code.

#[derive(Debug)]
pub(crate) enum WorkerError {
    NotFound(String),
    Invalid(String),
    NotRunning(String),
    Busy(String),
    NoQuestion(String),
    /// The named question is no longer pending; the message says why.
    QuestionGone(String),
    /// A kill of an exited or lost worker without `force`; the message says
    /// what it would signal.
    NeedsForce(String),
    /// An interrupt named a turn that has already ended.
    TurnEnded(String),
    Unsupported(String),
    /// A command id was reused with another method or other parameters.
    CommandConflict(String),
    /// A command id whose command a server restart cut off before its
    /// outcome was stored.
    CommandInterrupted(String),
    /// The refusal a command id got the first time, returned again.
    Replayed(&'static str, String),
    /// The repository has an active coordinator, or the pane coordinates
    /// another one; the message names it.
    CoordinatorActive(String),
    CoordinatorNotFound(String),
    /// New turns are not admitted while the server drains for an install
    /// ([`WorkerSupervisor::drain`]); the message names it.
    Draining(String),
    /// The worker's output since a server restart is not proven complete
    /// ([`Status::continuity_gap`]); the message names the gap.
    ContinuityGap(String),
    /// No `todo.run` run has that id.
    RunNotFound(String),
    /// A `todo.resume` named an event that is not the run's pending one.
    EventStale(String),
    /// The repository already has a run in progress; the message names it.
    RunActive(String),
    /// A `todo.resume` from a pane other than the run's owner, whose pane is
    /// still there; the message names the owner.
    RunOwnedElsewhere(String),
    /// A `todo.run`'s preflight refused it; the message says which check.
    Preflight(String),
    /// A new run or attempt refused while its provider's usage is high or
    /// unknown, or while the provider has `[usage] gate_max_workers`
    /// workers admitted and not exited; the message names each window and
    /// its value, the failed read, or the starts that hold the slots.
    UsageGate(String),
    /// `history.item` named an item without records.
    HistoryNotFound(String),
    /// No report has that id.
    ReportNotFound(String),
    /// `report.close` from a pane that does not hold the herdr
    /// repository's coordination tenure; the message says what it holds.
    ReportCloseRefused(String),
    /// A run requests a capability its adapter (the worker kind, or the
    /// confined job that runs an operation) cannot enforce; the message
    /// names the capability and the adapter.
    CapabilityUnsupported(String),
    /// The repository's operation has no user grant for its definition;
    /// the message shows the definition and its hash.
    GrantRequired(String),
    /// A grant question answered other than from the user's own client
    /// (a click in the `?` list): the API refuses it, so no agent grants.
    GrantNeedsUser(String),
    /// A supervision finding acknowledged by someone other than the user
    /// (their own client) or the herdr repository's coordinator (its
    /// tenure's pane); the message names who may.
    FindingAckRefused(String),
    /// A `worker.answer_as` from a tenure that no longer owns the worker;
    /// the message names the owner. The answer was not applied.
    OwnershipTransferred(String),
    /// A mutation from a coordinator whose tenure is frozen for a handoff;
    /// the message names the tenure.
    CoordinatorFrozen(String),
    /// A mutation from a session whose tenure was handed over to its
    /// successor; the message names the successor's tenure.
    CoordinatorSuperseded(String),
    /// A self-service handoff step that is not allowed now (`coordinator.ready`,
    /// `coordinator.handoff_successor`, `coordinator.ack`); the message says why.
    HandoffRefused(String),
    /// A start (`worker.run`) in a repository whose coordination tenure is
    /// active, without that tenure's current token or with another one;
    /// the message names the tenure and its holder. Nothing started.
    TenureToken(String),
    Io(std::io::Error),
}

/// Every code a [`WorkerError`] has, so a stored refusal keeps its code.
const WORKER_ERROR_CODES: &[&str] = &[
    "worker_not_found",
    "invalid_request",
    "worker_not_running",
    "worker_busy",
    "worker_no_question",
    "worker_question_gone",
    "worker_needs_force",
    "worker_turn_ended",
    "worker_unsupported",
    "worker_command_conflict",
    "worker_command_interrupted",
    "worker_io_error",
    "coordinator_active",
    "coordinator_not_found",
    "workers_draining",
    "worker_continuity_gap",
    "todo_run_not_found",
    "todo_event_stale",
    "todo_run_active",
    "run_owned_elsewhere",
    "todo_preflight_failed",
    "usage_gate",
    "history_item_not_found",
    "report_not_found",
    "report_close_refused",
    "capability_unsupported",
    "grant_required",
    "grant_needs_user",
    "finding_ack_refused",
    "ownership_transferred",
    "coordinator_frozen",
    "coordinator_superseded",
    "handoff_refused",
    "tenure_token_refused",
];

impl WorkerError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "worker_not_found",
            Self::Invalid(_) => "invalid_request",
            Self::NotRunning(_) => "worker_not_running",
            Self::Busy(_) => "worker_busy",
            Self::NoQuestion(_) => "worker_no_question",
            Self::QuestionGone(_) => "worker_question_gone",
            Self::NeedsForce(_) => "worker_needs_force",
            Self::TurnEnded(_) => "worker_turn_ended",
            Self::Unsupported(_) => "worker_unsupported",
            Self::CommandConflict(_) => "worker_command_conflict",
            Self::CommandInterrupted(_) => "worker_command_interrupted",
            Self::Replayed(code, _) => code,
            Self::Io(_) => "worker_io_error",
            Self::CoordinatorActive(_) => "coordinator_active",
            Self::CoordinatorNotFound(_) => "coordinator_not_found",
            Self::Draining(_) => "workers_draining",
            Self::ContinuityGap(_) => "worker_continuity_gap",
            Self::RunNotFound(_) => "todo_run_not_found",
            Self::EventStale(_) => "todo_event_stale",
            Self::RunActive(_) => "todo_run_active",
            Self::RunOwnedElsewhere(_) => "run_owned_elsewhere",
            Self::Preflight(_) => "todo_preflight_failed",
            Self::UsageGate(_) => "usage_gate",
            Self::HistoryNotFound(_) => "history_item_not_found",
            Self::ReportNotFound(_) => "report_not_found",
            Self::ReportCloseRefused(_) => "report_close_refused",
            Self::CapabilityUnsupported(_) => "capability_unsupported",
            Self::GrantRequired(_) => "grant_required",
            Self::GrantNeedsUser(_) => "grant_needs_user",
            Self::FindingAckRefused(_) => "finding_ack_refused",
            Self::OwnershipTransferred(_) => "ownership_transferred",
            Self::CoordinatorFrozen(_) => "coordinator_frozen",
            Self::CoordinatorSuperseded(_) => "coordinator_superseded",
            Self::HandoffRefused(_) => "handoff_refused",
            Self::TenureToken(_) => "tenure_token_refused",
        }
    }

    /// A refusal stored with a command id, as it is returned again.
    pub(super) fn replayed(code: &str, message: String) -> Self {
        let code = WORKER_ERROR_CODES
            .iter()
            .find(|known| **known == code)
            .copied()
            .unwrap_or("worker_io_error");
        Self::Replayed(code, message)
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "worker {id} not found"),
            Self::Invalid(message)
            | Self::NotRunning(message)
            | Self::Busy(message)
            | Self::NoQuestion(message)
            | Self::QuestionGone(message)
            | Self::NeedsForce(message)
            | Self::TurnEnded(message)
            | Self::Unsupported(message)
            | Self::CommandConflict(message)
            | Self::CommandInterrupted(message)
            | Self::Replayed(_, message)
            | Self::CoordinatorActive(message)
            | Self::CoordinatorNotFound(message)
            | Self::Draining(message)
            | Self::ContinuityGap(message)
            | Self::RunNotFound(message)
            | Self::EventStale(message)
            | Self::RunActive(message)
            | Self::RunOwnedElsewhere(message)
            | Self::Preflight(message)
            | Self::UsageGate(message)
            | Self::HistoryNotFound(message)
            | Self::ReportNotFound(message)
            | Self::ReportCloseRefused(message)
            | Self::CapabilityUnsupported(message)
            | Self::GrantRequired(message)
            | Self::GrantNeedsUser(message)
            | Self::FindingAckRefused(message)
            | Self::CoordinatorFrozen(message)
            | Self::CoordinatorSuperseded(message)
            | Self::HandoffRefused(message)
            | Self::TenureToken(message)
            | Self::OwnershipTransferred(message) => f.write_str(message),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl From<std::io::Error> for WorkerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
