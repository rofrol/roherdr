//! The rule table of the supervision predicates: data, versioned, from a fixed typed
//! vocabulary. A predicate's rule names the counter-evidence checked before a finding is
//! delivered and the event that retracts it. A finding whose counter-evidence holds is
//! `pending` (kept, not delivered) while that continuation is live. Fixing a false positive
//! means adding an entry here, not code in the predicate.
//!
//! Finding states (event-ordered, no timers):
//! - `pending`: an allowed continuation is live, nothing is delivered;
//! - `delivered`: the continuation ended (or never existed, as with a gone pane) without the
//!   expected event; the finding is recorded and goes to its coordinator;
//! - `retracted`: the expected event happened while it was pending: a false positive;
//! - `resolved`: the predicate stopped holding some other way (a real violation corrected).

use super::supervision::PaneFacts;
use crate::api::schema::SupervisionPredicate;

/// Who owns a job that counts as counter-evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JobOwner {
    /// The pane of the worker the assignment was typed into.
    WorkerAssignmentPane,
}

/// Where the completion of such a job is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletionTarget {
    /// The worker's pane, which exists on this server.
    WorkerLivePane,
}

/// One kind of counter-evidence: a fact that makes a continuation likely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CounterEvidence {
    /// A live herdr-job job herdr supervises, owned by `owner`, whose completion goes to
    /// `completion`.
    LiveSupervisedJob {
        owner: JobOwner,
        completion: CompletionTarget,
    },
}

/// The event that retracts a pending finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedEvent {
    /// The same worker's next turn on the same assignment, or its verdict.
    WorkerNextTurnOnAssignment,
}

/// The rule of one predicate.
#[derive(Debug, Clone, Copy)]
pub(super) struct PredicateRule {
    pub(super) predicate: SupervisionPredicate,
    /// Bumped with every change of the entry; retractions are counted per version.
    pub(super) version: u32,
    pub(super) counter_evidence: &'static [CounterEvidence],
    pub(super) expected_event: ExpectedEvent,
}

const OWN_LIVE_JOB: CounterEvidence = CounterEvidence::LiveSupervisedJob {
    owner: JobOwner::WorkerAssignmentPane,
    completion: CompletionTarget::WorkerLivePane,
};

pub(super) const RULES: &[PredicateRule] = &[
    PredicateRule {
        predicate: SupervisionPredicate::StoppedWithoutVerdict,
        version: 1,
        counter_evidence: &[OWN_LIVE_JOB],
        expected_event: ExpectedEvent::WorkerNextTurnOnAssignment,
    },
    PredicateRule {
        predicate: SupervisionPredicate::UnaccountedAssignment,
        version: 1,
        counter_evidence: &[OWN_LIVE_JOB],
        expected_event: ExpectedEvent::WorkerNextTurnOnAssignment,
    },
];

pub(super) fn rule_for(predicate: SupervisionPredicate) -> Option<&'static PredicateRule> {
    RULES.iter().find(|rule| rule.predicate == predicate)
}

/// The counter-evidence that holds for the worker's pane (`None`: the pane is gone), with
/// the jobs it rests on; `None` when no entry holds.
pub(super) fn counter_evidence(
    rule: &PredicateRule,
    pane: Option<&PaneFacts>,
) -> Option<(CounterEvidence, Vec<String>)> {
    rule.counter_evidence.iter().find_map(|entry| match entry {
        CounterEvidence::LiveSupervisedJob {
            owner: JobOwner::WorkerAssignmentPane,
            completion: CompletionTarget::WorkerLivePane,
        } => {
            // The jobs are those of the assignment's pane, and the pane must be live to
            // receive their completion: a gone pane delivers at once.
            let pane = pane?;
            (!pane.running_jobs.is_empty()).then(|| (*entry, pane.running_jobs.clone()))
        }
    })
}

/// How a pending finding ended without being delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoldEnd {
    /// The expected event happened.
    Retracted(&'static str),
    /// The predicate stopped holding some other way.
    Resolved,
}

/// Whether the expected event happened for the assignment `request_id`.
pub(super) fn hold_end(expected: ExpectedEvent, request_id: &str) -> HoldEnd {
    match expected {
        ExpectedEvent::WorkerNextTurnOnAssignment => match crate::assignments::get(request_id) {
            Some(assignment) if assignment.verdict.is_some() => {
                HoldEnd::Retracted("the worker recorded a verdict on the assignment")
            }
            Some(assignment)
                if assignment.is_open()
                    && assignment.stopped_ms.is_none()
                    && assignment.turn_closed.is_none() =>
            {
                HoldEnd::Retracted("the worker's next turn on the assignment started")
            }
            _ => HoldEnd::Resolved,
        },
    }
}
