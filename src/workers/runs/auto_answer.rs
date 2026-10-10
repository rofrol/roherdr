//! A run's automatic answers (`todo run --auto-answer`): when the run waits
//! on a `question` event (a request of its worker the worker policy left),
//! its driver decides each question in one bounded, stateless call
//! ([`super::decision`]) with the task, the question (or the tool call,
//! its whole input and why the policy left it) and the run's policy, and
//! applies the typed decision it returns:
//!
//! - `allow`: the tool runs, as `todo.resume --action answer allow`;
//! - `deny` (a message for the model): the tool does not run;
//! - `answer` (one text per question of an `AskUserQuestion`);
//! - `escalate` (a question for the user, with options): the worker's
//!   question is handed to the user (`worker.escalate`, with the model's
//!   question and options as the cause the `?` list and its answer dialog
//!   show), the run raises a new `question` event for the coordinator, and
//!   the user gets a notice.
//!
//! The actions a question takes depend on it ([`allowed_actions`]): a
//! request herdr's policy marks as needing the user
//! ([`policy::needs_the_user`]) is only escalated, without a call; an
//! `AskUserQuestion` is answered, denied or escalated, never allowed; any
//! other request allowed, denied or escalated. The schema the call gets
//! lists only those actions, and the server checks the output against
//! them again. A call that fails or returns invalid output is made once
//! more; a second failure is escalated.
//!
//! Each call (`run_answer_call`) and each decision (`run_answer_decision`,
//! with its id, input digest, output and model) is recorded before it is
//! applied; the answer carries a command id derived from the decision, so
//! a server that ends between them sends the recorded answer once when it
//! starts. When the run's questions are settled, the run goes back to its
//! attention step in one transaction that first checks it still waits on
//! the event (`run_auto_answered`): a coordinator's `todo.resume` that came
//! first wins (`run_answer_superseded`). An escalated event goes on by
//! itself once the user answered its questions (from the `?` list), or
//! when the coordinator answers it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::decision::{self, Prompt, MAX_CALLS};
use super::{event_of, new_event, pending_questions, Run};
use crate::api::schema::{
    TodoEventKind, TodoRunStatus, TodoStep, WorkerAnswerParams, WorkerDecision, WorkerQuestion,
    WorkerQuestionKind,
};
use crate::workers::{now_ms, policy, WorkerError, WorkerSupervisor};

/// The run events this module writes.
const CALL: &str = "run_answer_call";
const DECISION: &str = "run_answer_decision";
const SUPERSEDED: &str = "run_answer_superseded";
const APPLIED: &str = "run_auto_answered";
/// The key a `question` event raised by an escalation carries: the
/// decisions that raised it.
const ESCALATES: &str = "escalates";

const SYSTEM_PROMPT: &str = "A headless coding worker that herdr's TODO driver runs for a TODO \
item asks for something herdr's worker policy did not decide: a tool call to allow or deny, or \
a question (AskUserQuestion) to answer. The input is JSON: the item, the worker's task, the \
commit it must make and the paths it must keep to, the request (the tool, its whole input, why \
the policy left it), and the run's policy with the actions you may take. Decide one action and \
answer only with JSON matching the schema:\n\
- allow when the request serves the task, stays within the worker's directory and the policy, \
and only reads;\n\
- deny (message) when it does not serve the task or the policy forbids it: the message tells \
the worker why and what to do instead;\n\
- answer (answers, one per question, an option's label or free text) when the task or the item \
settles the question;\n\
- escalate (question, 2 to 4 options, the recommended first) when it needs the user: a choice \
of scope or product, a permission, anything destructive, or what you cannot judge.";

/// The model's decision, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    Allow,
    Deny {
        message: String,
    },
    Answer {
        answers: Vec<String>,
    },
    Escalate {
        question: String,
        options: Vec<String>,
    },
}

impl Decision {
    fn to_json(&self) -> Value {
        match self {
            Decision::Allow => json!({"action": "allow"}),
            Decision::Deny { message } => json!({"action": "deny", "message": message}),
            Decision::Answer { answers } => json!({"action": "answer", "answers": answers}),
            Decision::Escalate { question, options } => {
                json!({"action": "escalate", "question": question, "options": options})
            }
        }
    }
}

