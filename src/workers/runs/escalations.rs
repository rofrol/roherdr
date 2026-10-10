//! A review escalation in the user's `?` list. The automatic review's
//! `escalate` raises a new `review` event for the coordinator
//! ([`super::auto_review`]); it is also listed to the user as a question
//! of the run's worker with the model's question and options, which the
//! answer dialog shows and answers like an `AskUserQuestion`
//! (`worker.question`, `worker.answer`). Its request id
//! ([`REQUEST_PREFIX`]) tells it apart from a question of the worker
//! itself, which the escalations of `answer_question` hand to the user
//! instead ([`super::auto_answer`]).
//!
//! The user's answer raises a new `review` event carrying it, which the
//! server reviews again with the answer in the model's input; a denial
//! takes the escalation off the list and leaves the event to the
//! coordinator. Either is taken only while the run still waits on the
//! escalated event; `todo.resume` answering it first takes it off the list.
//!
//! The list is kept in memory, read for every client snapshot without
//! touching the store, and rebuilt from the runs' events when the server
//! starts ([`WorkerSupervisor::restore_escalations`]).

use std::sync::Mutex;

use serde_json::{json, Value};

use super::{event_of, lock, new_event, Run};
use crate::api::schema::{
    TodoEventKind, TodoRunStatus, WorkerChoiceQuestion, WorkerDecision, WorkerQuestion,
    WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionState, WorkerState,
};
use crate::workers::{now_ms, PendingWorkerQuestion, WorkerError, WorkerSupervisor};

/// The request id of an escalation: this, the run's id, `-` and the
/// escalated event's seq.
pub(crate) const REQUEST_PREFIX: &str = "herdr-review-";

/// The note a denial from the `?` list records.
const DECLINED: &str = "run_escalation_declined";
/// The note the user's answer records, with the event it raised.
const ANSWERED: &str = "run_escalation_answered";

/// Whether `request_id` names an escalation, not a worker's own question.
pub(crate) fn is_escalation(request_id: &str) -> bool {
    request_id.starts_with(REQUEST_PREFIX)
}

/// One escalation the user can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Escalation {
    pub(super) run_id: String,
    pub(super) item: String,
    pub(super) repo: String,
    pub(super) worker_id: String,
    pub(super) owner_pane: Option<String>,
    pub(super) owner_coordinator: Option<String>,
    /// The `review` event the escalation raised, which the run waits on.
    pub(super) event: i64,
    pub(super) decision_id: String,
    pub(super) question: String,
    pub(super) options: Vec<String>,
    pub(super) since_ms: u64,
}

impl Escalation {
    fn request_id(&self) -> String {
        format!("{REQUEST_PREFIX}{}-{}", self.run_id, self.event)
    }

    fn question(&self) -> WorkerQuestion {
        WorkerQuestion {
            request_id: self.request_id(),
            kind: WorkerQuestionKind::Choice,
            tool_name: "herdr review".into(),
            text: format!("run {} · {}: {}", self.run_id, self.item, self.question),
            reason: Some(format!(
                "the automatic review {} of run {} (item {}) asks you",
                self.decision_id, self.run_id, self.item
            )),
            questions: vec![WorkerChoiceQuestion {
                question: self.question.clone(),
                header: Some(format!("Review of {}", self.item)),
                options: self.options.clone(),
                multi_select: false,
            }],
            since_ms: self.since_ms,
            state: WorkerQuestionState::Pending,
            escalated: Some(format!(
                "the automatic review {} of run {} (item {}) asks you; your answer goes back to \
                 the review, Deny leaves the attempt to the coordinator",
                self.decision_id, self.run_id, self.item
            )),
        }
    }
}

static ESCALATIONS: Mutex<Vec<Escalation>> = Mutex::new(Vec::new());

/// Lists the escalation (again, replacing one of the same event).
pub(super) fn list(escalation: Escalation) {
    {
        let mut listed = lock(&ESCALATIONS);
        listed.retain(|known| known.run_id != escalation.run_id || known.event != escalation.event);
        listed.push(escalation);
    }
    crate::workers::notify_clients();
}

/// Takes off the list the run's escalations whose event the run no longer
/// waits on.
pub(super) fn settle(run: &Run) {
    let mut listed = lock(&ESCALATIONS);
    let before = listed.len();
    listed.retain(|known| {
        known.run_id != run.info.run_id
            || (run.info.status == TodoRunStatus::Waiting
                && run.info.pending_event == Some(known.event))
    });
    if listed.len() != before {
        drop(listed);
        crate::workers::notify_clients();
    }
}

/// Test only: empties the list, as a server that starts has it.
#[cfg(all(test, unix))]
pub(crate) fn forget_all_for_test() {
    lock(&ESCALATIONS).clear();
}

fn take(run_id: &str, event: i64) {
    lock(&ESCALATIONS).retain(|known| known.run_id != run_id || known.event != event);
    crate::workers::notify_clients();
}

