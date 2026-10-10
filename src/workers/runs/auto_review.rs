//! A run's automatic review (`todo run --auto-review`): when the run waits
//! on a `review` event, its driver stops the attempt's worker, runs the
//! verify (the registered checks, read-only) on the event's commit, and
//! asks a model in one bounded, stateless call ([`super::decision`]) with
//! the real results, then applies the typed decision it returns:
//!
//! - `approve` (an optional note): the approval `todo.resume --action
//!   approve` makes, bound to the event's commit and the run's base; the
//!   verify that passed before the review is not run again unless the
//!   commit changed, and an approval of a commit whose verify failed is
//!   escalated;
//! - `retry` (the review text): the next attempt, as `--action retry`
//!   starts it, behind the same usage gate;
//! - `escalate` (a question for the user with options): a new `review`
//!   event the run waits on, carrying the question, a notice to the user,
//!   and an entry in the user's `?` list ([`super::escalations`]); the
//!   coordinator answers it as any review, and the user's answer there
//!   raises a `review` event the server reviews again with that answer.
//!
//! The server checks the output against the schema and the action against
//! the run's state (an approval needs a commit, a retry an attempt left and
//! the usage gate's admission); an action the run cannot take is escalated
//! with the reason. A call that fails or returns invalid output is made
//! once more; a second failure is escalated.
//!
//! The verify is recorded (`run_review_verified`), each call
//! (`run_review_call`), and the decision with its id, input digest, output
//! and model (`run_review_decision`) before it is applied, so a server that
//! ends between them takes what was recorded when it starts instead of
//! verifying or asking again. The application is one transaction that
//! first checks the run still waits on the reviewed event: a coordinator's
//! `todo.resume` that came first wins, and the decision is recorded as
//! superseded (`run_review_superseded`).
//!
//! The call's process exit is the event the driver waits for. It has no
//! deadline: the provider imposes none on a `claude -p` call.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::decision::{self, Prompt, MAX_CALLS};
use super::escalations;
use super::{event_of, git, is_gone, new_event, Run, APPROVE, MAX_ATTEMPTS, NO_DEADLINE, RETRY};
use crate::api::schema::{
    TodoEventKind, TodoRunEvent, TodoRunStatus, TodoStep, WorkerCheckOutcome, WorkerCommandTarget,
    WorkerVerdict, WorkerVerification, WorkerWaitUntil,
};
use crate::workers::{now_ms, WorkerError, WorkerSupervisor};

/// The diff the model gets is cut at this many bytes.
const DIFF_MAX: usize = 200_000;
/// The run events this module writes.
const CALL: &str = "run_review_call";
const DECISION: &str = "run_review_decision";
const SUPERSEDED: &str = "run_review_superseded";
const APPLIED: &str = "run_auto_reviewed";
const VERIFIED: &str = "run_review_verified";
const STOP_INTENT: &str = "run_review_stop_intent";
/// The key a `review` event raised by an escalation carries: the decision
/// that raised it. The server does not review such an event again.
pub(super) const ESCALATES: &str = "escalates";

/// The output's JSON schema, which `claude -p --json-schema` enforces and
/// [`parse_decision`] checks again.
const OUTPUT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"type":"string","enum":["approve","retry","escalate"]},"note":{"type":"string","description":"approve only: an optional note on the approval"},"review":{"type":"string","description":"retry only: what the next attempt must change; its worker gets this text"},"question":{"type":"string","description":"escalate only: the question for the user"},"options":{"type":"array","items":{"type":"string"},"minItems":2,"maxItems":4,"description":"escalate only: the answers the user can choose from, the recommended one first"}}}"#;

const SYSTEM_PROMPT: &str = "You review one attempt of a headless coding worker that herdr's \
TODO driver ran for a TODO item. The input is JSON: the item's text, the worker's task, the \
exact commit subject and the paths it had to keep to, the worker's final reply, the diff of its \
commit against the run's base, and the verify the server ran on that commit before asking you \
(the exact subject, the paths, a clean tree, and every registered check with its outcome and \
evidence). Decide one action and answer only with JSON matching the schema:\n\
- approve (optional note) when the verify passed and the diff does what the item and the task \
ask, within the paths, and nothing in the worker's reply contradicts it;\n\
- retry (review) when something is missing or wrong that the worker can fix, a failed check \
included: the review says concretely what to change, and the next attempt's worker gets it;\n\
- escalate (question, 2 to 4 options, the recommended first) when the attempt needs the user's \
decision (scope, a product choice, a permission, a contradiction in the item), a check could \
not run, or you cannot judge it.\n\
When the input has `user_answer`, the user answered your earlier question: decide by it.\n\
Never approve an attempt without a commit or one whose verify failed.";

