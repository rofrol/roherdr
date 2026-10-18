//! `herdr supervise replay`: a predicate's recorded cases, replayed against a candidate rule
//! before a fix of the predicate is accepted.
//!
//! The replay is pure: it reads the cases `herdr supervise fixtures` exports and a candidate
//! rule as data, and touches no pane, job, finding or hold. A candidate is a rule from the
//! same typed vocabulary as the rule table ([`supervision_rules`]), never code: an unknown
//! kind, owner, completion, expected event or predicate is refused naming the field, and no
//! field defaults.
//!
//! A recorded case is a hold, and a hold is not a finding that fired: the case's counter-evidence
//! held it. What happened next decides the case:
//!
//! - `retracted`: the expected event happened while it was held, so the false positive was
//!   prevented;
//! - `delivered`: the continuation ended without the expected event, so the finding fired after
//!   the hold.
//!
//! So holding a case is not the same as losing it: a held case delivers again when its
//! continuation ends the same way. The replay therefore asks of each case:
//!
//! - is it still held, that is, does one of the candidate's counter-evidence entries name the
//!   evidence the case recorded?
//! - does the candidate expect the event the case recorded? A different event retracts
//!   neither a false positive nor a known-true finding, so a retracted case fails without it
//!   and a delivered case loses its known-true case with it.
//!
//! The checks:
//!
//! 1. every `retracted` case is still held and the candidate expects its recorded event, so a
//!    false positive does not come back;
//! 2. every `delivered` case still fires: the candidate expects its recorded event, so the
//!    same continuation ending without it delivers the finding again (a candidate that would
//!    retract a true finding fails here, whatever it does with the evidence);
//! 3. the set has at least one retracted and one delivered case it can decide: a set that
//!    cannot discriminate fails instead of passing vacuously, and the hold-out set is held to
//!    the same rule.
//!
//! `annotated` cases are the cases the predicate cannot express: listed, deciding nothing. A
//! recorded evidence text, or a recorded event, that the vocabulary cannot read is reported
//! as unrepresentable and decides nothing either, never silently as delivered; check 3 then
//! fails a set whose every case is unrepresentable.
//!
//! A hold-out set, one the fixer was not shown, runs the same checks; its result is part of
//! the replay's and its cases stay out of the primary set's totals. Without it the replay is
//! only a quick look, not an acceptance.

use serde::Serialize;

use super::supervision_rules::{CounterEvidence, ExpectedEvent};
use crate::api::schema::{SupervisionFixture, SupervisionFixtures, SupervisionPredicate};

/// The wire name of a predicate, for reports and refusals.
fn predicate_name(predicate: SupervisionPredicate) -> String {
    serde_json::to_value(predicate)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// The fields a candidate rule names, and the only ones it may.
const CANDIDATE_FIELDS: [&str; 4] = ["predicate", "version", "counter_evidence", "expected_event"];

/// The fields of one candidate counter-evidence entry.
const ENTRY_FIELDS: [&str; 3] = ["kind", "owner", "completion"];

/// The fields of a `supervise.fixtures` reply.
const FIXTURE_FIELDS: [&str; 5] = [
    "predicate",
    "rule_version",
    "retracted",
    "delivered",
    "annotated",
];

/// The fields a `supervise.fixtures` reply must carry: without them a file would read as an
/// empty set instead of what the server exported.
const FIXTURE_REQUIRED: [&str; 4] = ["predicate", "rule_version", "retracted", "delivered"];

/// A candidate rule, as `herdr supervise replay --candidate` reads it: the same typed
/// vocabulary as the rule table, so a fix of a predicate stays data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateRule {
    predicate: SupervisionPredicate,
    version: u32,
    counter_evidence: Vec<CounterEvidence>,
    expected_event: ExpectedEvent,
}

impl CandidateRule {
    /// The predicate the candidate rules.
    pub(crate) fn predicate(&self) -> SupervisionPredicate {
        self.predicate
    }

    fn identity(&self) -> CandidateIdentity {
        CandidateIdentity {
            predicate: self.predicate,
            version: self.version,
            expected_event: self.expected_event.name().to_owned(),
        }
    }
}

/// A string field of a JSON object, refused with the field's place and name when it is
/// missing or not a string.
fn field_text<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
    place: &str,
) -> Result<&'a str, String> {
    let value = object
        .get(name)
        .ok_or_else(|| format!("{place} has no {name}"))?;
    value
        .as_str()
        .ok_or_else(|| format!("{place} {name} must be a string"))
}

