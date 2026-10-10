//! `herdr todo review`: one attempt of a run in the view the coordinator
//! reviews it in, joined from what is already kept: the run's `attempts`
//! row (its task, decision, review and verdict) and `landings`, the
//! attempt's worker journal (its transcript: the task it got, its last
//! reply, its questions with their answers and the tool calls that failed
//! or were denied) and `git diff base commit` in the run's repository. No
//! table of its own.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use super::log::{tool_input, tool_result_text, user_text};
use super::runs::{git, store_error, APPROVE};
use super::{one_line, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    TodoApproval, TodoReview, TodoReviewQuestion, TodoReviewToolFailure, TodoToolFailureKind,
    WorkerVerification,
};

/// A failed call's error is cut to this many characters.
const DETAIL_MAX: usize = 300;

/// What the review reads from a worker's journal.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Transcript {
    /// The first prompt the worker got.
    pub(super) task: Option<String>,
    /// The last turn's reply.
    pub(super) final_message: Option<String>,
    pub(super) questions: Vec<TodoReviewQuestion>,
    pub(super) tool_failures: Vec<TodoReviewToolFailure>,
}

/// The journal's records (one JSON object per line) read into a
/// [`Transcript`]. A line that is not a record is skipped: `herdr worker
/// log` shows it as it is.
pub(super) fn transcript<'a>(lines: impl IntoIterator<Item = &'a str>) -> Transcript {
    let mut transcript = Transcript::default();
    // Each tool call by its id: its name and most telling input.
    let mut calls: HashMap<String, (String, String)> = HashMap::new();
    // The calls already listed as denied, whose error result says the same.
    let mut denied: HashSet<String> = HashSet::new();
    let deny = |transcript: &mut Transcript,
                denied: &mut HashSet<String>,
                calls: &HashMap<String, (String, String)>,
                id: Option<&str>,
                tool_name: Option<&str>,
                input: Option<String>,
                detail: String| {
        if let Some(id) = id {
            if !denied.insert(id.to_owned()) {
                return;
            }
        }
        let call = id.and_then(|id| calls.get(id));
        transcript.tool_failures.push(TodoReviewToolFailure {
            kind: TodoToolFailureKind::Denied,
            tool_name: tool_name
                .map(str::to_owned)
                .or_else(|| call.map(|(name, _)| name.clone()))
                .unwrap_or_else(|| "?".to_owned()),
            input: input.or_else(|| call.map(|(_, input)| input.clone())),
            detail,
        });
    };
    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event = &record["event"];
        let kind = event["type"].as_str().unwrap_or("");
        match (record["dir"].as_str().unwrap_or(""), kind) {
            ("in", "user") if transcript.task.is_none() => transcript.task = user_text(event),
            ("out", "assistant") => {
                for block in event["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] != "tool_use" {
                        continue;
                    }
                    if let Some(id) = block["id"].as_str() {
                        calls.insert(
                            id.to_owned(),
                            (
                                block["name"].as_str().unwrap_or("tool").to_owned(),
                                tool_input(&block["input"]),
                            ),
                        );
                    }
                }
            }
            ("out", "user") => {
                for block in event["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] != "tool_result" || block["is_error"] != true {
                        continue;
                    }
                    let id = block["tool_use_id"].as_str();
                    if id.is_some_and(|id| denied.contains(id)) {
                        continue;
                    }
                    let call = id.and_then(|id| calls.get(id));
                    transcript.tool_failures.push(TodoReviewToolFailure {
                        kind: TodoToolFailureKind::Failed,
                        tool_name: call.map_or_else(|| "?".to_owned(), |(name, _)| name.clone()),
                        input: call.map(|(_, input)| input.clone()),
                        detail: one_line(&tool_result_text(&block["content"]), DETAIL_MAX),
                    });
                }
            }
            ("out", "result") => {
                if let Some(text) = event["result"].as_str().filter(|text| !text.is_empty()) {
                    transcript.final_message = Some(text.to_owned());
                }
                for denial in event["permission_denials"].as_array().into_iter().flatten() {
                    deny(
                        &mut transcript,
                        &mut denied,
                        &calls,
                        denial["tool_use_id"].as_str(),
                        denial["tool_name"].as_str(),
                        Some(tool_input(&denial["tool_input"])),
                        "denied by the CLI's permission mode".to_owned(),
                    );
                }
            }
            ("herdr", "permission") if event["decision"] == "deny" => deny(
                &mut transcript,
                &mut denied,
                &calls,
                event["tool_use_id"].as_str(),
                event["tool_name"].as_str(),
                None,
                format!(
                    "denied by the policy: {}",
                    one_line(event["message"].as_str().unwrap_or(""), DETAIL_MAX)
                ),
            ),
            ("herdr", "pre_tool_check") if event["decision"] == "deny" => deny(
                &mut transcript,
                &mut denied,
                &calls,
                event["tool_use_id"].as_str(),
                event["tool_name"].as_str(),
                None,
                format!(
                    "denied by a pre-tool check: {}",
                    one_line(event["message"].as_str().unwrap_or(""), DETAIL_MAX)
                ),
            ),
            ("herdr", "question") => {
                let question = &event["question"];
                transcript.questions.push(TodoReviewQuestion {
                    request_id: question["request_id"].as_str().map(str::to_owned),
                    tool_name: question["tool_name"].as_str().unwrap_or("?").to_owned(),
                    text: question["text"].as_str().unwrap_or("").to_owned(),
                    answer: None,
                });
            }
            // `answer` is the record of journals from before the answer
            // outbox.
            ("herdr", "answer" | "answer_intent") => {
                let request_id = event["request_id"].as_str();
                let answers = event["answers"]
                    .as_object()
                    .map(|answers| {
                        answers
                            .iter()
                            .map(|(question, answer)| {
                                format!("{question} → {}", answer.as_str().unwrap_or(""))
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .filter(|answers| !answers.is_empty());
                let answer = answers
                    .or_else(|| event["decision"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "?".to_owned());
                let asked = transcript.questions.iter_mut().rev().find(|question| {
                    question.answer.is_none()
                        && (request_id.is_none() || question.request_id.as_deref() == request_id)
                });
                if let Some(question) = asked {
                    question.answer = Some(answer);
                }
            }
            _ => {}
        }
    }
    transcript
}

impl WorkerSupervisor {
    /// Attempt `attempt` (the current one when `None`) of run `run_id` in
    /// one view; with `with_diff`, the full diff too.
    pub(crate) fn todo_review(
        &self,
        run_id: &str,
        attempt: Option<u32>,
        with_diff: bool,
    ) -> Result<TodoReview, WorkerError> {
        let run = self.load_run(run_id)?;
        let store = self.run_store()?;
        let rows = store.attempts(run_id).map_err(store_error)?;
        let number = attempt.unwrap_or(run.info.attempt);
        let row = rows
            .into_iter()
            .find(|row| row.number == number)
            .ok_or_else(|| {
                WorkerError::RunNotFound(format!(
                    "run {run_id} has no attempt {number} (it has {})",
                    run.info.attempt
                ))
            })?;
        let landed_sha = store
            .landings_of_run(run_id)
            .map_err(store_error)?
            .into_iter()
            .rev()
            .find(|(landed, _)| *landed == number)
            .map(|(_, sha)| sha);
        let verification = row
            .attempt
            .verification
            .as_deref()
            .and_then(|text| serde_json::from_str::<WorkerVerification>(text).ok());
        let repo = Path::new(&run.info.repo);
        let base = row.attempt.base.clone().or_else(|| run.info.base.clone());
        // The commit as the decision saw it, else as the verify found it,
        // else the branch as it is.
        let commit = row
            .attempt
            .commit
            .clone()
            .or_else(|| verification.as_ref().and_then(|found| found.head.clone()))
            .or_else(|| {
                let branch = row.branch.as_deref()?;
                let tip = git(
                    repo,
                    &[
                        "rev-parse",
                        "--verify",
                        "-q",
                        &format!("{branch}^{{commit}}"),
                    ],
                )
                .ok()?
                .trim()
                .to_owned();
                (Some(&tip) != base.as_ref() && !tip.is_empty()).then_some(tip)
            });
        let (diff_stat, diff, diff_error) = match (&base, &commit) {
            (Some(base), Some(commit)) => {
                let stat = git(repo, &["diff", "--stat", base, commit]);
                let full = with_diff.then(|| git(repo, &["diff", base, commit]));
                match (stat, full) {
                    (Err(error), _) | (Ok(_), Some(Err(error))) => (None, None, Some(error)),
                    (Ok(stat), full) => (
                        Some(stat.trim_end().to_owned()).filter(|stat| !stat.is_empty()),
                        full.and_then(Result::ok),
                        None,
                    ),
                }
            }
            (None, _) => (None, None, Some("the run has no base yet".to_owned())),
            (Some(_), None) => (None, None, Some("the attempt has no commit".to_owned())),
        };
        let transcript = row
            .worker_id
            .as_deref()
            .and_then(|worker_id| std::fs::read_to_string(self.journal_path(worker_id)).ok())
            .map(|journal| transcript(journal.lines()))
            .unwrap_or_default();
        let decision = row.attempt.review_decision.clone();
        let approved = match (decision.as_deref(), &row.attempt.commit, &base) {
            (Some(APPROVE), Some(commit), Some(base)) => Some(TodoApproval {
                commit: commit.clone(),
                base: base.clone(),
            }),
            _ => None,
        };
        Ok(TodoReview {
            run_id: run.info.run_id.clone(),
            item: run.info.item.clone(),
            repo: run.info.repo.clone(),
            status: run.info.status,
            attempt: number,
            attempts: run.info.attempt,
            worker_id: row.worker_id,
            branch: row.branch,
            base,
            commit,
            from_attempt: row.attempt.from_attempt,
            from_commit: row.attempt.from_commit,
            task: transcript.task.unwrap_or(row.task),
            final_message: transcript.final_message,
            questions: transcript.questions,
            tool_failures: transcript.tool_failures,
            diff_stat,
            diff,
            diff_error,
            verification,
            decision,
            review_text: row.attempt.review_text,
            approved,
            landed_sha,
            draft: self.applied_draft(run_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(dir: &str, event: Value) -> String {
        json!({"ts_ms": 1, "dir": dir, "event": event}).to_string()
    }

    #[test]
    fn a_journal_gives_the_task_reply_questions_and_failed_calls() {
        let journal = [
            record(
                "in",
                json!({"type": "user", "message": {"content": "do it\n---\ncontract"}}),
            ),
            record(
                "out",
                json!({"type": "assistant", "message": {"content": [
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "false"}},
                    {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "git push"}},
                    {"type": "tool_use", "id": "t3", "name": "WebFetch",
                     "input": {"url": "https://example.com"}}
                ]}}),
            ),
            record(
                "out",
                json!({"type": "user", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t1", "is_error": true,
                     "content": "exit 1"},
                    {"type": "tool_result", "tool_use_id": "t0", "is_error": false,
                     "content": "fine"}
                ]}}),
            ),
            record(
                "herdr",
                json!({"type": "permission", "tool_name": "Bash", "tool_use_id": "t2",
                       "decision": "deny", "message": "no pushes"}),
            ),
            // The denied call's own error result is the same denial.
            record(
                "out",
                json!({"type": "user", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t2", "is_error": true,
                     "content": "no pushes"}
                ]}}),
            ),
            record(
                "herdr",
                json!({"type": "question", "question": {"request_id": "q1",
                       "tool_name": "WebFetch", "text": "https://example.com"}}),
            ),
            record(
                "herdr",
                json!({"type": "answer_intent", "request_id": "q1", "tool_name": "WebFetch",
                       "decision": "allow", "answers": {}}),
            ),
            record(
                "herdr",
                json!({"type": "question", "question": {"request_id": "q2",
                       "tool_name": "Bash", "text": "rm -rf target"}}),
            ),
            record(
                "out",
                json!({"type": "result", "subtype": "success", "result": "first"}),
            ),
            record(
                "in",
                json!({"type": "user", "message": {"content": "a later prompt"}}),
            ),
            record(
                "out",
                json!({"type": "result", "subtype": "success", "result": "WORKER-DONE abc | x",
                       "permission_denials": [{"tool_name": "Write", "tool_use_id": "t9",
                                               "tool_input": {"file_path": "/outside"}}]}),
            ),
            "not json".to_owned(),
        ];
        let read = transcript(journal.iter().map(String::as_str));
        assert_eq!(read.task.as_deref(), Some("do it\n---\ncontract"));
        assert_eq!(read.final_message.as_deref(), Some("WORKER-DONE abc | x"));
        assert_eq!(
            read.questions,
            [
                TodoReviewQuestion {
                    request_id: Some("q1".into()),
                    tool_name: "WebFetch".into(),
                    text: "https://example.com".into(),
                    answer: Some("allow".into()),
                },
                TodoReviewQuestion {
                    request_id: Some("q2".into()),
                    tool_name: "Bash".into(),
                    text: "rm -rf target".into(),
                    answer: None,
                },
            ]
        );
        assert_eq!(
            read.tool_failures,
            [
                TodoReviewToolFailure {
                    kind: TodoToolFailureKind::Failed,
                    tool_name: "Bash".into(),
                    input: Some("false".into()),
                    detail: "exit 1".into(),
                },
                TodoReviewToolFailure {
                    kind: TodoToolFailureKind::Denied,
                    tool_name: "Bash".into(),
                    input: Some("git push".into()),
                    detail: "denied by the policy: no pushes".into(),
                },
                TodoReviewToolFailure {
                    kind: TodoToolFailureKind::Denied,
                    tool_name: "Write".into(),
                    input: Some("/outside".into()),
                    detail: "denied by the CLI's permission mode".into(),
                },
            ]
        );
    }
}