const PROMPT: Prompt<'static> = Prompt {
    schema: OUTPUT_SCHEMA,
    system: SYSTEM_PROMPT,
    intro: "Review this attempt and answer with the JSON decision only.",
};

/// The model's decision, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    Approve {
        note: Option<String>,
    },
    Retry {
        review: String,
    },
    Escalate {
        question: String,
        options: Vec<String>,
    },
}

/// The output as the schema allows it, before the per-action checks.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecision {
    action: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    review: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Option<Vec<String>>,
}

/// The text of a field an action needs, trimmed, refused when empty.
pub(super) fn nonempty(action: &str, text: Option<String>, field: &str) -> Result<String, String> {
    text.map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{action} needs a nonempty {field}"))
}

/// Refuses the first field present that the action does not take.
pub(super) fn only(action: &str, fields: &[(&str, bool)]) -> Result<(), String> {
    match fields.iter().find(|(_, present)| *present) {
        Some((field, _)) => Err(format!("{action} takes no {field}")),
        None => Ok(()),
    }
}

/// An escalation's options: 2 to 4, each nonempty once trimmed.
pub(super) fn escalation_options(options: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let options: Vec<String> = options
        .unwrap_or_default()
        .into_iter()
        .map(|option| option.trim().to_owned())
        .collect();
    if !(2..=4).contains(&options.len()) || options.iter().any(String::is_empty) {
        return Err("escalate needs 2 to 4 nonempty options".into());
    }
    Ok(options)
}

/// Checks the model's output: the schema, then that each action has its
/// own fields and only those.
pub(super) fn parse_decision(output: &Value) -> Result<Decision, String> {
    let raw: RawDecision = serde_json::from_value(output.clone())
        .map_err(|error| format!("the output does not match the schema: {error}"))?;
    let action = raw.action.as_str();
    match action {
        "approve" => {
            only(
                action,
                &[
                    ("review", raw.review.is_some()),
                    ("question", raw.question.is_some()),
                    ("options", raw.options.is_some()),
                ],
            )?;
            let note = raw
                .note
                .clone()
                .map(|note| note.trim().to_owned())
                .filter(|note| !note.is_empty());
            Ok(Decision::Approve { note })
        }
        "retry" => {
            only(
                action,
                &[
                    ("note", raw.note.is_some()),
                    ("question", raw.question.is_some()),
                    ("options", raw.options.is_some()),
                ],
            )?;
            Ok(Decision::Retry {
                review: nonempty(action, raw.review.clone(), "review")?,
            })
        }
        "escalate" => {
            only(
                action,
                &[
                    ("note", raw.note.is_some()),
                    ("review", raw.review.is_some()),
                ],
            )?;
            let question = nonempty(action, raw.question.clone(), "question")?;
            let options = escalation_options(raw.options.clone())?;
            Ok(Decision::Escalate { question, options })
        }
        other => Err(format!(
            "action {other:?} is not approve, retry or escalate"
        )),
    }
}

impl Decision {
    fn to_json(&self) -> Value {
        match self {
            Decision::Approve { note: None } => json!({"action": "approve"}),
            Decision::Approve { note: Some(note) } => json!({"action": "approve", "note": note}),
            Decision::Retry { review } => json!({"action": "retry", "review": review}),
            Decision::Escalate { question, options } => {
                json!({"action": "escalate", "question": question, "options": options})
            }
        }
    }
}

/// The diff cut at [`DIFF_MAX`] bytes on a character boundary, and whether
/// it was cut.
fn cut_diff(diff: String) -> (String, bool) {
    if diff.len() <= DIFF_MAX {
        return (diff, false);
    }
    let mut at = DIFF_MAX;
    while !diff.is_char_boundary(at) {
        at -= 1;
    }
    (diff[..at].to_owned(), true)
}