/// The actions a decision on `question` may take.
pub(super) fn allowed_actions(question: &WorkerQuestion) -> Vec<&'static str> {
    if policy::needs_the_user(question) {
        return vec!["escalate"];
    }
    match question.kind {
        WorkerQuestionKind::Choice => vec!["answer", "deny", "escalate"],
        _ => vec!["allow", "deny", "escalate"],
    }
}

/// The output's JSON schema for these actions, which `claude -p
/// --json-schema` enforces and [`parse_decision`] checks again.
pub(super) fn output_schema(actions: &[&str]) -> String {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action"],
        "properties": {
            "action": {"type": "string", "enum": actions},
            "message": {"type": "string", "description": "deny only: why, and what the worker should do instead"},
            "answers": {"type": "array", "items": {"type": "string"}, "description": "answer only: one answer per question, in order: an option's label, or free text"},
            "question": {"type": "string", "description": "escalate only: the question for the user"},
            "options": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 4, "description": "escalate only: the answers the user can choose from, the recommended one first"},
        },
    })
    .to_string()
}

/// The output as the schema allows it, before the per-action checks.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecision {
    action: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    answers: Option<Vec<String>>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Option<Vec<String>>,
}

/// Checks the model's output: the schema, an action `question` takes, and
/// that each action has its own fields and only those.
pub(super) fn parse_decision(
    output: &Value,
    question: &WorkerQuestion,
) -> Result<Decision, String> {
    use super::auto_review::{escalation_options, nonempty, only};
    let raw: RawDecision = serde_json::from_value(output.clone())
        .map_err(|error| format!("the output does not match the schema: {error}"))?;
    let action = raw.action.as_str();
    let actions = allowed_actions(question);
    if !actions.contains(&action) {
        return Err(format!(
            "action {action:?} is not one this request takes ({})",
            actions.join(", ")
        ));
    }
    match action {
        "allow" => {
            only(
                action,
                &[
                    ("message", raw.message.is_some()),
                    ("answers", raw.answers.is_some()),
                    ("question", raw.question.is_some()),
                    ("options", raw.options.is_some()),
                ],
            )?;
            Ok(Decision::Allow)
        }
        "deny" => {
            only(
                action,
                &[
                    ("answers", raw.answers.is_some()),
                    ("question", raw.question.is_some()),
                    ("options", raw.options.is_some()),
                ],
            )?;
            Ok(Decision::Deny {
                message: nonempty(action, raw.message.clone(), "message")?,
            })
        }
        "answer" => {
            only(
                action,
                &[
                    ("message", raw.message.is_some()),
                    ("question", raw.question.is_some()),
                    ("options", raw.options.is_some()),
                ],
            )?;
            let answers: Vec<String> = raw
                .answers
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|answer| answer.trim().to_owned())
                .collect();
            if answers.len() != question.questions.len() || answers.iter().any(String::is_empty) {
                return Err(format!(
                    "answer needs one nonempty answer per question ({})",
                    question.questions.len()
                ));
            }
            Ok(Decision::Answer { answers })
        }
        _ => {
            only(
                action,
                &[
                    ("message", raw.message.is_some()),
                    ("answers", raw.answers.is_some()),
                ],
            )?;
            let text = nonempty(action, raw.question.clone(), "question")?;
            let options = escalation_options(raw.options.clone())?;
            Ok(Decision::Escalate {
                question: text,
                options,
            })
        }
    }
}

/// Whether the server answers the run's pending event itself: an
/// automatic run waiting on a `question` event that no escalation raised.
/// An escalated one goes on once its questions are settled
/// ([`WorkerSupervisor::auto_answer_due`]).
pub(super) fn is_due(run: &Run, seq: i64, body: &Value) -> bool {
    run.finish.auto_answer
        && run.info.status == TodoRunStatus::Waiting
        && run.info.pending_event == Some(seq)
        && body["kind"] == json!(TodoEventKind::Question)
        && body.get(ESCALATES).is_none()
}

