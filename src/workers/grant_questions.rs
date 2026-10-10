//! A capability grant as the user's question in the `?` list. A run that
//! preflight refuses with `grant_required` ([`super::capabilities`]) lists
//! the operation, its requested capabilities, its definition hash and the
//! repository as a choice question (Grant, Not now), under a stand-in
//! worker id of its own. Only the user's own client answers it: the
//! client's answer dialog sends `worker.answer` through the client shell's
//! endpoint lane, which the server takes as the user's click
//! ([`WorkerSupervisor::answer_from_client`]); the same method through the
//! local JSON API (an agent's `herdr worker answer`, `todo resume`) is
//! refused with `grant_needs_user`, so no agent pane grants.
//!
//! A grant stores the definition as `todo.grant` does (checked again
//! against `master`), records it, and lets the repository's queue look
//! again; a coordinator starts the refused run again itself. Not now takes
//! the question off the list, recorded; the next refused run lists it
//! again. The list is kept in memory only: a server that starts lists a
//! question again at the next refusal.

use std::sync::Mutex;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{lock, notify_clients, PendingWorkerQuestion, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    WorkerAnswerParams, WorkerChoiceQuestion, WorkerDecision, WorkerInfo, WorkerQuestion,
    WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionState, WorkerState,
};

/// The request id of a grant question: this and its key.
pub(crate) const REQUEST_PREFIX: &str = "herdr-grant-";
/// The stand-in worker id a grant question is listed under: this and its
/// key.
const WORKER_PREFIX: &str = "grant-";
/// The options, in order: the first grants.
const GRANT: &str = "Grant";
const NOT_NOW: &str = "Not now";

/// Whether `request_id` names a grant question.
pub(crate) fn is_grant_question(request_id: &str) -> bool {
    request_id.starts_with(REQUEST_PREFIX)
}

/// One definition waiting for the user's grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GrantQuestion {
    pub(super) repo: String,
    pub(super) operation: String,
    pub(super) hash: String,
    /// The canonical definition: argv and the requested capabilities.
    pub(super) definition: Value,
    /// The base commit whose operations file the refusal read.
    pub(super) base: String,
    pub(super) since_ms: u64,
}