/// Reads a candidate rule from its JSON description. Every value must name one of the rule
/// table's vocabulary values: an unknown predicate, kind, owner, completion or expected event,
/// and a missing or extra field, is refused naming it, and nothing defaults.
pub(crate) fn parse_candidate(text: &str) -> Result<CandidateRule, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| format!("the candidate is not JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "the candidate must be a JSON object".to_owned())?;
    for field in object.keys() {
        if !CANDIDATE_FIELDS.contains(&field.as_str()) {
            return Err(format!("unknown candidate field: {field}"));
        }
    }
    let name = field_text(object, "predicate", "the candidate")?;
    let predicate = match serde_json::from_value::<SupervisionPredicate>(serde_json::Value::String(
        name.to_owned(),
    )) {
        Ok(SupervisionPredicate::Unknown) | Err(_) => {
            return Err(format!("unknown candidate predicate: {name}"))
        }
        Ok(predicate) => predicate,
    };
    let version = object
        .get("version")
        .ok_or_else(|| "the candidate has no version".to_owned())?
        .as_u64()
        .ok_or_else(|| "the candidate version must be a whole number".to_owned())?;
    let version = u32::try_from(version)
        .map_err(|_| format!("the candidate version {version} is not a rule version"))?;
    let entries = object
        .get("counter_evidence")
        .ok_or_else(|| "the candidate has no counter_evidence list".to_owned())?
        .as_array()
        .ok_or_else(|| "the candidate counter_evidence must be a list".to_owned())?;
    let mut counter_evidence = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let place = format!("the candidate counter-evidence entry {}", index + 1);
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("{place} must be an object"))?;
        for field in entry.keys() {
            if !ENTRY_FIELDS.contains(&field.as_str()) {
                return Err(format!("unknown counter-evidence field: {field}"));
            }
        }
        counter_evidence.push(CounterEvidence::parse(
            field_text(entry, "kind", &place)?,
            field_text(entry, "owner", &place)?,
            field_text(entry, "completion", &place)?,
        )?);
    }
    let expected_event =
        ExpectedEvent::parse(field_text(object, "expected_event", "the candidate")?)?;
    Ok(CandidateRule {
        predicate,
        version,
        counter_evidence,
        expected_event,
    })
}

/// Reads a `supervise.fixtures` reply as `herdr supervise fixtures` prints it, or the fixtures
/// object alone. A JSON object that is neither is refused: a missing field must not read as an
/// empty set.
pub(crate) fn parse_fixtures(text: &str) -> Result<SupervisionFixtures, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| format!("the fixtures file is not JSON: {error}"))?;
    let fixtures = match value
        .get("result")
        .and_then(|result| result.get("fixtures"))
    {
        Some(fixtures) => fixtures.clone(),
        None => match value.get("error") {
            Some(error) => {
                return Err(format!(
                    "the fixtures file holds an error reply, not fixtures: {error}"
                ))
            }
            None => value,
        },
    };
    let object = fixtures.as_object().ok_or_else(|| {
        "the fixtures file is not a fixtures reply: it is not a JSON object".to_owned()
    })?;
    for field in object.keys() {
        if !FIXTURE_FIELDS.contains(&field.as_str()) {
            return Err(format!(
                "the fixtures file is not a fixtures reply: it has an unknown field {field}"
            ));
        }
    }
    for field in FIXTURE_REQUIRED {
        if !object.contains_key(field) {
            return Err(format!(
                "the fixtures file is not a fixtures reply: it has no {field}"
            ));
        }
    }
    serde_json::from_value::<SupervisionFixtures>(fixtures)
        .map_err(|error| format!("the fixtures file is not a fixtures reply: {error}"))
}

/// What the candidate would do with a case's recorded evidence, and whether the case can be
/// decided at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Decision {
    /// One of the candidate's entries names the evidence: the finding would be held again, not
    /// delivered, and its recorded end then says whether that is a retraction or a delivery.
    Held,
    /// No entry of the candidate names the evidence: the finding would be delivered.
    Delivered,
    /// The case records evidence this vocabulary cannot read (or a rule that no longer exists):
    /// nothing can be decided from it, and the replay says so instead of calling it delivered.
    Unrepresentable,
}

impl Decision {
    fn text(self) -> &'static str {
        match self {
            Decision::Held => "held",
            Decision::Delivered => "delivered",
            Decision::Unrepresentable => "unrepresentable",
        }
    }
}

/// One recorded case, replayed: what the candidate would do with it and whether that is what
/// the case requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CaseOutcome {
    pub(crate) fingerprint: String,
    /// The counter-evidence the case recorded.
    pub(crate) counter_evidence: String,
    /// The event the rule that held it expected; `None` for a hold recorded before herdr
    /// stored it.
    pub(crate) expected_event: Option<String>,
    pub(crate) decision: Decision,
    /// Whether the case passes its check; `None` when the case cannot decide anything, which
    /// the check itself then catches (a set that cannot discriminate fails).
    pub(crate) passed: Option<bool>,
    /// Why the case cannot decide anything, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) unrepresentable: Option<String>,
}

impl CaseOutcome {
    fn passed(&self) -> Option<bool> {
        self.passed
    }

    fn line(&self) -> String {
        let event = self
            .expected_event
            .as_deref()
            .unwrap_or("no event recorded");
        match self.passed {
            Some(passed) => format!(
                "  {}  evidence {}  expects {}  {}  {}\n",
                self.fingerprint,
                self.counter_evidence,
                event,
                self.decision.text(),
                if passed { "pass" } else { "fail" }
            ),
            None => format!(
                "  {}  evidence {}  expects {}  unrepresentable  decides nothing: {}\n",
                self.fingerprint,
                self.counter_evidence,
                event,
                self.unrepresentable.as_deref().unwrap_or("not decidable")
            ),
        }
    }
}

/// An annotated case: one the predicate cannot express, listed and deciding nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AnnotatedCase {
    pub(crate) fingerprint: Option<String>,
    /// What the predicate lacks: `fact` or `rule`.
    pub(crate) missing: String,
    /// Where the case can be seen.
    pub(crate) evidence: String,
}