/// Whether the server reviews the run's pending event itself: an automatic
/// run waiting on a `review` event that no escalation raised.
pub(super) fn is_due(run: &Run, seq: i64, body: &Value) -> bool {
    run.finish.auto_review
        && run.info.status == TodoRunStatus::Waiting
        && run.info.step == TodoStep::Review
        && run.info.pending_event == Some(seq)
        && body["kind"] == json!(TodoEventKind::Review)
        && body.get(ESCALATES).is_none()
}

/// A decision as `run_review_decision` records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DecisionRecord {
    decision_id: String,
    /// The `review` event decided.
    event: i64,
    attempt: u32,
    /// The commit and base an approval is bound to.
    commit: Option<String>,
    base: Option<String>,
    /// SHA-256 of the input the model got, in hex.
    input_digest: String,
    model: Option<String>,
    /// The checked decision; none when every call failed.
    output: Option<Value>,
    /// Each failed call's error.
    #[serde(default)]
    errors: Vec<String>,
    /// The verdict of the verify run before the review, of `commit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<WorkerVerdict>,
    /// Its checks that failed, with their evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    failed_checks: Vec<String>,
}

/// How a checked decision is applied to the run.
enum Outcome {
    /// The run took it (or was already past the event).
    Applied,
    /// It goes to the user: the question and its options.
    Escalate(String, Vec<String>),
}

/// The checks of a verification that failed, each with its evidence.
fn failed_checks(verification: &WorkerVerification) -> Vec<String> {
    verification
        .checks
        .iter()
        .filter(|check| check.outcome == WorkerCheckOutcome::Failed)
        .map(|check| {
            let name = check.name.as_deref().unwrap_or(&check.check);
            match check.detail.trim() {
                "" => name.to_owned(),
                detail => format!("{name}: {}", decision::cut(detail)),
            }
        })
        .collect()
}

impl WorkerSupervisor {
    /// The run's pending `review` event, with its body, when the server
    /// reviews it itself.
    pub(super) fn auto_review_due(&self, run: &Run) -> Option<(i64, Value)> {
        if !run.finish.auto_review || run.info.status != TodoRunStatus::Waiting {
            return None;
        }
        let (seq, body) = self
            .run_store()
            .ok()?
            .latest_run_event(&run.info.run_id)
            .ok()??;
        is_due(run, seq, &body).then_some((seq, body))
    }

    /// Reviews the run's pending `review` event `seq`: the decision recorded
    /// for it, else the model's after the verify, recorded first; then
    /// applies it.
    pub(super) fn step_auto_review(
        &self,
        run: &mut Run,
        seq: i64,
        body: &Value,
    ) -> Result<(), String> {
        let event = event_of(seq, body, &run.info);
        let store = self.run_store().map_err(|error| error.to_string())?;
        let records = store
            .run_events_of(&run.info.run_id, DECISION)
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let recorded = records
            .into_iter()
            .filter_map(|body| serde_json::from_value::<DecisionRecord>(body).ok())
            .find(|record| record.event == seq);
        let record = match recorded {
            // Decided before this driver: applied now, never asked again.
            Some(record) => record,
            None => {
                let Some(verification) = self.verify_before_review(run, seq, &event)? else {
                    // The coordinator answered the event meanwhile.
                    return Ok(());
                };
                if self.handed_off() {
                    return Ok(());
                }
                let record = self.decide(run, seq, &event, body, verification.as_ref())?;
                #[cfg(test)]
                super::crashes_after(&run.info.repo, TodoStep::Review)?;
                record
            }
        };
        // A handoff during the call: the new server applies the record.
        if self.handed_off() {
            return Ok(());
        }
        self.apply(run, seq, &event, &record)
    }

