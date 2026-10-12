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
///
/// A job is bound to the assignment, not only to the pane: it counts when its pane owns it
/// and it was started after the assignment was typed (`accepted_ms`). A job started before
/// is the pane's older business and no evidence of a continuation of this assignment.
/// Known gap: herdr records a job's start only when its registration reaches this server
/// (`note_job_started`); a job whose start is unknown (registered before a server restart or
/// handoff) keeps the per-pane check and counts.
pub(super) fn counter_evidence(
    rule: &PredicateRule,
    pane: Option<&PaneFacts>,
    accepted_ms: u64,
) -> Option<(CounterEvidence, Vec<String>)> {
    rule.counter_evidence.iter().find_map(|entry| match entry {
        CounterEvidence::LiveSupervisedJob {
            owner: JobOwner::WorkerAssignmentPane,
            completion: CompletionTarget::WorkerLivePane,
        } => {
            // The jobs are those of the assignment's pane, and the pane must be live to
            // receive their completion: a gone pane delivers at once.
            let pane = pane?;
            let jobs: Vec<String> = pane
                .running_jobs
                .iter()
                .filter(|job| match pane.job_started_ms.get(*job) {
                    Some(started) => *started >= accepted_ms,
                    None => true,
                })
                .cloned()
                .collect();
            (!jobs.is_empty()).then_some((*entry, jobs))
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

/// A pending hold, as far as its end is judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingHold {
    pub(super) request_id: String,
    /// When the hold opened, Unix milliseconds: the stop it rests on happened before.
    pub(super) opened_ms: u64,
}

/// Whether the expected event happened for the pending hold: the worker's next turn on the
/// assignment (recorded by herdr when it started, `resumed_ms`) or its verdict, after the
/// hold opened, that is after the job(s) it rests on started, whether they still live or
/// ended. A resume or verdict from before the hold is no evidence; the finding is delivered
/// by the sweep when no such turn follows the job's end.
pub(super) fn hold_end(expected: ExpectedEvent, hold: &PendingHold) -> HoldEnd {
    match expected {
        ExpectedEvent::WorkerNextTurnOnAssignment => {
            let Some(assignment) = crate::assignments::get(&hold.request_id) else {
                return HoldEnd::Resolved;
            };
            if let Some(verdict) = &assignment.verdict {
                if verdict.recorded_ms >= hold.opened_ms {
                    return HoldEnd::Retracted("the worker recorded a verdict on the assignment");
                }
                return HoldEnd::Resolved;
            }
            match assignment.resumed_ms {
                Some(resumed) if resumed >= hold.opened_ms && assignment.is_open() => {
                    HoldEnd::Retracted("the worker's next turn on the assignment started")
                }
                _ => HoldEnd::Resolved,
            }
        }
    }
}