/// The checks a replayed fixture set must pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Checks {
    /// Every retracted case is still held by the candidate, which expects its recorded event:
    /// drop that evidence and the false positive comes back.
    pub(crate) retracted_all_held: bool,
    /// The set has a retracted case it can decide, so the replay is not vacuous there.
    pub(crate) has_retracted_case: bool,
    /// Every delivered case still fires: the candidate expects its recorded event, so the same
    /// continuation ending without it delivers the finding again.
    pub(crate) delivered_all_fire: bool,
    /// The set has a known-true case it can decide, so the replay is not vacuous there.
    pub(crate) has_delivered_case: bool,
}

impl Checks {
    fn passed(self) -> bool {
        self.retracted_all_held
            && self.has_retracted_case
            && self.delivered_all_fire
            && self.has_delivered_case
    }
}

/// The cases of one set and how they were replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Totals {
    pub(crate) retracted: usize,
    pub(crate) retracted_passed: usize,
    pub(crate) delivered: usize,
    pub(crate) delivered_passed: usize,
    pub(crate) annotated: usize,
    /// The cases that decided nothing because what they recorded is not in the vocabulary.
    pub(crate) undecidable: usize,
}

/// One recorded set (the primary one, or the hold-out one), replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FixtureReplay {
    pub(crate) predicate: SupervisionPredicate,
    /// The rule version the fixtures were recorded at.
    pub(crate) rule_version: u32,
    pub(crate) retracted: Vec<CaseOutcome>,
    pub(crate) delivered: Vec<CaseOutcome>,
    pub(crate) annotated: Vec<AnnotatedCase>,
    pub(crate) checks: Checks,
    pub(crate) totals: Totals,
    pub(crate) passed: bool,
}

impl FixtureReplay {
    /// Replays `fixtures` against the candidate. A set recorded for another predicate is
    /// refused, since its cases are not this candidate's, and so is a candidate at the
    /// version the set was recorded at: a fix is a new version.
    fn new(candidate: &CandidateRule, fixtures: &SupervisionFixtures) -> Result<Self, String> {
        if fixtures.predicate != candidate.predicate {
            return Err(format!(
                "the candidate names {} but the fixtures were recorded for {}",
                predicate_name(candidate.predicate),
                predicate_name(fixtures.predicate)
            ));
        }
        if candidate.version <= fixtures.rule_version {
            return Err(format!(
                "the candidate is {} v{} but the fixtures were recorded at v{}: a fix is a new \
                 rule version, and replaying the version that recorded the cases proves nothing",
                predicate_name(candidate.predicate),
                candidate.version,
                fixtures.rule_version
            ));
        }
        let retracted = cases(candidate, &fixtures.retracted, Group::Retracted);
        let delivered = cases(candidate, &fixtures.delivered, Group::Delivered);
        let annotated = fixtures
            .annotated
            .iter()
            .map(|annotation| AnnotatedCase {
                fingerprint: annotation.fingerprint.clone(),
                missing: missing_name(annotation.missing),
                evidence: annotation.evidence.clone(),
            })
            .collect();
        let decided = |cases: &[CaseOutcome]| -> Vec<bool> {
            cases.iter().filter_map(CaseOutcome::passed).collect()
        };
        let retracted_decided = decided(&retracted);
        let delivered_decided = decided(&delivered);
        let checks = Checks {
            retracted_all_held: retracted_decided.iter().all(|passed| *passed),
            has_retracted_case: !retracted_decided.is_empty(),
            delivered_all_fire: delivered_decided.iter().all(|passed| *passed),
            has_delivered_case: !delivered_decided.is_empty(),
        };
        let totals = Totals {
            retracted: retracted.len(),
            retracted_passed: retracted_decided.iter().filter(|passed| **passed).count(),
            delivered: delivered.len(),
            delivered_passed: delivered_decided.iter().filter(|passed| **passed).count(),
            annotated: fixtures.annotated.len(),
            undecidable: retracted
                .iter()
                .chain(&delivered)
                .filter(|case| case.passed.is_none())
                .count(),
        };
        Ok(FixtureReplay {
            predicate: fixtures.predicate,
            rule_version: fixtures.rule_version,
            passed: checks.passed(),
            retracted,
            delivered,
            annotated,
            checks,
            totals,
        })
    }

    fn text(&self, heading: &str) -> String {
        let mut out = format!(
            "{heading} {} v{}\n",
            predicate_name(self.predicate),
            self.rule_version
        );
        out.push_str(&self.group(
            "retracted (must still be held, with the recorded event)",
            &self.retracted,
            "retracted",
            self.checks.has_retracted_case,
            self.checks.retracted_all_held,
        ));
        out.push_str(&self.group(
            "delivered (must still fire: the candidate must expect the recorded event)",
            &self.delivered,
            "known-true",
            self.checks.has_delivered_case,
            self.checks.delivered_all_fire,
        ));
        if !self.annotated.is_empty() {
            out.push_str(&format!(
                "annotated (decides nothing): {}\n",
                case_count(self.annotated.len())
            ));
            for annotation in &self.annotated {
                out.push_str(&format!(
                    "  {}  missing {}  evidence {}\n",
                    annotation
                        .fingerprint
                        .as_deref()
                        .unwrap_or("no fingerprint"),
                    annotation.missing,
                    annotation.evidence
                ));
            }
        }
        out
    }