/// A decision as `run_answer_decision` records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DecisionRecord {
    decision_id: String,
    /// The `question` event and the question decided.
    event: i64,
    request_id: String,
    /// SHA-256 of the input the model got, in hex; empty when the policy
    /// left no choice but to escalate and no model was asked.
    input_digest: String,
    model: Option<String>,
    /// The checked decision; none when every call failed.
    output: Option<Value>,
    /// Each failed call's error.
    #[serde(default)]
    errors: Vec<String>,
}

/// What became of one question.
enum Settled {
    /// Answered (by this decision, or elsewhere first).
    Done(Value),
    /// It goes to the user: its decision, the question and its options.
    Escalate(String, String, Vec<String>),
}

impl WorkerSupervisor {
    /// The run's pending `question` event, with its body, when the server
    /// answers it itself: one no escalation raised, or an escalated one
    /// whose questions the user (or anyone) answered since.
    pub(super) fn auto_answer_due(&self, run: &Run) -> Option<(i64, Value)> {
        if !run.finish.auto_answer || run.info.status != TodoRunStatus::Waiting {
            return None;
        }
        let (seq, body) = self
            .run_store()
            .ok()?
            .latest_run_event(&run.info.run_id)
            .ok()??;
        if is_due(run, seq, &body) {
            return Some((seq, body));
        }
        if run.info.pending_event != Some(seq)
            || body["kind"] != json!(TodoEventKind::Question)
            || body.get(ESCALATES).is_none()
        {
            return None;
        }
        let worker = self.status(run.info.worker_id.as_deref()?).ok()?;
        let open = pending_questions(&worker.questions);
        let event = event_of(seq, &body, &run.info);
        let settled = event.questions.iter().all(|asked| {
            !open
                .iter()
                .any(|question| question.request_id == asked.request_id)
        });
        settled.then_some((seq, body))
    }

    /// Answers the run's pending `question` event `seq`: each question
    /// still pending by its recorded decision, else the model's, recorded
    /// first; then the run goes back to its attention step, or raises an
    /// escalated `question` event for the questions that go to the user.
    pub(super) fn step_auto_answer(
        &self,
        run: &mut Run,
        seq: i64,
        body: &Value,
    ) -> Result<(), String> {
        let event = event_of(seq, body, &run.info);
        let worker_id = run
            .info
            .worker_id
            .clone()
            .ok_or("the run has no worker to answer")?;
        let escalated = body.get(ESCALATES).is_some();
        let open = self
            .status(&worker_id)
            .map(|worker| pending_questions(&worker.questions))
            .map_err(|error| error.to_string())?;
        let mut settled = Vec::new();
        let mut to_user = Vec::new();
        for question in &event.questions {
            let still_open = open
                .iter()
                .find(|open| open.request_id == question.request_id);
            let Some(question) = still_open.filter(|_| !escalated) else {
                settled.push(json!({"request_id": question.request_id, "answered": "elsewhere"}));
                continue;
            };
            match self.settle_question(run, seq, &worker_id, question)? {
                Settled::Done(how) => settled.push(how),
                Settled::Escalate(decision_id, text, options) => {
                    to_user.push((question.clone(), decision_id, text, options));
                }
            }
            if self.handed_off() {
                return Ok(());
            }
        }
        if to_user.is_empty() {
            return self.answered(run, seq, &worker_id, &settled);
        }
        self.escalate_questions(run, seq, &worker_id, &to_user)
    }

