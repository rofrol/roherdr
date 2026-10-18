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

impl JobOwner {
    /// The owner's text in the vocabulary.
    pub(super) fn name(self) -> &'static str {
        match self {
            JobOwner::WorkerAssignmentPane => "worker_assignment_pane",
        }
    }
}

impl CompletionTarget {
    /// The completion's text in the vocabulary.
    pub(super) fn name(self) -> &'static str {
        match self {
            CompletionTarget::WorkerLivePane => "worker_live_pane",
        }
    }
}

impl CounterEvidence {
    /// The text a hold keeps of this entry, in the store and in `supervise.fixtures`: one
    /// stable text for the sweep that records it and the replay that matches against it, of
    /// the vocabulary's own names as `<kind>/<owner>/<completion>`.
    pub(super) fn recorded(&self) -> String {
        match self {
            CounterEvidence::LiveSupervisedJob { owner, completion } => {
                format!("live_supervised_job/{}/{}", owner.name(), completion.name())
            }
        }
    }

    /// The entry a recorded text names, or `None` when this vocabulary cannot parse it: a hold
    /// of a rule whose entry no longer exists, or one herdr wrote before the text was stable
    /// (it then holds the entry's `Debug` output, which [`CounterEvidence::legacy_recorded`]
    /// still reads). A text the vocabulary cannot parse decides no replay check.
    pub(super) fn from_recorded(text: &str) -> Option<Self> {
        let mut fields = text.split('/');
        if let (Some(kind), Some(owner), Some(completion), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        {
            if let Ok(entry) = Self::parse(kind, owner, completion) {
                return Some(entry);
            }
        }
        VOCABULARY
            .iter()
            .copied()
            .find(|entry| entry.legacy_recorded() == text)
    }

    /// The text a hold written before [`CounterEvidence::recorded`] was stable holds: the
    /// entry's `Debug` output. Read for compatibility only, never written.
    fn legacy_recorded(&self) -> String {
        format!("{self:?}")
    }

    /// The entry this candidate names, from the vocabulary's own texts. An unknown kind,
    /// owner or completion is refused naming the field; nothing defaults.
    pub(super) fn parse(kind: &str, owner: &str, completion: &str) -> Result<Self, String> {
        if kind != "live_supervised_job" {
            return Err(format!("unknown counter-evidence kind: {kind}"));
        }
        let owner = match owner {
            "worker_assignment_pane" => JobOwner::WorkerAssignmentPane,
            other => return Err(format!("unknown counter-evidence owner: {other}")),
        };
        let completion = match completion {
            "worker_live_pane" => CompletionTarget::WorkerLivePane,
            other => return Err(format!("unknown counter-evidence completion: {other}")),
        };
        Ok(CounterEvidence::LiveSupervisedJob { owner, completion })
    }
}

impl ExpectedEvent {
    /// The event this candidate names, from the vocabulary's own texts; an unknown event is
    /// refused naming the field.
    pub(super) fn parse(name: &str) -> Result<Self, String> {
        match name {
            "worker_next_turn_on_assignment" => Ok(ExpectedEvent::WorkerNextTurnOnAssignment),
            other => Err(format!("unknown expected event: {other}")),
        }
    }

    /// The event's text in the vocabulary.
    pub(super) fn name(self) -> &'static str {
        match self {
            ExpectedEvent::WorkerNextTurnOnAssignment => "worker_next_turn_on_assignment",
        }
    }
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

/// Every entry of the vocabulary: a recorded text that is not the stable name of an entry is
/// matched against them, as the `Debug` text herdr wrote before the name was stable.
const VOCABULARY: &[CounterEvidence] = &[OWN_LIVE_JOB];

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

#[cfg(test)]
thread_local! {
    /// A test-only bump of one rule's version, as a change of the rule table would make it.
    static TEST_RULE_VERSION: std::cell::Cell<Option<(SupervisionPredicate, u32)>> =
        const { std::cell::Cell::new(None) };
}

/// The rule of `predicate`, as the table has it now; a test can bump one rule's version.
pub(super) fn rule_for(predicate: SupervisionPredicate) -> Option<PredicateRule> {
    let rule = *RULES.iter().find(|rule| rule.predicate == predicate)?;
    #[cfg(test)]
    {
        if let Some((bumped, version)) = TEST_RULE_VERSION.with(std::cell::Cell::get) {
            if bumped == predicate {
                return Some(PredicateRule { version, ..rule });
            }
        }
    }
    Some(rule)
}

/// Test-only: the rule of `predicate` at `version`, as its next version would be.
#[cfg(test)]
pub(super) fn set_test_rule_version(predicate: SupervisionPredicate, version: u32) {
    TEST_RULE_VERSION.with(|cell| cell.set(Some((predicate, version))));
}

/// Test-only: the rule table's versions again.
#[cfg(test)]
pub(super) fn clear_test_rule_version() {
    TEST_RULE_VERSION.with(|cell| cell.set(None));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The text a hold records is the vocabulary's own names, and it parses back to the entry;
    /// a hold written before the text was stable holds the entry's `Debug` output, which still
    /// parses to it. A text the vocabulary cannot read names no entry.
    #[test]
    fn a_recorded_text_parses_back_to_its_entry() {
        let entry = OWN_LIVE_JOB;
        let recorded = entry.recorded();
        assert_eq!(
            recorded,
            "live_supervised_job/worker_assignment_pane/worker_live_pane"
        );
        assert_eq!(CounterEvidence::from_recorded(&recorded), Some(entry));
        // The old rows keep what `Debug` wrote then: a change of `Debug` must not silently
        // drop their case.
        assert_eq!(
            entry.legacy_recorded(),
            "LiveSupervisedJob { owner: WorkerAssignmentPane, completion: WorkerLivePane }"
        );
        assert_eq!(
            CounterEvidence::from_recorded(
                "LiveSupervisedJob { owner: WorkerAssignmentPane, completion: WorkerLivePane }"
            ),
            Some(entry)
        );
        assert_eq!(CounterEvidence::from_recorded("PinnedPane"), None);
        assert_eq!(
            CounterEvidence::from_recorded("live_supervised_job/any_pane/worker_live_pane"),
            None
        );
        assert_eq!(
            CounterEvidence::from_recorded("live_supervised_job/worker_assignment_pane"),
            None
        );
        // The event's text parses back too.
        assert_eq!(
            ExpectedEvent::parse(ExpectedEvent::WorkerNextTurnOnAssignment.name()),
            Ok(ExpectedEvent::WorkerNextTurnOnAssignment)
        );
    }
}