    /// One group of cases with its tally, or why it cannot show anything.
    fn group(
        &self,
        heading: &str,
        cases: &[CaseOutcome],
        what: &str,
        decidable: bool,
        all_passed: bool,
    ) -> String {
        let mut out = String::new();
        if !decidable {
            out.push_str(&format!(
                "{heading}: no {what} case the candidate can decide, so the replay cannot show \
                 it is not vacuous  fail\n"
            ));
        } else {
            out.push_str(&format!(
                "{heading}: {}, {} pass{}\n",
                case_count(cases.len()),
                cases
                    .iter()
                    .filter(|case| case.passed == Some(true))
                    .count(),
                if all_passed { "" } else { " (a case fails)" }
            ));
        }
        for case in cases {
            out.push_str(&case.line());
        }
        out
    }
}

/// What the candidate names, as the replay reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CandidateIdentity {
    pub(crate) predicate: SupervisionPredicate,
    pub(crate) version: u32,
    pub(crate) expected_event: String,
}

/// A candidate rule replayed against the recorded cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReplayReport {
    pub(crate) candidate: CandidateIdentity,
    /// The entries the candidate names, as a recorded hold keeps them.
    pub(crate) counter_evidence: Vec<String>,
    pub(crate) fixtures: FixtureReplay,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hold_out: Option<FixtureReplay>,
    /// Every replayed set passed its checks.
    pub(crate) passed: bool,
    /// The replay is an acceptance only when a hold-out set, one the fixer was not shown, was
    /// replayed too: otherwise it is a quick look.
    pub(crate) accepted: bool,
}

impl ReplayReport {
    /// Replays the recorded cases against the candidate. The fixtures must be the candidate's
    /// predicate's, recorded at an earlier rule version.
    pub(crate) fn new(
        candidate: &CandidateRule,
        fixtures: &SupervisionFixtures,
    ) -> Result<Self, String> {
        let fixtures = FixtureReplay::new(candidate, fixtures)?;
        let passed = fixtures.passed;
        Ok(ReplayReport {
            candidate: candidate.identity(),
            counter_evidence: candidate
                .counter_evidence
                .iter()
                .map(CounterEvidence::recorded)
                .collect(),
            passed,
            accepted: false,
            fixtures,
            hold_out: None,
        })
    }

    /// Runs the same checks on a set the fixer was not shown. Its cases stay out of the
    /// primary set's totals and its result joins the replay's; with it the replay is an
    /// acceptance, without it only a quick look.
    pub(crate) fn with_hold_out(
        mut self,
        candidate: &CandidateRule,
        fixtures: &SupervisionFixtures,
    ) -> Result<Self, String> {
        let hold_out = FixtureReplay::new(candidate, fixtures)?;
        self.passed = self.fixtures.passed && hold_out.passed;
        self.accepted = self.passed;
        self.hold_out = Some(hold_out);
        Ok(self)
    }

    pub(crate) fn passed(&self) -> bool {
        self.passed
    }

    /// Whether this replay can accept the candidate: every check passed on the primary set and
    /// on the hold-out set.
    pub(crate) fn accepted(&self) -> bool {
        self.accepted
    }

    /// The report as one JSON object.
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// The report as text, one case per line.
    pub(crate) fn text(&self) -> String {
        let mut out = format!(
            "candidate {} v{}: expected event {}, counter-evidence {}\n",
            predicate_name(self.candidate.predicate),
            self.candidate.version,
            self.candidate.expected_event,
            entries_text(&self.counter_evidence)
        );
        out.push_str(&self.fixtures.text("fixtures"));
        if let Some(hold_out) = &self.hold_out {
            out.push_str(&hold_out.text("hold-out"));
        }
        if self.accepted {
            out.push_str(
                "replay passes: every case passes its check, on the fixtures and on the hold-out \
                 set\n",
            );
        } else if self.passed {
            out.push_str(
                "the fixtures' cases pass, but this is not an acceptance: replay with --hold-out \
                 FILE, a set the fixer was not shown (exit 3)\n",
            );
        } else {
            out.push_str("replay fails\n");
        }
        out
    }
}

/// Which check a case belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// A false positive the rule held: it must not come back.
    Retracted,
    /// A known-true finding: it must still fire.
    Delivered,
}

/// Replays every case of one group, deciding each by its own recorded evidence and event.
fn cases(
    candidate: &CandidateRule,
    cases: &[SupervisionFixture],
    group: Group,
) -> Vec<CaseOutcome> {
    cases
        .iter()
        .map(|case| outcome(candidate, case, group))
        .collect()
}

/// What the candidate would do with a recorded case. A case is held exactly when one of the
/// candidate's entries names the evidence the case recorded, so two cases with the same
/// recorded evidence are held or delivered alike.
fn outcome(candidate: &CandidateRule, case: &SupervisionFixture, group: Group) -> CaseOutcome {
    let entry = CounterEvidence::from_recorded(&case.counter_evidence);
    let decision = match entry {
        Some(entry) if candidate.counter_evidence.contains(&entry) => Decision::Held,
        Some(_) => Decision::Delivered,
        None => Decision::Unrepresentable,
    };
    let unrepresentable = if entry.is_none() {
        Some(format!(
            "the recorded counter-evidence {} is not in the vocabulary",
            case.counter_evidence
        ))
    } else if case.expected_event.is_none() {
        Some("the case records no expected event".to_owned())
    } else {
        None
    };
    let expected = case.expected_event.as_deref() == Some(candidate.expected_event.name());
    let passed = unrepresentable.is_none().then(|| match group {
        // The false positive stays prevented only while the candidate still holds the case
        // and still waits for the event that retracted it.
        Group::Retracted => decision == Decision::Held && expected,
        // The known-true finding fires again when the same continuation ends without the
        // event the candidate waits for; the evidence only decides whether it is held first.
        Group::Delivered => expected,
    });
    CaseOutcome {
        fingerprint: case.fingerprint.clone(),
        counter_evidence: case.counter_evidence.clone(),
        expected_event: case.expected_event.clone(),
        decision,
        passed,
        unrepresentable,
    }
}