/// The listed escalations as the `?` list shows a worker's questions.
pub(crate) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    lock(&ESCALATIONS)
        .iter()
        .map(|escalation| PendingWorkerQuestion {
            worker_id: escalation.worker_id.clone(),
            cwd: escalation.repo.clone(),
            question: escalation.question(),
            quiet: false,
            owner_seen_ms: None,
        })
        .collect()
}

fn find(worker_id: &str, request_id: &str) -> Result<Escalation, WorkerError> {
    lock(&ESCALATIONS)
        .iter()
        .find(|known| known.worker_id == worker_id && known.request_id() == request_id)
        .cloned()
        .ok_or_else(|| {
            WorkerError::QuestionGone(format!(
                "question {request_id} of worker {worker_id} is no longer pending: the run no \
                 longer waits on that review"
            ))
        })
}

/// The answer an escalation takes: an option's label, its 1-based number,
/// or free text; one answer, for its one question.
fn chosen(escalation: &Escalation, answers: &[String]) -> Result<String, WorkerError> {
    let [answer] = answers else {
        return Err(WorkerError::Invalid(format!(
            "the review's question takes one answer, got {}",
            answers.len()
        )));
    };
    let answer = answer.trim();
    if answer.is_empty() {
        return Err(WorkerError::Invalid("the answer is empty".into()));
    }
    Ok(answer
        .parse::<usize>()
        .ok()
        .and_then(|number| number.checked_sub(1))
        .and_then(|index| escalation.options.get(index))
        .cloned()
        .unwrap_or_else(|| answer.to_owned()))
}

impl WorkerSupervisor {
    /// The escalation `request_id` names, as the answer dialog shows it.
    pub(crate) fn escalation_detail(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerQuestionDetail, WorkerError> {
        let escalation = find(worker_id, request_id)?;
        let state = self
            .status(worker_id)
            .map(|worker| worker.state)
            .unwrap_or(WorkerState::Exited);
        let mut input = vec![
            format!(
                "Run {} of item {} asks you, through its automatic review ({}):",
                escalation.run_id, escalation.item, escalation.decision_id
            ),
            String::new(),
            escalation.question.clone(),
            String::new(),
        ];
        input.extend(
            escalation
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| format!("{}. {option}", index + 1)),
        );
        if let Ok(Some((seq, body))) = self.run_store().and_then(|store| {
            store
                .latest_run_event(&escalation.run_id)
                .map_err(super::store_error)
        }) {
            if seq == escalation.event {
                let run = self.load_run(&escalation.run_id)?;
                let event = event_of(seq, &body, &run.info);
                if let Some(stat) = event.diff_stat {
                    input.extend([String::new(), stat]);
                }
                if let Some(text) = event.result_text {
                    input.extend([String::new(), "The worker's last reply:".into(), text]);
                }
            }
        }
        Ok(WorkerQuestionDetail {
            worker_id: escalation.worker_id.clone(),
            name: format!("run {} · {}", escalation.run_id, escalation.item),
            cwd: escalation.repo.clone(),
            state,
            question: escalation.question(),
            input_text: input.join("\n"),
            owner_pane_id: escalation.owner_pane.clone(),
            owner_coordinator_id: escalation.owner_coordinator.clone(),
            quiet: false,
        })
    }