impl GrantQuestion {
    /// Names the repository, operation and hash, short enough for an id.
    fn key(&self) -> String {
        let digest =
            Sha256::digest(format!("{}\0{}\0{}", self.repo, self.operation, self.hash).as_bytes());
        digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn worker_id(&self) -> String {
        format!("{WORKER_PREFIX}{}", self.key())
    }

    fn request_id(&self) -> String {
        format!("{REQUEST_PREFIX}{}", self.key())
    }

    /// The argv as one line.
    fn argv(&self) -> String {
        self.definition["argv"]
            .as_array()
            .map(|argv| {
                argv.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    }

    /// The requested capabilities, one line each.
    fn capabilities(&self) -> Vec<String> {
        let Some(capabilities) = self.definition["capabilities"].as_object() else {
            return Vec::new();
        };
        capabilities
            .iter()
            .map(|(name, scope)| {
                let values: Vec<String> = scope
                    .as_object()
                    .into_iter()
                    .flat_map(|fields| fields.values())
                    .filter_map(Value::as_array)
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                if values.is_empty() {
                    name.clone()
                } else {
                    format!("{name}: {}", values.join(", "))
                }
            })
            .collect()
    }

    fn summary(&self) -> String {
        let capabilities = self.capabilities();
        format!(
            "Grant [{}] of {}: `{}`{}?",
            self.operation,
            self.repo,
            self.argv(),
            if capabilities.is_empty() {
                " with no capabilities".to_owned()
            } else {
                format!(" with {}", capabilities.join("; "))
            }
        )
    }

    fn question(&self) -> WorkerQuestion {
        WorkerQuestion {
            request_id: self.request_id(),
            kind: WorkerQuestionKind::Choice,
            tool_name: "herdr grant".into(),
            text: format!("{} (definition {})", self.summary(), self.short_hash()),
            reason: Some(format!(
                "a todo run of {} was refused: its [{}] has no grant for this definition",
                self.repo, self.operation
            )),
            questions: vec![WorkerChoiceQuestion {
                question: self.summary(),
                header: Some(format!("Grant {}", self.operation)),
                options: vec![GRANT.into(), NOT_NOW.into()],
                multi_select: false,
            }],
            since_ms: self.since_ms,
            state: WorkerQuestionState::Pending,
            escalated: Some(format!(
                "a grant is your decision, answered only from your client: Grant stores it for \
                 definition {}, Not now leaves the run refused",
                self.hash
            )),
        }
    }

    fn short_hash(&self) -> &str {
        self.hash.get(..12).unwrap_or(&self.hash)
    }

    /// The answer dialog's whole text.
    fn input_text(&self) -> String {
        let mut lines = vec![
            format!(
                "A todo run of {} needs your grant of its [{}] operation, as {} defines it in \
                 .herdr/operations.toml:",
                self.repo, self.operation, self.base
            ),
            String::new(),
            format!("argv: {}", self.argv()),
        ];
        let capabilities = self.capabilities();
        if capabilities.is_empty() {
            lines.push("capabilities: none".into());
        } else {
            lines.push("capabilities:".into());
            lines.extend(capabilities.iter().map(|line| format!("  {line}")));
        }
        lines.extend([
            String::new(),
            format!("definition hash: {}", self.hash),
            format!("definition: {}", self.definition),
            String::new(),
            "The operation runs repository code in a confined job with exactly these \
             capabilities. A changed definition has another hash and asks again."
                .into(),
        ]);
        lines.join("\n")
    }
}

static GRANT_QUESTIONS: Mutex<Vec<GrantQuestion>> = Mutex::new(Vec::new());

/// Lists the question (again, replacing one of the same definition).
pub(super) fn list(question: GrantQuestion) {
    let changed = {
        let mut listed = lock(&GRANT_QUESTIONS);
        let key = question.key();
        let known = listed.iter().position(|known| known.key() == key);
        match known {
            Some(index) if listed[index].base == question.base => false,
            Some(index) => {
                listed[index] = question;
                true
            }
            None => {
                listed.push(question);
                true
            }
        }
    };
    if changed {
        notify_clients();
    }
}

/// Takes the question of that definition off the list.
pub(super) fn take(repo: &str, operation: &str, hash: &str) {
    let taken = {
        let mut listed = lock(&GRANT_QUESTIONS);
        let before = listed.len();
        listed.retain(|known| {
            known.repo != repo || known.operation != operation || known.hash != hash
        });
        listed.len() != before
    };
    if taken {
        notify_clients();
    }
}

/// The listed grant questions as the `?` list shows a worker's questions.
pub(super) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    lock(&GRANT_QUESTIONS)
        .iter()
        .map(|question| PendingWorkerQuestion {
            worker_id: question.worker_id(),
            cwd: question.repo.clone(),
            question: question.question(),
            quiet: false,
            owner_seen_ms: None,
        })
        .collect()
}

fn find(worker_id: &str, request_id: &str) -> Result<GrantQuestion, WorkerError> {
    lock(&GRANT_QUESTIONS)
        .iter()
        .find(|known| known.worker_id() == worker_id && known.request_id() == request_id)
        .cloned()
        .ok_or_else(|| {
            WorkerError::QuestionGone(format!(
                "question {request_id} of {worker_id} is no longer pending: the definition was \
                 granted or declined meanwhile"
            ))
        })
}

/// The refusal of a grant question's answer from anywhere but the user's
/// client.
pub(super) fn needs_user(request_id: &str) -> WorkerError {
    WorkerError::GrantNeedsUser(format!(
        "{request_id} is a capability grant: only the user answers it, with a click in the `?` \
         list of their own client (or `herdr todo grant` in their own terminal); an agent never \
         grants"
    ))
}

/// Whether the answer grants: the first option, by number or label.
fn grants(answers: &[String]) -> Result<bool, WorkerError> {
    let [answer] = answers else {
        return Err(WorkerError::Invalid(format!(
            "a grant question takes one answer, got {}",
            answers.len()
        )));
    };
    match answer.trim() {
        "1" | GRANT => Ok(true),
        "2" | NOT_NOW => Ok(false),
        other => Err(WorkerError::Invalid(format!(
            "a grant question takes `{GRANT}` or `{NOT_NOW}`, not `{other}`"
        ))),
    }
}

impl WorkerSupervisor {
    /// The grant question `request_id` names, as the answer dialog shows it.
    pub(super) fn grant_question_detail(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerQuestionDetail, WorkerError> {
        let question = find(worker_id, request_id)?;
        Ok(WorkerQuestionDetail {
            worker_id: question.worker_id(),
            name: format!("grant [{}] of {}", question.operation, question.repo),
            cwd: question.repo.clone(),
            state: WorkerState::Exited,
            question: question.question(),
            input_text: question.input_text(),
            owner_pane_id: None,
            owner_coordinator_id: None,
            quiet: false,
        })
    }

    /// `worker.answer` as the user's own client sends it (the client
    /// shell's endpoint lane): the only way a grant question is answered.
    /// Any other question is answered as [`Self::answer`] answers it.
    pub(crate) fn answer_from_client(
        &self,
        params: &WorkerAnswerParams,
    ) -> Result<WorkerInfo, WorkerError> {
        match params.request_id.as_deref() {
            Some(request_id) if is_grant_question(request_id) => self.answer_grant_question(
                &params.worker_id,
                request_id,
                params.decision,
                &params.answers,
            ),
            _ => self.answer(params),
        }
    }

    /// `worker.deny_and_stop` from the user's own client: a grant question
    /// has no worker to stop, so it is declined.
    pub(crate) fn deny_and_stop_from_client(
        &self,
        params: &crate::api::schema::WorkerDenyAndStopParams,
    ) -> Result<WorkerInfo, WorkerError> {
        if is_grant_question(&params.request_id) {
            return self.answer_grant_question(
                &params.worker_id,
                &params.request_id,
                Some(WorkerDecision::Deny),
                &[],
            );
        }
        self.deny_and_stop(params)
    }

    /// The user's answer: Grant stores the grant (refused as gone when
    /// `master` defines the operation otherwise now), records it and lets
    /// the queue look again; Not now or a denial takes the question off
    /// the list, recorded.
    fn answer_grant_question(
        &self,
        worker_id: &str,
        request_id: &str,
        decision: Option<WorkerDecision>,
        answers: &[String],
    ) -> Result<WorkerInfo, WorkerError> {
        let question = find(worker_id, request_id)?;
        let granted = decision != Some(WorkerDecision::Deny) && grants(answers)?;
        if granted {
            match self.store_grant(&question.repo, &question.hash, "client") {
                Ok(_) => {}
                // The definition changed on master: this question is stale,
                // and the next refused run asks about the new one.
                Err(WorkerError::Invalid(why)) => {
                    take(&question.repo, &question.operation, &question.hash);
                    return Err(WorkerError::QuestionGone(format!(
                        "question {request_id} is no longer pending: {why}"
                    )));
                }
                Err(error) => return Err(error),
            }
        } else {
            self.record_grant_action(
                &question.repo,
                &question.operation,
                &question.hash,
                "declined",
                "client",
            )?;
        }
        take(&question.repo, &question.operation, &question.hash);
        if granted {
            // A granted definition is one of the queue's events.
            self.queue_event(&question.repo);
        }
        grant_reply(&question)
    }

    /// What an answer to a grant question replies: a stand-in worker that
    /// names the definition.
    pub(super) fn grant_reply_of(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        lock(&GRANT_QUESTIONS)
            .iter()
            .find(|known| known.worker_id() == worker_id)
            .map(grant_reply)
            .unwrap_or_else(|| Err(WorkerError::NotFound(worker_id.to_owned())))
    }
}

fn grant_reply(question: &GrantQuestion) -> Result<WorkerInfo, WorkerError> {
    serde_json::from_value(json!({
        "worker_id": question.worker_id(),
        "state": WorkerState::Exited,
        "cwd": question.repo,
        "name": format!("grant [{}] of {}", question.operation, question.repo),
        "turns": 0,
        "journal_path": "",
    }))
    .map_err(|error| WorkerError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> GrantQuestion {
        GrantQuestion {
            repo: "/repo".into(),
            operation: "prepare".into(),
            hash: "ab".repeat(32),
            definition: json!({
                "version": 1,
                "operation": "prepare",
                "argv": ["fetch-deps", "--locked"],
                "capabilities": {
                    "env": { "names": ["HOME"] },
                    "net.egress": { "hosts": ["registry.example"] },
                },
            }),
            base: "c0ffee".into(),
            since_ms: 1,
        }
    }

    #[test]
    fn a_grant_question_names_the_operation_capabilities_hash_and_repository() {
        let question = question();
        let shown = question.question();
        assert!(is_grant_question(&shown.request_id));
        assert_eq!(shown.kind, WorkerQuestionKind::Choice);
        assert_eq!(shown.questions[0].options, [GRANT, NOT_NOW]);
        for part in [
            "[prepare]",
            "/repo",
            "fetch-deps --locked",
            "net.egress: registry.example",
            "env: HOME",
        ] {
            assert!(shown.text.contains(part), "{part:?} not in {}", shown.text);
        }
        let text = question.input_text();
        assert!(text.contains(&question.hash), "{text}");
        assert!(text.contains("c0ffee"), "{text}");
        // Another repository with the same definition is another question.
        let other = GrantQuestion {
            repo: "/other".into(),
            ..question.clone()
        };
        assert_ne!(other.request_id(), question.request_id());
    }

    #[test]
    fn a_grant_takes_its_first_option_only() {
        assert!(grants(&["1".into()]).unwrap());
        assert!(grants(&[GRANT.into()]).unwrap());
        assert!(!grants(&["2".into()]).unwrap());
        assert!(!grants(&[NOT_NOW.into()]).unwrap());
        assert!(grants(&["yes".into()]).is_err());
        assert!(grants(&[]).is_err());
        assert!(grants(&["1".into(), "1".into()]).is_err());
    }
}