    /// Decides one question (the recorded decision, else the policy's or
    /// the model's) and sends the answer, unless it escalates.
    fn settle_question(
        &self,
        run: &Run,
        seq: i64,
        worker_id: &str,
        question: &WorkerQuestion,
    ) -> Result<Settled, String> {
        let store = self.run_store().map_err(|error| error.to_string())?;
        let recorded = store
            .run_events_of(&run.info.run_id, DECISION)
            .map_err(|error| format!("the worker store failed: {error}"))?
            .into_iter()
            .filter_map(|body| serde_json::from_value::<DecisionRecord>(body).ok())
            .find(|record| record.event == seq && record.request_id == question.request_id);
        let record = match recorded {
            Some(record) => record,
            None => {
                let record = self.decide_question(run, seq, worker_id, question)?;
                #[cfg(test)]
                super::crashes_after(&run.info.repo, TodoStep::Attention)?;
                record
            }
        };
        let decision = match record
            .output
            .as_ref()
            .map(|output| parse_decision(output, question))
            .transpose()
        {
            Ok(Some(decision)) => decision,
            Ok(None) => {
                return Ok(Settled::Escalate(
                    record.decision_id.clone(),
                    format!(
                        "The automatic answer failed {} times ({}); answer the worker's request \
                         yourself.",
                        record.errors.len(),
                        record.errors.join("; ")
                    ),
                    answer_options(question),
                ))
            }
            Err(error) => {
                return Ok(Settled::Escalate(
                    record.decision_id.clone(),
                    format!(
                        "The recorded automatic answer is unreadable ({error}); answer the \
                         worker's request yourself."
                    ),
                    answer_options(question),
                ))
            }
        };
        let (decision_kind, answers, message) = match &decision {
            Decision::Escalate {
                question: text,
                options,
            } => {
                return Ok(Settled::Escalate(
                    record.decision_id.clone(),
                    text.clone(),
                    options.clone(),
                ))
            }
            Decision::Allow => (WorkerDecision::Allow, Vec::new(), None),
            Decision::Deny { message } => (WorkerDecision::Deny, Vec::new(), Some(message.clone())),
            Decision::Answer { answers } => (WorkerDecision::Allow, answers.clone(), None),
        };
        let params = WorkerAnswerParams {
            worker_id: worker_id.to_owned(),
            request_id: Some(question.request_id.clone()),
            decision: Some(decision_kind),
            answers,
            message,
            command_id: Some(format!(
                "{}:{}:auto-answer",
                run.info.run_id, record.decision_id
            )),
        };
        let how = match self.answer(&params) {
            Ok(_) => "answered",
            // Answered elsewhere first (the user's `?` list), withdrawn, or
            // its worker ended: nothing left to answer.
            Err(WorkerError::QuestionGone(_) | WorkerError::NoQuestion(_)) => "gone",
            Err(WorkerError::NotRunning(_)) => "worker ended",
            Err(error) => return Err(format!("answering {}: {error}", question.request_id)),
        };
        Ok(Settled::Done(json!({
            "request_id": question.request_id, "decision_id": record.decision_id,
            "decision": decision.to_json(), "answered": how,
        })))
    }