    /// The user's answer to an escalation from the `?` list: `deny` takes
    /// it off the list and leaves the event to the coordinator; an answer
    /// raises a new `review` event carrying it, which the server reviews
    /// again. Refused as gone once the run no longer waits on the event.
    pub(crate) fn answer_escalation(
        &self,
        worker_id: &str,
        request_id: &str,
        decision: Option<WorkerDecision>,
        answers: &[String],
        message: Option<&str>,
    ) -> Result<(), WorkerError> {
        let escalation = find(worker_id, request_id)?;
        let store = self.run_store()?;
        if decision == Some(WorkerDecision::Deny) {
            let note = json!({
                "type": DECLINED, "event": escalation.event,
                "decision_id": escalation.decision_id, "message": message,
            });
            store
                .transaction(|tx| tx.run_note(&escalation.run_id, &note, now_ms()))
                .map_err(super::store_error)?;
            take(&escalation.run_id, escalation.event);
            super::announce();
            return Ok(());
        }
        let answer = chosen(&escalation, answers)?;
        let latest = store
            .latest_run_event(&escalation.run_id)
            .map_err(super::store_error)?;
        let escalated = latest.filter(|(seq, _)| *seq == escalation.event);
        let raised = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&escalation.run_id)? else {
                    return Ok(None);
                };
                let Some((seq, body)) = escalated.as_ref() else {
                    return Ok(None);
                };
                if current.info.status != TodoRunStatus::Waiting
                    || current.info.pending_event != Some(*seq)
                {
                    return Ok(None);
                }
                let shown = event_of(*seq, body, &current.info);
                let mut event = new_event(TodoEventKind::Review);
                event.diff_stat = shown.diff_stat;
                event.commits = shown.commits;
                event.result_text = shown.result_text;
                event.error = Some(format!(
                    "The user answered the automatic review's question ({}): {answer}",
                    escalation.decision_id
                ));
                let user_answer = json!({
                    "decision_id": escalation.decision_id,
                    "question": escalation.question,
                    "options": escalation.options,
                    "answer": answer,
                });
                let body = json!({
                    "type": "run_event",
                    "kind": event.kind,
                    "step": current.info.step,
                    "status": current.info.status,
                    "attempt": current.info.attempt,
                    "event": event,
                    "answers_event": seq,
                    "user_answer": user_answer,
                });
                let raised = tx.run_event(&mut current, &body, true, now_ms())?;
                tx.run_note(
                    &current.info.run_id,
                    &json!({
                        "type": ANSWERED, "event": seq, "raised": raised,
                        "decision_id": escalation.decision_id, "answer": answer,
                    }),
                    now_ms(),
                )?;
                Ok(Some(current))
            })
            .map_err(super::store_error)?;
        take(&escalation.run_id, escalation.event);
        if raised.is_none() {
            return Err(WorkerError::QuestionGone(format!(
                "question {request_id} of worker {worker_id} is no longer pending: the run no \
                 longer waits on that review"
            )));
        }
        super::announce();
        self.spawn_driver(&escalation.run_id);
        Ok(())
    }

    /// Lists again, as a server that starts does, each run's escalation it
    /// still waits on that the user has not declined.
    pub(super) fn restore_escalations(&self, runs: &[Run]) {
        let Ok(store) = self.run_store() else {
            return;
        };
        for run in runs {
            if run.info.status != TodoRunStatus::Waiting {
                continue;
            }
            let Ok(Some((seq, body))) = store.latest_run_event(&run.info.run_id) else {
                continue;
            };
            if run.info.pending_event != Some(seq) {
                continue;
            }
            let declined = store
                .run_events_of(&run.info.run_id, DECLINED)
                .unwrap_or_default()
                .iter()
                .any(|note| note["event"] == json!(seq));
            if !declined {
                if let Some(escalation) = escalation_of(run, seq, &body) {
                    list(escalation);
                }
            }
        }
    }
}

/// The escalation a run's `review` event raised, when it is one with a
/// worker to list it under.
pub(super) fn escalation_of(run: &Run, seq: i64, body: &Value) -> Option<Escalation> {
    if body["kind"] != json!(TodoEventKind::Review) {
        return None;
    }
    let decision_id = body[super::auto_review::ESCALATES].as_str()?.to_owned();
    let question = body["question"].as_str()?.to_owned();
    let options = body["options"]
        .as_array()?
        .iter()
        .filter_map(|option| option.as_str().map(str::to_owned))
        .collect();
    Some(Escalation {
        run_id: run.info.run_id.clone(),
        item: run.info.item.clone(),
        repo: run.info.repo.clone(),
        worker_id: run.info.worker_id.clone()?,
        owner_pane: run.owner_pane.clone(),
        owner_coordinator: run.owner_coordinator.clone(),
        event: seq,
        decision_id,
        question,
        options,
        since_ms: run.info.updated_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escalation() -> Escalation {
        Escalation {
            run_id: "r-abcdefgh".into(),
            item: "t-abcd2345".into(),
            repo: "/repo".into(),
            worker_id: "w1".into(),
            owner_pane: None,
            owner_coordinator: None,
            event: 7,
            decision_id: "d-abcdefgh".into(),
            question: "Keep b.txt?".into(),
            options: vec!["keep".into(), "drop".into()],
            since_ms: 1,
        }
    }

    #[test]
    fn an_answer_is_a_label_a_number_or_free_text() {
        let escalation = escalation();
        assert_eq!(chosen(&escalation, &["2".into()]).unwrap(), "drop");
        assert_eq!(chosen(&escalation, &["keep".into()]).unwrap(), "keep");
        assert_eq!(
            chosen(&escalation, &[" keep it, but rename ".into()]).unwrap(),
            "keep it, but rename"
        );
        assert_eq!(chosen(&escalation, &["9".into()]).unwrap(), "9");
        assert!(chosen(&escalation, &[]).is_err());
        assert!(chosen(&escalation, &[" ".into()]).is_err());
        assert!(chosen(&escalation, &["1".into(), "2".into()]).is_err());
    }

    #[test]
    fn an_escalation_is_a_choice_question_with_the_run_and_the_item() {
        let question = escalation().question();
        assert!(is_escalation(&question.request_id));
        assert_eq!(question.kind, WorkerQuestionKind::Choice);
        assert_eq!(question.questions[0].options, ["keep", "drop"]);
        for part in ["r-abcdefgh", "t-abcd2345", "Keep b.txt?"] {
            assert!(
                question.text.contains(part),
                "{part:?} not in {}",
                question.text
            );
        }
        assert!(question.escalated.unwrap().contains("d-abcdefgh"));
    }
}