    /// Stops the attempt's worker and runs the verify on the event's
    /// commit, recorded with the run's verdict while the run still waits on
    /// `seq` (the run keeps waiting on it). `None` when the coordinator
    /// answered the event meanwhile; `Some(None)` for an event without a
    /// commit, which nothing verifies. A verify already recorded for the
    /// attempt's commit is taken again.
    fn verify_before_review(
        &self,
        run: &mut Run,
        seq: i64,
        event: &TodoRunEvent,
    ) -> Result<Option<Option<WorkerVerification>>, String> {
        let (Some(commit), Some(worker_id), Some(base)) = (
            event.commits.last().cloned(),
            run.info.worker_id.clone(),
            run.info.base.clone(),
        ) else {
            return Ok(Some(None));
        };
        let store = self.run_store().map_err(|error| error.to_string())?;
        let earlier = store
            .run_events_of(&run.info.run_id, VERIFIED)
            .map_err(|error| format!("the worker store failed: {error}"))?
            .into_iter()
            .find(|note| note["attempt"] == json!(run.info.attempt) && note["commit"] == commit)
            .and_then(|note| serde_json::from_value(note["verification"].clone()).ok());
        if let Some(verification) = earlier {
            return Ok(Some(Some(verification)));
        }
        self.stop_for_review(run, &worker_id)?;
        let added = self.contract_check_due(run, &worker_id, &base)?;
        let mut checked = run.clone();
        if let Some((check, _)) = &added {
            checked.info.checks.push(check.name.clone());
            checked.checks.push(check.clone());
        }
        let verification = self.verify_attempt(&checked, &worker_id, &base)?;
        let note = json!({
            "type": VERIFIED, "event": seq, "attempt": run.info.attempt, "commit": commit,
            "verdict": verification.verdict, "check_added": added, "verification": verification,
        });
        let text = serde_json::to_string(&verification).ok();
        let written = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&run.info.run_id)? else {
                    return Ok(None);
                };
                if current.info.status != TodoRunStatus::Waiting
                    || current.info.pending_event != Some(seq)
                {
                    return Ok(None);
                }
                if let Some((check, _)) = &added {
                    current.info.checks.push(check.name.clone());
                    current.checks.push(check.clone());
                }
                current.current.verification = text.clone();
                tx.run_event_keeping_pending(&mut current, &note, now_ms())?;
                Ok(Some(current))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let Some(current) = written else {
            return Ok(None);
        };
        *run = current;
        super::announce();
        Ok(Some(Some(verification)))
    }

    /// Stops the attempt's worker, when it still runs, and waits for its
    /// exit (its own exit event), without writing the run, which keeps
    /// waiting on its event. The stop carries the command id the driver's
    /// own stop uses, so a later stop step finds the worker gone.
    fn stop_for_review(&self, run: &Run, worker_id: &str) -> Result<(), String> {
        let worker = self.status(worker_id).map_err(|error| error.to_string())?;
        if is_gone(&worker) {
            return Ok(());
        }
        self.write_note(
            &run.info.run_id,
            &json!({"type": STOP_INTENT, "worker_id": worker_id}),
        )?;
        let target = WorkerCommandTarget {
            worker_id: worker_id.to_owned(),
            caller_pane_id: run.owner_pane.clone(),
            command_id: Some(format!("{}:{}:stop", run.info.run_id, run.info.attempt)),
        };
        let stopped = match self.stop_command(&target) {
            // A stop a crash cut off: send it again without the id.
            Err(WorkerError::CommandInterrupted(_)) => self.stop_command(&WorkerCommandTarget {
                command_id: None,
                ..target
            }),
            other => other,
        };
        match stopped {
            Ok(_) | Err(WorkerError::NotRunning(_)) => {}
            Err(error) => return Err(format!("stopping worker {worker_id}: {error}")),
        }
        self.wait(worker_id, WorkerWaitUntil::Exit, NO_DEADLINE, || {
            !self.handed_off()
        })
        .map_err(|error| error.to_string())?
        .ok_or("this server handed the run off")?;
        Ok(())
    }

    /// Asks the model about the event (at most [`MAX_CALLS`] calls per
    /// event, counting those a previous server made, and taking a recorded
    /// answer to the same input without asking again) and records each
    /// call and the decision.
    fn decide(
        &self,
        run: &Run,
        seq: i64,
        event: &TodoRunEvent,
        body: &Value,
        verification: Option<&WorkerVerification>,
    ) -> Result<DecisionRecord, String> {
        let input = self.review_input(run, event, body);
        let text = serde_json::to_string_pretty(&input).map_err(|error| error.to_string())?;
        let digest = decision::digest(&text);
        let store = self.run_store().map_err(|error| error.to_string())?;
        let calls = store
            .run_events_of(&run.info.run_id, CALL)
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let (mut errors, answered) =
            decision::recorded_calls(&calls, ("event", &json!(seq)), &digest);
        let mut decided = answered.and_then(|(output, model)| {
            parse_decision(&output)
                .ok()
                .map(|decision| (decision, model))
        });
        while decided.is_none() && errors.len() < MAX_CALLS {
            let call = errors.len() + 1;
            let answered = self
                .call_model(&PROMPT, &text)
                .and_then(|(output, model)| Ok((parse_decision(&output)?, model)));
            let note = match &answered {
                Ok((decision, model)) => json!({
                    "type": CALL, "event": seq, "call": call, "input_digest": digest,
                    "model": model, "output": decision.to_json(),
                }),
                Err(error) => json!({
                    "type": CALL, "event": seq, "call": call, "input_digest": digest,
                    "error": error,
                }),
            };
            self.write_note(&run.info.run_id, &note)?;
            match answered {
                Ok(answer) => decided = Some(answer),
                Err(error) => errors.push(error),
            }
        }
        let (output, model) = match decided {
            Some((decision, model)) => (Some(decision.to_json()), model),
            None => (None, None),
        };
        let record = DecisionRecord {
            decision_id: decision::decision_id(&run.info.run_id, &seq.to_string(), &digest),
            event: seq,
            attempt: run.info.attempt,
            commit: event.commits.last().cloned(),
            base: run.info.base.clone(),
            input_digest: digest,
            model,
            output,
            errors,
            verdict: verification.map(|verification| verification.verdict),
            failed_checks: verification.map(failed_checks).unwrap_or_default(),
        };
        let mut body = serde_json::to_value(&record).map_err(|error| error.to_string())?;
        body["type"] = json!(DECISION);
        self.write_note(&run.info.run_id, &body)?;
        Ok(record)
    }

    /// What the model reviews: the item, the task, the contract, the
    /// worker's final reply, the diff of the event's commit against the
    /// base, the checks with the verify run before the review, and the
    /// user's answer when the event carries one.
    fn review_input(&self, run: &Run, event: &TodoRunEvent, body: &Value) -> Value {
        let repo = Path::new(&run.info.repo);
        let commit = event.commits.last().cloned();
        let (diff, cut) = match (&run.info.base, &commit) {
            (Some(base), Some(commit)) => cut_diff(
                git(repo, &["diff", base, commit])
                    .unwrap_or_else(|error| format!("(the diff could not be read: {error})")),
            ),
            _ => (
                "(the attempt has no commit since the base)".to_owned(),
                false,
            ),
        };
        let item_text = self
            .run_store()
            .ok()
            .and_then(|store| store.run_events_of(&run.info.run_id, "run_created").ok())
            .and_then(|created| created.into_iter().next())
            .map(|created| created["item_text"].clone())
            .unwrap_or(Value::Null);
        let verification = run
            .current
            .verification
            .as_deref()
            .and_then(|verification| serde_json::from_str::<Value>(verification).ok());
        let mut input = json!({
            "item": {"id": run.info.item, "text": item_text},
            "task": run.info.task,
            "attempt": run.info.attempt,
            "max_attempts": MAX_ATTEMPTS,
            "commit_subject": run.info.message,
            "allowed_paths": run.info.paths,
            "base": run.info.base,
            "commit": commit,
            "worker_final_text": event.result_text,
            "diff_stat": event.diff_stat,
            "diff": diff,
            "diff_truncated": cut,
            "event_note": event.error,
            "checks": {
                "registered": run.checks,
                "added_for_api_changes": run.finish.contract_check,
                "verify_before_review": verification,
            },
        });
        if body["user_answer"].is_object() {
            input["user_answer"] = body["user_answer"].clone();
        }
        input
    }

    /// Applies the recorded decision, checked against the run's state; an
    /// action the run cannot take, a failed review and an escalation each
    /// raise a new `review` event for the user.
    fn apply(
        &self,
        run: &mut Run,
        seq: i64,
        event: &TodoRunEvent,
        record: &DecisionRecord,
    ) -> Result<(), String> {
        let decision = record.output.as_ref().map(parse_decision).transpose();
        let outcome = match decision {
            Err(error) => Outcome::Escalate(
                format!(
                    "The recorded automatic review is unreadable ({error}); review the attempt."
                ),
                review_options(),
            ),
            Ok(None) => Outcome::Escalate(
                format!(
                    "The automatic review failed {} times ({}); review attempt {} yourself.",
                    record.errors.len(),
                    record.errors.join("; "),
                    record.attempt
                ),
                review_options(),
            ),
            Ok(Some(Decision::Escalate { question, options })) => {
                Outcome::Escalate(question, options)
            }
            Ok(Some(Decision::Approve { .. })) if record.verdict == Some(WorkerVerdict::Failed) => {
                Outcome::Escalate(
                    format!(
                        "The automatic review approved commit {}, but its verify failed ({}); \
                         review it.",
                        record.commit.as_deref().unwrap_or("none"),
                        record.failed_checks.join("; ")
                    ),
                    review_options(),
                )
            }
            Ok(Some(Decision::Approve { note })) => match &record.commit {
                None => Outcome::Escalate(
                    "The automatic review approved an attempt without a commit; review it."
                        .to_owned(),
                    review_options(),
                ),
                Some(commit) => self.approve(run, seq, record, commit, note)?,
            },
            Ok(Some(Decision::Retry { review })) => self.retry(run, seq, record, &review)?,
        };
        match outcome {
            Outcome::Applied => Ok(()),
            Outcome::Escalate(question, options) => {
                self.escalate_review(run, seq, event, record, &question, &options)
            }
        }
    }

    /// The approval, bound to `commit` and the record's base.
    fn approve(
        &self,
        run: &mut Run,
        seq: i64,
        record: &DecisionRecord,
        commit: &str,
        note: Option<String>,
    ) -> Result<Outcome, String> {
        let body = json!({
            "type": APPLIED, "event": seq, "decision_id": record.decision_id,
            "action": "approve", "note": note, "commit": commit, "base": record.base,
        });
        self.apply_change(run, seq, record, &body, |current| {
            current.info.step = TodoStep::Stop;
            current.current.review_event = Some(seq);
            current.current.review_decision = Some(APPROVE.to_owned());
            current.current.review_text = note.clone();
            current.current.commit = Some(commit.to_owned());
            current.current.base = record.base.clone();
        })
    }

    /// The next attempt with `review`, when the run has one left and the
    /// usage gate admits it.
    fn retry(
        &self,
        run: &mut Run,
        seq: i64,
        record: &DecisionRecord,
        review: &str,
    ) -> Result<Outcome, String> {
        if run.info.attempt >= MAX_ATTEMPTS {
            return Ok(Outcome::Escalate(
                format!(
                    "The automatic review asks for another attempt, but the run used its \
                     {MAX_ATTEMPTS} attempts. Its review:\n{review}"
                ),
                vec!["approve this attempt".into(), "abort the run".into()],
            ));
        }
        let usage = match self.usage_gate(&run.info.repo, false) {
            Ok(usage) => usage,
            Err(refused) => {
                return Ok(Outcome::Escalate(
                    format!(
                        "The automatic review asks for another attempt, but the usage gate \
                         refuses it ({refused}). Its review:\n{review}"
                    ),
                    review_options(),
                ))
            }
        };
        let body = json!({
            "type": APPLIED, "event": seq, "decision_id": record.decision_id,
            "action": "retry", "task": review, "usage_gate": usage,
        });
        self.apply_change(run, seq, record, &body, |current| {
            current.info.step = TodoStep::Restart;
            current.current.review_event = Some(seq);
            current.current.review_decision = Some(RETRY.to_owned());
            current.current.review_text = Some(review.to_owned());
        })
    }

    /// Changes the run in one transaction while it still waits on `seq`,
    /// else records the decision superseded (a coordinator answered first).
    fn apply_change(
        &self,
        run: &mut Run,
        seq: i64,
        record: &DecisionRecord,
        body: &Value,
        change: impl FnOnce(&mut Run),
    ) -> Result<Outcome, String> {
        let store = self.run_store().map_err(|error| error.to_string())?;
        let applied = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&run.info.run_id)? else {
                    return Ok(None);
                };
                if current.info.status != TodoRunStatus::Waiting
                    || current.info.pending_event != Some(seq)
                {
                    return Ok(None);
                }
                current.info.status = TodoRunStatus::Running;
                change(&mut current);
                tx.run_event(&mut current, body, false, now_ms())?;
                Ok(Some(current))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        match applied {
            Some(current) => {
                *run = current;
                super::announce();
            }
            None => self.superseded(&run.info.run_id, seq, record)?,
        }
        Ok(Outcome::Applied)
    }

    fn superseded(&self, run_id: &str, seq: i64, record: &DecisionRecord) -> Result<(), String> {
        self.write_note(
            run_id,
            &json!({
                "type": SUPERSEDED, "event": seq, "decision_id": record.decision_id,
                "note": "the run no longer waits on the reviewed event: the coordinator answered it first",
            }),
        )
    }

    /// A new `review` event with the reviewed one's evidence and the
    /// question, which the run waits on, a notice to the user and an entry
    /// in the user's `?` list.
    fn escalate_review(
        &self,
        run: &mut Run,
        seq: i64,
        reviewed: &TodoRunEvent,
        record: &DecisionRecord,
        question: &str,
        options: &[String],
    ) -> Result<(), String> {
        let mut event = new_event(TodoEventKind::Review);
        event.diff_stat = reviewed.diff_stat.clone();
        event.commits = reviewed.commits.clone();
        event.result_text = reviewed.result_text.clone();
        event.error = Some(format!(
            "The automatic review ({}) asks: {question}\nOptions: {}",
            record.decision_id,
            options.join(" | ")
        ));
        let store = self.run_store().map_err(|error| error.to_string())?;
        let escalated = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&run.info.run_id)? else {
                    return Ok(None);
                };
                if current.info.status != TodoRunStatus::Waiting
                    || current.info.pending_event != Some(seq)
                {
                    return Ok(None);
                }
                let mut body = json!({
                    "type": "run_event",
                    "kind": event.kind,
                    "step": current.info.step,
                    "status": current.info.status,
                    "attempt": current.info.attempt,
                    "event": event,
                    "reviewed_event": seq,
                    "question": question,
                    "options": options,
                });
                body[ESCALATES] = json!(record.decision_id);
                let raised = tx.run_event(&mut current, &body, true, now_ms())?;
                Ok(Some((current, raised, body)))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let Some((current, raised, body)) = escalated else {
            return self.superseded(&run.info.run_id, seq, record);
        };
        *run = current;
        super::announce();
        if let Some(escalation) = escalations::escalation_of(run, raised, &body) {
            escalations::list(escalation);
        }
        crate::workers::notify_user(crate::workers::UserNotice {
            title: format!(
                "{}: the automatic review asks you (run {})",
                run.info.item, run.info.run_id
            ),
            body: format!("{question}\nOptions: {}", options.join(" | ")),
        });
        Ok(())
    }
}