    /// Asks the model about one question (at most [`MAX_CALLS`] calls,
    /// counting those a previous server made, taking a recorded answer to
    /// the same input without asking again), or decides by the policy alone
    /// when the request may only be escalated; records the decision.
    fn decide_question(
        &self,
        run: &Run,
        seq: i64,
        worker_id: &str,
        question: &WorkerQuestion,
    ) -> Result<DecisionRecord, String> {
        let actions = allowed_actions(question);
        let key = format!("{seq}:{}", question.request_id);
        let record = if actions == ["escalate"] {
            let output = Decision::Escalate {
                question: format!(
                    "The worker of run {} (item {}) asks to use {}: {}. Herdr's policy leaves \
                     this to you ({}). Allow it?",
                    run.info.run_id,
                    run.info.item,
                    question.tool_name,
                    question.text,
                    question
                        .reason
                        .as_deref()
                        .unwrap_or("not decided by the policy")
                ),
                options: vec!["deny".into(), "allow".into()],
            };
            DecisionRecord {
                decision_id: decision::decision_id(&run.info.run_id, &key, ""),
                event: seq,
                request_id: question.request_id.clone(),
                input_digest: String::new(),
                model: None,
                output: Some(output.to_json()),
                errors: Vec::new(),
            }
        } else {
            let input = self.answer_input(run, worker_id, question, &actions);
            let text = serde_json::to_string_pretty(&input).map_err(|error| error.to_string())?;
            let digest = decision::digest(&text);
            let schema = output_schema(&actions);
            let prompt = Prompt {
                schema: &schema,
                system: SYSTEM_PROMPT,
                intro: "Decide this request and answer with the JSON decision only.",
            };
            let store = self.run_store().map_err(|error| error.to_string())?;
            let calls = store
                .run_events_of(&run.info.run_id, CALL)
                .map_err(|error| format!("the worker store failed: {error}"))?;
            let (mut errors, answered) =
                decision::recorded_calls(&calls, ("key", &json!(key)), &digest);
            let mut decided = answered.and_then(|(output, model)| {
                parse_decision(&output, question)
                    .ok()
                    .map(|decision| (decision, model))
            });
            while decided.is_none() && errors.len() < MAX_CALLS {
                let call = errors.len() + 1;
                let answered = self
                    .call_model(&prompt, &text)
                    .and_then(|(output, model)| Ok((parse_decision(&output, question)?, model)));
                let note = match &answered {
                    Ok((decision, model)) => json!({
                        "type": CALL, "key": key, "event": seq, "call": call,
                        "input_digest": digest, "model": model, "output": decision.to_json(),
                    }),
                    Err(error) => json!({
                        "type": CALL, "key": key, "event": seq, "call": call,
                        "input_digest": digest, "error": error,
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
            DecisionRecord {
                decision_id: decision::decision_id(&run.info.run_id, &key, &digest),
                event: seq,
                request_id: question.request_id.clone(),
                input_digest: digest,
                model,
                output,
                errors,
            }
        };
        let mut body = serde_json::to_value(&record).map_err(|error| error.to_string())?;
        body["type"] = json!(DECISION);
        self.write_note(&run.info.run_id, &body)?;
        Ok(record)
    }

    /// What the model decides on: the item, the task, the contract, the
    /// request with the tool's whole input, and the run's policy.
    fn answer_input(
        &self,
        run: &Run,
        worker_id: &str,
        question: &WorkerQuestion,
        actions: &[&str],
    ) -> Value {
        let item_text = self
            .run_store()
            .ok()
            .and_then(|store| store.run_events_of(&run.info.run_id, "run_created").ok())
            .and_then(|created| created.into_iter().next())
            .map(|created| created["item_text"].clone())
            .unwrap_or(Value::Null);
        let detail = self.question_detail(worker_id, &question.request_id).ok();
        json!({
            "item": {"id": run.info.item, "text": item_text},
            "task": run.info.task,
            "commit_subject": run.info.message,
            "allowed_paths": run.info.paths,
            "request": {
                "tool": question.tool_name,
                "kind": question.kind,
                "text": question.text,
                "input": detail.as_ref().map(|detail| detail.input_text.clone()),
                "questions": question.questions,
                "why_the_policy_left_it": question.reason,
            },
            "policy": {
                "worker_directory": detail.as_ref().map(|detail| detail.cwd.clone()),
                "rules": "Bash runs in a sandbox that writes only to the worker's directory and \
                          its temp dir and reaches no credentials; file tools may use only those \
                          directories; environment files are never read; nothing may wait in \
                          the background; a classifier escalation, a path outside those \
                          directories and a tool herdr does not know go to the user.",
                "actions": actions,
            },
        })
    }

    /// The run's questions are settled: it goes back to its attention step,
    /// while it still waits on `seq`; else the decisions are recorded as
    /// superseded (a coordinator answered first).
    fn answered(
        &self,
        run: &mut Run,
        seq: i64,
        worker_id: &str,
        settled: &[Value],
    ) -> Result<(), String> {
        let body = json!({"type": APPLIED, "event": seq, "questions": settled});
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
                current.info.step = TodoStep::Attention;
                tx.run_event(&mut current, &body, false, now_ms())?;
                Ok(Some(current))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let Some(current) = applied else {
            return self.write_note(
                &run.info.run_id,
                &json!({
                    "type": SUPERSEDED, "event": seq, "questions": settled,
                    "note": "the run no longer waits on the question event: the coordinator answered it first",
                }),
            );
        };
        *run = current;
        super::announce();
        // Its owner owes nothing more for the worker's events up to here.
        if let Some(seq) = run.info.last_acked_seq {
            self.ack_quietly(worker_id, seq);
        }
        Ok(())
    }

    /// Hands the questions to the user (each one escalated on the worker
    /// with the model's question and options as its cause, which the `?`
    /// list shows) and raises a `question` event with them for the
    /// coordinator, which the run waits on, and a notice to the user.
    fn escalate_questions(
        &self,
        run: &mut Run,
        seq: i64,
        worker_id: &str,
        to_user: &[(WorkerQuestion, String, String, Vec<String>)],
    ) -> Result<(), String> {
        for (question, decision_id, text, options) in to_user {
            let cause = format!(
                "run {} (item {}), decision {decision_id}: {text} Options: {}",
                run.info.run_id,
                run.info.item,
                options.join(" | ")
            );
            match self.escalate_because(worker_id, &question.request_id, &cause) {
                Ok(_) | Err(WorkerError::QuestionGone(_) | WorkerError::NoQuestion(_)) => {}
                Err(error) => return Err(format!("escalating {}: {error}", question.request_id)),
            }
        }
        let mut event = new_event(TodoEventKind::Question);
        event.questions = to_user
            .iter()
            .map(|(question, ..)| question.clone())
            .collect();
        event.error = Some(
            to_user
                .iter()
                .map(|(question, decision_id, text, options)| {
                    format!(
                        "The automatic answer ({decision_id}) to {} asks: {text}\nOptions: {}",
                        question.request_id,
                        options.join(" | ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let decisions: Vec<&str> = to_user.iter().map(|(_, id, ..)| id.as_str()).collect();
        let store = self.run_store().map_err(|error| error.to_string())?;
        let raised = store
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
                    "asked_event": seq,
                });
                body[ESCALATES] = json!(decisions);
                tx.run_event(&mut current, &body, true, now_ms())?;
                Ok(Some(current))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let Some(current) = raised else {
            return self.write_note(
                &run.info.run_id,
                &json!({
                    "type": SUPERSEDED, "event": seq, "escalated": decisions,
                    "note": "the run no longer waits on the question event: the coordinator answered it first",
                }),
            );
        };
        *run = current;
        super::announce();
        let (_, _, first, options) = &to_user[0];
        crate::workers::notify_user(crate::workers::UserNotice {
            title: format!(
                "{}: the worker's request needs you (run {})",
                run.info.item, run.info.run_id
            ),
            body: format!("{first}\nOptions: {}", options.join(" | ")),
        });
        Ok(())
    }

    /// Wakes the driver of an automatic run whose escalated question of
    /// `worker_id` was just answered, so the run goes on.
    pub(crate) fn question_settled(&self, worker_id: &str) {
        let Ok(store) = self.run_store() else {
            return;
        };
        let Ok(runs) = store.runs(None) else {
            return;
        };
        for run in runs {
            if run.finish.auto_answer
                && run.info.status == TodoRunStatus::Waiting
                && run.info.worker_id.as_deref() == Some(worker_id)
            {
                #[cfg(test)]
                if settles_inline(&run.info.repo) {
                    self.drive(&run.info.run_id);
                    continue;
                }
                self.spawn_driver(&run.info.run_id);
            }
        }
    }
}

/// Test only: the repositories whose runs [`WorkerSupervisor::question_settled`]
/// drives in the answering thread instead of a new one, so the driver it
/// wakes always gets to the run before the answering call goes on.
#[cfg(test)]
static SETTLE_INLINE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(all(test, unix))]
pub(crate) fn settle_inline(repo: &str) {
    super::lock(&SETTLE_INLINE).push(repo.to_owned());
}

#[cfg(test)]
fn settles_inline(repo: &str) -> bool {
    super::lock(&SETTLE_INLINE).iter().any(|at| at == repo)
}

/// The choices an answer the model could not settle leaves the user.
fn answer_options(question: &WorkerQuestion) -> Vec<String> {
    match question.kind {
        WorkerQuestionKind::Choice => vec!["answer it".into(), "deny it".into()],
        _ => vec!["deny".into(), "allow".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{WorkerChoiceQuestion, WorkerQuestionState};

    fn question(tool: &str, reason: &str) -> WorkerQuestion {
        WorkerQuestion {
            request_id: "perm-1".into(),
            kind: if tool == "AskUserQuestion" {
                WorkerQuestionKind::Choice
            } else {
                WorkerQuestionKind::Approval
            },
            tool_name: tool.into(),
            text: "x".into(),
            reason: Some(reason.into()),
            questions: if tool == "AskUserQuestion" {
                vec![WorkerChoiceQuestion {
                    question: "Which?".into(),
                    header: None,
                    options: vec!["a".into(), "b".into()],
                    multi_select: false,
                }]
            } else {
                Vec::new()
            },
            since_ms: 0,
            state: WorkerQuestionState::Pending,
            escalated: None,
        }
    }

    #[test]
    fn a_request_the_policy_leaves_to_the_user_may_only_be_escalated() {
        let classifier = question("WebFetch", policy::CLASSIFIER_ASK);
        assert_eq!(allowed_actions(&classifier), ["escalate"]);
        let unknown = question("Skill", "Skill is not decided by herdr's policy");
        assert_eq!(allowed_actions(&unknown), ["escalate"]);
        let outside = question("Bash", "a Bash command that names /etc");
        assert_eq!(allowed_actions(&outside), ["escalate"]);
        // The schema itself forbids allow.
        let schema: Value =
            serde_json::from_str(&output_schema(&allowed_actions(&unknown))).unwrap();
        assert_eq!(schema["properties"]["action"]["enum"], json!(["escalate"]));
        assert!(parse_decision(&json!({"action": "allow"}), &unknown).is_err());
        let fetch = question("WebFetch", "WebFetch is not decided by herdr's policy");
        assert_eq!(allowed_actions(&fetch), ["allow", "deny", "escalate"]);
        assert_eq!(
            parse_decision(&json!({"action": "allow"}), &fetch),
            Ok(Decision::Allow)
        );
        let ask = question("AskUserQuestion", "a question for the user");
        assert_eq!(allowed_actions(&ask), ["answer", "deny", "escalate"]);
        assert!(parse_decision(&json!({"action": "allow"}), &ask).is_err());
    }

    #[test]
    fn each_action_takes_its_own_fields_only() {
        let fetch = question("WebFetch", "WebFetch is not decided by herdr's policy");
        let ask = question("AskUserQuestion", "a question for the user");
        assert_eq!(
            parse_decision(&json!({"action": "deny", "message": " no "}), &fetch),
            Ok(Decision::Deny {
                message: "no".into()
            })
        );
        assert_eq!(
            parse_decision(&json!({"action": "answer", "answers": ["a"]}), &ask),
            Ok(Decision::Answer {
                answers: vec!["a".into()]
            })
        );
        assert_eq!(
            parse_decision(
                &json!({"action": "escalate", "question": "q?", "options": ["a", "b"]}),
                &fetch
            ),
            Ok(Decision::Escalate {
                question: "q?".into(),
                options: vec!["a".into(), "b".into()]
            })
        );
        for (bad, of) in [
            (json!({"action": "deny"}), &fetch),
            (json!({"action": "allow", "message": "x"}), &fetch),
            (json!({"action": "answer", "answers": []}), &ask),
            (json!({"action": "answer", "answers": ["a", "b"]}), &ask),
            (json!({"action": "answer", "answers": [" "]}), &ask),
            (
                json!({"action": "escalate", "question": "q?", "options": ["a"]}),
                &fetch,
            ),
            (json!({"action": "merge"}), &fetch),
            (json!({"action": "allow", "extra": 1}), &fetch),
        ] {
            assert!(parse_decision(&bad, of).is_err(), "{bad}");
        }
    }
}