/// The wire name of what a predicate lacks, for the text report.
fn missing_name(missing: crate::api::schema::SupervisionMissing) -> String {
    serde_json::to_value(missing)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn case_count(count: usize) -> String {
    if count == 1 {
        "1 case".to_owned()
    } else {
        format!("{count} cases")
    }
}

fn entries_text(entries: &[String]) -> String {
    match entries.len() {
        0 => "none".to_owned(),
        1 => format!("1 entry: {}", entries[0]),
        count => format!("{count} entries: {}", entries.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{SupervisionAnnotation, SupervisionMissing};

    /// The event the rule table's cases record.
    const EVENT: &str = "worker_next_turn_on_assignment";
    /// An event a rule of another version recorded.
    const OTHER_EVENT: &str = "worker_verdict_on_assignment";
    /// The evidence the vocabulary's one entry records, in its stable text.
    const OWN_LIVE_JOB: &str = "live_supervised_job/worker_assignment_pane/worker_live_pane";
    /// The same evidence as a hold written before the text was stable records it.
    const LEGACY_OWN_LIVE_JOB: &str =
        "LiveSupervisedJob { owner: WorkerAssignmentPane, completion: WorkerLivePane }";
    /// The evidence of a rule whose entry the vocabulary no longer has.
    const RETIRED_ENTRY: &str = "PinnedPane { pane: WorkerAssignmentPane }";

    fn case(fingerprint: &str, counter_evidence: &str) -> SupervisionFixture {
        SupervisionFixture {
            fingerprint: fingerprint.into(),
            request_id: format!("req-{fingerprint}"),
            job_ids: vec![format!("j-{fingerprint}")],
            counter_evidence: counter_evidence.into(),
            expected_event: Some(EVENT.into()),
            opened_ms: 1000,
            ended_ms: Some(1100),
            reason: Some("it resumed".into()),
        }
    }

    fn fixtures(retracted: &[&str], delivered: &[&str], annotated: usize) -> SupervisionFixtures {
        SupervisionFixtures {
            predicate: SupervisionPredicate::StoppedWithoutVerdict,
            rule_version: 1,
            retracted: retracted
                .iter()
                .map(|fingerprint| case(fingerprint, OWN_LIVE_JOB))
                .collect(),
            delivered: delivered
                .iter()
                .map(|fingerprint| case(fingerprint, OWN_LIVE_JOB))
                .collect(),
            annotated: (0..annotated)
                .map(|index| SupervisionAnnotation {
                    finding_id: format!("f-{index}"),
                    fingerprint: Some(format!("fp-annotated-{index}")),
                    predicate: SupervisionPredicate::StoppedWithoutVerdict,
                    rule_version: Some(1),
                    job_ids: Vec::new(),
                    pane_id: None,
                    missing: SupervisionMissing::Rule,
                    evidence: format!("job j-a{index}"),
                    note: None,
                    added_ms: 0,
                })
                .collect(),
        }
    }

    fn candidate(predicate: &str, version: u32, entries: &[(&str, &str, &str)]) -> String {
        let entries: Vec<serde_json::Value> = entries
            .iter()
            .map(|(kind, owner, completion)| {
                serde_json::json!({"kind": kind, "owner": owner, "completion": completion})
            })
            .collect();
        serde_json::json!({
            "predicate": predicate,
            "version": version,
            "counter_evidence": entries,
            "expected_event": EVENT,
        })
        .to_string()
    }

    fn owning_candidate(version: u32) -> CandidateRule {
        parse_candidate(&candidate(
            "stopped_without_verdict",
            version,
            &[(
                "live_supervised_job",
                "worker_assignment_pane",
                "worker_live_pane",
            )],
        ))
        .unwrap()
    }

    fn dropping_candidate(version: u32) -> CandidateRule {
        parse_candidate(&candidate("stopped_without_verdict", version, &[])).unwrap()
    }

    /// A retracted case and a delivered case that recorded the *same* evidence: the candidate
    /// keeps the evidence and expects the recorded event, so it holds the false positive again
    /// and still delivers the known-true finding. Holding both is not losing either: the case
    /// that exposed the wrong model.
    #[test]
    fn keeping_the_shared_evidence_and_the_recorded_event_passes_both_directions() {
        let set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        assert_eq!(report.fixtures.retracted[0].decision, Decision::Held);
        assert_eq!(report.fixtures.retracted[0].passed, Some(true));
        assert_eq!(report.fixtures.delivered[0].decision, Decision::Held);
        assert_eq!(report.fixtures.delivered[0].passed, Some(true));
        assert!(report.fixtures.checks.retracted_all_held);
        assert!(report.fixtures.checks.delivered_all_fire);
        assert!(report.passed(), "{}", report.text());
        assert_eq!(
            report.fixtures.totals,
            Totals {
                retracted: 1,
                retracted_passed: 1,
                delivered: 1,
                delivered_passed: 1,
                annotated: 0,
                undecidable: 0,
            }
        );
        // It is only an acceptance with a hold-out set.
        assert!(!report.accepted());
        let accepted = report.with_hold_out(&owning_candidate(2), &set).unwrap();
        assert!(accepted.accepted(), "{}", accepted.text());
        assert!(
            accepted.to_json().contains("\"accepted\":true"),
            "{}",
            accepted.to_json()
        );
    }

    /// Dropping the retracted case's evidence brings the false positive back, so the retracted
    /// check fails; the known-true case still fires, its event unchanged.
    #[test]
    fn dropping_the_retracted_evidence_fails_the_retracted_check() {
        let set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let report = ReplayReport::new(&dropping_candidate(2), &set).unwrap();
        assert_eq!(report.fixtures.retracted[0].decision, Decision::Delivered);
        assert_eq!(report.fixtures.retracted[0].passed, Some(false));
        assert!(!report.fixtures.checks.retracted_all_held);
        assert!(report.fixtures.checks.has_retracted_case);
        assert!(report.fixtures.checks.delivered_all_fire);
        assert!(!report.passed(), "{}", report.text());
        let text = report.text();
        assert!(text.contains("fp-retracted  evidence"), "{text}");
        assert!(!text.contains("held  pass"), "{text}");
        assert!(text.contains("delivered  fail"), "{text}");
        assert!(text.contains("replay fails"), "{text}");
    }

    /// A candidate that expects another event retracts nothing and delivers nothing it should:
    /// both directions fail, whatever it does with the evidence.
    #[test]
    fn changing_the_expected_event_fails_both_directions() {
        let mut set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        for case in set.retracted.iter_mut().chain(set.delivered.iter_mut()) {
            case.expected_event = Some(OTHER_EVENT.into());
        }
        // Held, and still failing: the hold alone retracts nothing.
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        assert_eq!(report.fixtures.retracted[0].decision, Decision::Held);
        assert_eq!(report.fixtures.retracted[0].passed, Some(false));
        assert_eq!(report.fixtures.delivered[0].decision, Decision::Held);
        assert_eq!(report.fixtures.delivered[0].passed, Some(false));
        assert!(!report.fixtures.checks.retracted_all_held);
        assert!(!report.fixtures.checks.delivered_all_fire);
        assert!(!report.passed(), "{}", report.text());
        let text = report.text();
        assert!(
            text.contains(&format!("expects {OTHER_EVENT}  held  fail")),
            "{text}"
        );

        // Delivered, and still failing: it would fire a finding the rule must retract.
        let report = ReplayReport::new(&dropping_candidate(2), &set).unwrap();
        assert_eq!(report.fixtures.retracted[0].decision, Decision::Delivered);
        assert_eq!(report.fixtures.retracted[0].passed, Some(false));
        assert_eq!(report.fixtures.delivered[0].decision, Decision::Delivered);
        assert_eq!(report.fixtures.delivered[0].passed, Some(false));
        assert!(!report.passed(), "{}", report.text());
    }

    /// A set with no retracted or no known-true case cannot discriminate, and fails instead of
    /// passing vacuously.
    #[test]
    fn a_set_without_a_retracted_or_without_a_known_true_case_fails() {
        let without_known_true = fixtures(&["fp-retracted"], &[], 1);
        let report = ReplayReport::new(&owning_candidate(2), &without_known_true).unwrap();
        assert!(report.fixtures.checks.retracted_all_held);
        assert!(!report.fixtures.checks.has_delivered_case);
        assert!(!report.passed(), "{}", report.text());
        assert!(
            report.text().contains("no known-true case"),
            "{}",
            report.text()
        );
        assert_eq!(report.fixtures.totals.annotated, 1);

        let without_retracted = fixtures(&[], &["fp-delivered"], 0);
        let report = ReplayReport::new(&owning_candidate(2), &without_retracted).unwrap();
        assert!(!report.fixtures.checks.has_retracted_case);
        assert!(report.fixtures.checks.delivered_all_fire);
        assert!(!report.passed(), "{}", report.text());
        assert!(
            report.text().contains("no retracted case"),
            "{}",
            report.text()
        );
    }

    /// A case whose recorded evidence the vocabulary cannot read decides nothing, is reported
    /// as unrepresentable (never as delivered), and leaves a set that cannot discriminate
    /// failing.
    #[test]
    fn unreadable_recorded_evidence_is_reported_as_unrepresentable() {
        let set = fixtures(&[], &["fp-delivered"], 0);
        let set = SupervisionFixtures {
            retracted: vec![case("fp-retracted", RETIRED_ENTRY)],
            ..set
        };
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        let case = &report.fixtures.retracted[0];
        assert_eq!(case.decision, Decision::Unrepresentable);
        assert_eq!(case.passed, None);
        assert!(case
            .unrepresentable
            .as_deref()
            .unwrap_or_default()
            .contains("not in the vocabulary"));
        assert!(!report.fixtures.checks.has_retracted_case);
        assert!(!report.passed(), "{}", report.text());
        assert!(
            report.text().contains("unrepresentable  decides nothing"),
            "{}",
            report.text()
        );
        assert_eq!(report.fixtures.totals.undecidable, 1);
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(
            json["fixtures"]["retracted"][0]["decision"],
            "unrepresentable"
        );
        assert_eq!(
            json["fixtures"]["retracted"][0]["passed"],
            serde_json::Value::Null
        );
    }

    /// A hold written before the evidence text was stable records the entry's `Debug` output;
    /// the replay still reads it as that entry, and a case with no recorded event decides
    /// nothing.
    #[test]
    fn a_legacy_evidence_text_is_read_and_a_missing_event_is_not() {
        let set = fixtures(&[], &["fp-delivered"], 0);
        let set = SupervisionFixtures {
            retracted: vec![case("fp-legacy", LEGACY_OWN_LIVE_JOB)],
            ..set
        };
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        assert_eq!(report.fixtures.retracted[0].decision, Decision::Held);
        assert_eq!(report.fixtures.retracted[0].passed, Some(true));

        let set = fixtures(&[], &["fp-delivered"], 0);
        let mut unrecorded = case("fp-no-event", OWN_LIVE_JOB);
        unrecorded.expected_event = None;
        let set = SupervisionFixtures {
            retracted: vec![unrecorded],
            ..set
        };
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        assert_eq!(report.fixtures.retracted[0].decision, Decision::Held);
        assert_eq!(report.fixtures.retracted[0].passed, None);
        assert!(
            report.fixtures.retracted[0]
                .unrepresentable
                .as_deref()
                .unwrap_or_default()
                .contains("no expected event"),
            "{}",
            report.text()
        );
        assert!(!report.passed(), "{}", report.text());
    }

    /// A hold-out set the fixer was not shown runs the same checks: its failure fails the
    /// replay, and its cases stay out of the primary set's totals.
    #[test]
    fn a_hold_out_failure_fails_the_replay_without_joining_the_totals() {
        let primary = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let hold_out = SupervisionFixtures {
            predicate: SupervisionPredicate::StoppedWithoutVerdict,
            rule_version: 1,
            retracted: vec![case("fp-held-out", RETIRED_ENTRY)],
            delivered: vec![case("fp-held-out-true", OWN_LIVE_JOB)],
            annotated: Vec::new(),
        };
        let report = ReplayReport::new(&owning_candidate(2), &primary)
            .unwrap()
            .with_hold_out(&owning_candidate(2), &hold_out)
            .unwrap();
        assert!(report.fixtures.passed);
        let hold_out_report = report.hold_out.as_ref().unwrap();
        assert!(!hold_out_report.passed);
        assert!(!report.passed(), "{}", report.text());
        assert!(!report.accepted());
        // The primary totals name only the primary set's cases.
        assert_eq!(report.fixtures.totals.retracted, 1);
        assert_eq!(report.fixtures.totals.delivered, 1);
        assert_eq!(report.fixtures.totals.undecidable, 0);
        assert_eq!(hold_out_report.totals.retracted, 1);
        assert!(
            report
                .text()
                .contains("hold-out stopped_without_verdict v1"),
            "{}",
            report.text()
        );
    }

    /// A replay without a hold-out set is not an acceptance: it says so and is not accepted.
    #[test]
    fn a_replay_without_a_hold_out_is_not_an_acceptance() {
        let set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        assert!(report.passed());
        assert!(!report.accepted());
        let text = report.text();
        assert!(text.contains("not an acceptance"), "{text}");
        assert!(text.contains("--hold-out"), "{text}");
        assert!(!text.contains("replay passes"), "{text}");
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["passed"], true);
        assert_eq!(json["accepted"], false);
        assert!(json.get("hold_out").is_none());
    }

    /// A candidate at the version the cases were recorded at, or for another predicate, is
    /// refused naming both.
    #[test]
    fn a_candidate_at_the_recorded_version_or_for_another_predicate_is_refused() {
        let set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let error = ReplayReport::new(&owning_candidate(1), &set).unwrap_err();
        assert!(
            error.contains("the candidate is stopped_without_verdict v1")
                && error.contains("recorded at v1"),
            "{error}"
        );
        let error = ReplayReport::new(
            &parse_candidate(&candidate(
                "unaccounted_assignment",
                2,
                &[(
                    "live_supervised_job",
                    "worker_assignment_pane",
                    "worker_live_pane",
                )],
            ))
            .unwrap(),
            &set,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "the candidate names unaccounted_assignment but the fixtures were recorded for \
             stopped_without_verdict"
        );
    }

    /// An unknown vocabulary value, a missing or extra field are refused naming the field.
    #[test]
    fn a_candidate_outside_the_vocabulary_is_refused() {
        let refused = |text: String| parse_candidate(&text).unwrap_err();
        assert_eq!(
            refused(candidate(
                "stopped_without_verdict",
                2,
                &[(
                    "supervised_job",
                    "worker_assignment_pane",
                    "worker_live_pane"
                )]
            )),
            "unknown counter-evidence kind: supervised_job"
        );
        assert_eq!(
            refused(candidate(
                "stopped_without_verdict",
                2,
                &[("live_supervised_job", "any_pane", "worker_live_pane")]
            )),
            "unknown counter-evidence owner: any_pane"
        );
        assert_eq!(
            refused(candidate(
                "stopped_without_verdict",
                2,
                &[("live_supervised_job", "worker_assignment_pane", "stdout")]
            )),
            "unknown counter-evidence completion: stdout"
        );
        assert_eq!(
            refused(
                serde_json::json!({
                    "predicate": "stopped_without_verdict",
                    "version": 2,
                    "counter_evidence": [],
                    "expected_event": "worker_next_turn",
                })
                .to_string()
            ),
            "unknown expected event: worker_next_turn"
        );
        assert_eq!(
            refused(
                serde_json::json!({
                    "predicate": "nonsense",
                    "version": 2,
                    "counter_evidence": [],
                    "expected_event": EVENT,
                })
                .to_string()
            ),
            "unknown candidate predicate: nonsense"
        );
        assert_eq!(
            refused(
                serde_json::json!({
                    "predicate": "stopped_without_verdict",
                    "version": 2,
                    "counter_evidence": [],
                    "expected_event": EVENT,
                    "code": "fn fix() {}",
                })
                .to_string()
            ),
            "unknown candidate field: code"
        );
        assert_eq!(
            refused(
                serde_json::json!({
                    "predicate": "stopped_without_verdict",
                    "version": 2,
                    "counter_evidence": [{"kind": "live_supervised_job"}],
                    "expected_event": EVENT,
                })
                .to_string()
            ),
            "the candidate counter-evidence entry 1 has no owner"
        );
    }

    /// The text report and the JSON object carry each case's decision, its recorded evidence
    /// and event, and the totals.
    #[test]
    fn the_report_carries_each_case_decision_in_text_and_json() {
        let set = fixtures(&["fp-retracted"], &[], 1);
        let set = SupervisionFixtures {
            delivered: vec![case("fp-delivered", OWN_LIVE_JOB)],
            ..set
        };
        let report = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        let text = report.text();
        for line in [
            "candidate stopped_without_verdict v2: expected event \
             worker_next_turn_on_assignment",
            "fixtures stopped_without_verdict v1",
            "retracted (must still be held, with the recorded event): 1 case, 1 pass",
            "delivered (must still fire: the candidate must expect the recorded event): 1 case, \
             1 pass",
            "annotated (decides nothing): 1 case",
            "missing rule  evidence job j-a0",
        ] {
            assert!(text.contains(line), "missing {line:?} in:\n{text}");
        }
        assert!(
            text.contains(&format!(
                "fp-retracted  evidence {OWN_LIVE_JOB}  expects {EVENT}  held  pass"
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "fp-delivered  evidence {OWN_LIVE_JOB}  expects {EVENT}  held  pass"
            )),
            "{text}"
        );

        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["passed"], true);
        assert_eq!(json["accepted"], false);
        assert_eq!(json["candidate"]["predicate"], "stopped_without_verdict");
        assert_eq!(json["candidate"]["version"], 2);
        assert_eq!(json["fixtures"]["rule_version"], 1);
        assert_eq!(
            json["fixtures"]["retracted"][0]["fingerprint"],
            "fp-retracted"
        );
        assert_eq!(json["fixtures"]["retracted"][0]["expected_event"], EVENT);
        assert_eq!(json["fixtures"]["retracted"][0]["decision"], "held");
        assert_eq!(json["fixtures"]["retracted"][0]["passed"], true);
        assert_eq!(json["fixtures"]["delivered"][0]["decision"], "held");
        assert_eq!(json["fixtures"]["checks"]["has_delivered_case"], true);
        assert_eq!(json["fixtures"]["totals"]["retracted"], 1);
        assert_eq!(json["fixtures"]["annotated"][0]["evidence"], "job j-a0");
    }

    /// A case's decision follows only from its recorded evidence: two cases that recorded the
    /// same evidence decide the same way, whatever list they are in.
    #[test]
    fn a_case_with_the_same_evidence_decides_the_same_way() {
        let set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let holding = ReplayReport::new(&owning_candidate(2), &set).unwrap();
        assert_eq!(holding.fixtures.retracted[0].decision, Decision::Held);
        assert_eq!(holding.fixtures.delivered[0].decision, Decision::Held);
        let delivering = ReplayReport::new(&dropping_candidate(2), &set).unwrap();
        assert_eq!(
            delivering.fixtures.retracted[0].decision,
            Decision::Delivered
        );
        assert_eq!(
            delivering.fixtures.delivered[0].decision,
            Decision::Delivered
        );
        // Their verdicts differ, their decisions do not differ within a candidate.
        assert!(holding.passed());
        assert!(!delivering.passed());
    }

    /// A fixtures file is read as `herdr supervise fixtures` prints it, or as the fixtures
    /// object alone; anything else, an error reply included, is refused instead of reading as
    /// an empty set.
    #[test]
    fn a_fixtures_reply_is_read_from_its_envelope_or_alone() {
        let set = fixtures(&["fp-retracted"], &["fp-delivered"], 0);
        let bare = serde_json::to_value(&set).unwrap();
        let reply = serde_json::json!({"id": "cli:supervise", "result": {"fixtures": bare}});
        assert_eq!(parse_fixtures(&reply.to_string()).unwrap(), set);
        assert_eq!(parse_fixtures(&bare.to_string()).unwrap(), set);

        let refused = |text: serde_json::Value| parse_fixtures(&text.to_string()).unwrap_err();
        assert!(
            refused(serde_json::json!({"id": "cli:supervise", "error": {"code": "x"}}))
                .contains("error reply")
        );
        assert!(refused(serde_json::json!({})).contains("not a fixtures reply"));
        assert!(
            refused(serde_json::json!({"id": "cli:supervise", "result": {}}))
                .contains("not a fixtures reply")
        );
        assert!(refused(serde_json::json!({
            "predicate": "stopped_without_verdict",
            "rule_version": 1,
        }))
        .contains("no retracted"));
        assert!(refused(serde_json::json!([])).contains("not a JSON object"));
    }
}