/// The choices a review the model could not settle leaves the user.
fn review_options() -> Vec<String> {
    vec![
        "approve the attempt".into(),
        "retry with a review".into(),
        "abort the run".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_takes_its_own_fields_only() {
        assert_eq!(
            parse_decision(&json!({"action": "approve"})),
            Ok(Decision::Approve { note: None })
        );
        assert_eq!(
            parse_decision(&json!({"action": "approve", "note": " fine "})),
            Ok(Decision::Approve {
                note: Some("fine".into())
            })
        );
        assert_eq!(
            parse_decision(&json!({"action": "retry", "review": "add b"})),
            Ok(Decision::Retry {
                review: "add b".into()
            })
        );
        assert_eq!(
            parse_decision(&json!({"action": "escalate", "question": "q?", "options": ["a", "b"]})),
            Ok(Decision::Escalate {
                question: "q?".into(),
                options: vec!["a".into(), "b".into()]
            })
        );
        for bad in [
            json!({}),
            json!("approve"),
            json!({"action": "merge"}),
            json!({"action": "approve", "review": "x"}),
            json!({"action": "approve", "extra": 1}),
            json!({"action": "retry"}),
            json!({"action": "retry", "review": " "}),
            json!({"action": "retry", "review": "x", "note": "y"}),
            json!({"action": "escalate", "question": "q?"}),
            json!({"action": "escalate", "question": "q?", "options": ["a"]}),
            json!({"action": "escalate", "question": "q?", "options": ["a", ""]}),
            json!({"action": "escalate", "options": ["a", "b"]}),
        ] {
            assert!(parse_decision(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_output_schema_is_json_and_names_the_three_actions() {
        let schema: Value = serde_json::from_str(OUTPUT_SCHEMA).unwrap();
        assert_eq!(
            schema["properties"]["action"]["enum"],
            json!(["approve", "retry", "escalate"])
        );
    }

    #[test]
    fn a_long_diff_is_cut_on_a_character_boundary() {
        let (short, cut) = cut_diff("abc".into());
        assert_eq!((short.as_str(), cut), ("abc", false));
        let long = "é".repeat(DIFF_MAX);
        let (diff, cut) = cut_diff(long);
        assert!(cut && diff.len() <= DIFF_MAX && diff.chars().all(|c| c == 'é'));
    }
}
