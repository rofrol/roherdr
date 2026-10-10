//! A run's automatic review (`todo run --auto-review`): when the run waits
//! on a `review` event, its driver asks a model itself, in one bounded,
//! stateless call (`claude -p` with structured JSON output, no tools, no
//! session kept, the user's normal Claude login, run by the server outside
//! any worker sandbox), and applies the typed decision it returns:
//!
//! - `approve` (an optional note): the approval `todo.resume --action
//!   approve` makes, bound to the event's commit and the run's base;
//! - `retry` (the review text): the next attempt, as `--action retry`
//!   starts it, behind the same usage gate;
//! - `escalate` (a question for the user with options): a new `review`
//!   event the run waits on, carrying the question, and a notice to the
//!   user; the coordinator answers it as any review.
//!
//! The server checks the output against the schema and the action against
//! the run's state (an approval needs a commit, a retry an attempt left and
//! the usage gate's admission); an action the run cannot take is escalated
//! with the reason. A call that fails or returns invalid output is made
//! once more; a second failure is escalated.
//!
//! Each call is recorded (`run_review_call`), and the decision with its id,
//! input digest, output and model (`run_review_decision`) before it is
//! applied, so a server that ends between the two applies the recorded
//! decision when it starts instead of asking again. The application is one
//! transaction that first checks the run still waits on the reviewed event:
//! a coordinator's `todo.resume` that came first wins, and the decision is
//! recorded as superseded (`run_review_superseded`).
//!
//! The call's process exit is the event the driver waits for. It has no
//! deadline: the provider imposes none on a `claude -p` call.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{event_of, git, new_event, Run, APPROVE, MAX_ATTEMPTS, RETRY};
use crate::api::schema::{TodoEventKind, TodoRunEvent, TodoRunStatus, TodoStep};
#[cfg(test)]
use crate::workers::lock;
use crate::workers::{now_ms, WorkerSupervisor};

/// Calls per review event: the first and one more after a failure.
const MAX_CALLS: usize = 2;
/// The diff the model gets is cut at this many bytes.
const DIFF_MAX: usize = 200_000;
/// A failed call's output kept in its record, in characters.
const ERROR_MAX: usize = 2_000;
/// The run events this module writes.
const CALL: &str = "run_review_call";
const DECISION: &str = "run_review_decision";
const SUPERSEDED: &str = "run_review_superseded";
const APPLIED: &str = "run_auto_reviewed";
/// The key a `review` event raised by an escalation carries: the decision
/// that raised it. The server does not review such an event again.
const ESCALATES: &str = "escalates";

/// The output's JSON schema, which `claude -p --json-schema` enforces and
/// [`parse_decision`] checks again.
const OUTPUT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"type":"string","enum":["approve","retry","escalate"]},"note":{"type":"string","description":"approve only: an optional note on the approval"},"review":{"type":"string","description":"retry only: what the next attempt must change; its worker gets this text"},"question":{"type":"string","description":"escalate only: the question for the user"},"options":{"type":"array","items":{"type":"string"},"minItems":2,"maxItems":4,"description":"escalate only: the answers the user can choose from, the recommended one first"}}}"#;

const SYSTEM_PROMPT: &str = "You review one attempt of a headless coding worker that herdr's \
TODO driver ran for a TODO item. The input is JSON: the item's text, the worker's task, the \
exact commit subject and the paths it had to keep to, the worker's final reply, the diff of its \
commit against the run's base, and the registered checks (the verify runs them, the exact \
subject and the paths after an approval, and a failing verify does not land the commit). \
Decide one action and answer only with JSON matching the schema:\n\
- approve (optional note) when the diff does what the item and the task ask, within the paths, \
and nothing in the worker's reply contradicts it;\n\
- retry (review) when something is missing or wrong that the worker can fix: the review says \
concretely what to change, and the next attempt's worker gets it;\n\
- escalate (question, 2 to 4 options, the recommended first) when the attempt needs the user's \
decision (scope, a product choice, a permission, a contradiction in the item) or you cannot \
judge it.\n\
Never approve an attempt without a commit.";

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

/// Checks the model's output: the schema, then that each action has its
/// own fields and only those.
pub(super) fn parse_decision(output: &Value) -> Result<Decision, String> {
    let raw: RawDecision = serde_json::from_value(output.clone())
        .map_err(|error| format!("the output does not match the schema: {error}"))?;
    let nonempty = |text: Option<String>, field: &str| {
        text.map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("{} needs a nonempty {field}", raw.action))
    };
    let only = |fields: &[(&str, bool)]| match fields.iter().find(|(_, present)| *present) {
        Some((field, _)) => Err(format!("{} takes no {field}", raw.action)),
        None => Ok(()),
    };
    match raw.action.as_str() {
        "approve" => {
            only(&[
                ("review", raw.review.is_some()),
                ("question", raw.question.is_some()),
                ("options", raw.options.is_some()),
            ])?;
            let note = raw
                .note
                .clone()
                .map(|note| note.trim().to_owned())
                .filter(|note| !note.is_empty());
            Ok(Decision::Approve { note })
        }
        "retry" => {
            only(&[
                ("note", raw.note.is_some()),
                ("question", raw.question.is_some()),
                ("options", raw.options.is_some()),
            ])?;
            Ok(Decision::Retry {
                review: nonempty(raw.review.clone(), "review")?,
            })
        }
        "escalate" => {
            only(&[
                ("note", raw.note.is_some()),
                ("review", raw.review.is_some()),
            ])?;
            let question = nonempty(raw.question.clone(), "question")?;
            let options: Vec<String> = raw
                .options
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|option| option.trim().to_owned())
                .collect();
            if !(2..=4).contains(&options.len()) || options.iter().any(String::is_empty) {
                return Err("escalate needs 2 to 4 nonempty options".into());
            }
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

/// What `claude -p --output-format json` printed: the structured output and
/// the model that answered.
pub(super) fn parse_cli_output(stdout: &str) -> Result<(Value, Option<String>), String> {
    let reply: Value = serde_json::from_str(stdout.trim())
        .map_err(|error| format!("the reply is not JSON ({error}): {}", cut(stdout)))?;
    if reply["is_error"].as_bool() == Some(true) {
        return Err(format!(
            "the call failed ({}): {}",
            reply["subtype"].as_str().unwrap_or("error"),
            cut(reply["result"].as_str().unwrap_or_default())
        ));
    }
    let model = reply["modelUsage"]
        .as_object()
        .and_then(|models| models.keys().next().cloned());
    let output = match &reply["structured_output"] {
        Value::Object(_) => reply["structured_output"].clone(),
        _ => {
            let text = reply["result"].as_str().unwrap_or_default();
            serde_json::from_str(text.trim()).map_err(|error| {
                format!(
                    "the reply has no structured output and its result is not JSON ({error}): {}",
                    cut(text)
                )
            })?
        }
    };
    Ok((output, model))
}

fn cut(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(ERROR_MAX) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_owned(),
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
}

/// A decision's id: `d-` and 8 base32 characters of a hash of the run, the
/// event and the input, so the same review gets the same id.
fn decision_id(run_id: &str, event: i64, digest: &str) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let hash = Sha256::digest(format!("{run_id}:{event}:{digest}"));
    let bits = hash[..5]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let id: String = (0..8)
        .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 31) as usize] as char)
        .collect();
    format!("d-{id}")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// How a checked decision is applied to the run.
enum Outcome {
    /// The run took it (or was already past the event).
    Applied,
    /// It goes to the user: the question and its options.
    Escalate(String, Vec<String>),
}

impl WorkerSupervisor {
    /// The program the review calls: the server's `claude`.
    fn review_program(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(program) = lock(&self.shared.review_program).clone() {
            return program;
        }
        self.shared.program.clone()
    }

    #[cfg(all(test, unix))]
    pub(in crate::workers) fn set_review_program_for_test(&self, program: PathBuf) {
        *lock(&self.shared.review_program) = Some(program);
    }

    /// The run's pending `review` event when the server reviews it itself.
    pub(super) fn auto_review_due(&self, run: &Run) -> Option<(i64, TodoRunEvent)> {
        if !run.finish.auto_review || run.info.status != TodoRunStatus::Waiting {
            return None;
        }
        let (seq, body) = self
            .run_store()
            .ok()?
            .latest_run_event(&run.info.run_id)
            .ok()??;
        is_due(run, seq, &body).then(|| (seq, event_of(seq, &body, &run.info)))
    }

    /// Reviews the run's pending `review` event `seq`: the decision recorded
    /// for it, else the model's, recorded first; then applies it.
    pub(super) fn step_auto_review(
        &self,
        run: &mut Run,
        seq: i64,
        event: TodoRunEvent,
    ) -> Result<(), String> {
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
                let record = self.decide(run, seq, &event)?;
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

    /// Asks the model about the event (at most [`MAX_CALLS`] calls per
    /// event, counting those a previous server made) and records each call
    /// and the decision.
    fn decide(&self, run: &Run, seq: i64, event: &TodoRunEvent) -> Result<DecisionRecord, String> {
        let input = self.review_input(run, event);
        let text = serde_json::to_string_pretty(&input).map_err(|error| error.to_string())?;
        let digest = hex(&Sha256::digest(text.as_bytes()));
        let store = self.run_store().map_err(|error| error.to_string())?;
        let mut errors: Vec<String> = store
            .run_events_of(&run.info.run_id, CALL)
            .map_err(|error| format!("the worker store failed: {error}"))?
            .into_iter()
            .filter(|call| call["event"] == json!(seq) && call["error"].is_string())
            .filter_map(|call| call["error"].as_str().map(str::to_owned))
            .collect();
        let mut decided = None;
        while decided.is_none() && errors.len() < MAX_CALLS {
            let call = errors.len() + 1;
            let answered = self
                .call_model(&text)
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
            decision_id: decision_id(&run.info.run_id, seq, &digest),
            event: seq,
            attempt: run.info.attempt,
            commit: event.commits.last().cloned(),
            base: run.info.base.clone(),
            input_digest: digest,
            model,
            output,
            errors,
        };
        let mut body = serde_json::to_value(&record).map_err(|error| error.to_string())?;
        body["type"] = json!(DECISION);
        self.write_note(&run.info.run_id, &body)?;
        Ok(record)
    }

    /// Appends a note to the run, failing when the store does.
    fn write_note(&self, run_id: &str, note: &Value) -> Result<(), String> {
        self.run_store()
            .map_err(|error| error.to_string())?
            .transaction(|tx| tx.run_note(run_id, note, now_ms()))
            .map_err(|error| format!("the worker store failed: {error}"))?;
        super::announce();
        Ok(())
    }

    /// What the model reviews: the item, the task, the contract, the
    /// worker's final reply, the diff of the event's commit against the
    /// base and the checks with the attempt's latest verdict (the verify
    /// runs after an approval, so the first review has none yet).
    fn review_input(&self, run: &Run, event: &TodoRunEvent) -> Value {
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
        json!({
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
                "latest_verification": verification,
            },
        })
    }

    /// One `claude -p` call with `input` on its standard input; its exit is
    /// the event waited for (no deadline: the provider imposes none). The
    /// call runs in the worker store's directory with the server's
    /// environment (the user's login) without `HERDR_*`, and without tools,
    /// session, MCP servers, hooks or project instructions.
    fn call_model(&self, input: &str) -> Result<(Value, Option<String>), String> {
        let program = self.review_program();
        let mut command = Command::new(&program);
        command
            .args([
                "-p",
                "--output-format",
                "json",
                "--json-schema",
                OUTPUT_SCHEMA,
                "--tools",
                "",
                "--no-session-persistence",
                "--strict-mcp-config",
                "--safe-mode",
                "--system-prompt",
                SYSTEM_PROMPT,
            ])
            .current_dir(&self.shared.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot run {}: {error}", program.display()))?;
        // Written from a thread: a child that answers before it read all
        // of its input must not block on a full pipe while this one writes.
        let writer = child.stdin.take().map(|mut stdin| {
            let input =
                format!("Review this attempt and answer with the JSON decision only.\n\n{input}\n");
            std::thread::spawn(move || stdin.write_all(input.as_bytes()))
        });
        let output = child
            .wait_with_output()
            .map_err(|error| format!("waiting for {}: {error}", program.display()))?;
        if let Some(writer) = writer {
            // A child that exited without reading all of it broke the pipe;
            // its exit status says what happened.
            let _ = writer.join();
        }
        if !output.status.success() {
            return Err(format!(
                "{} exited with {}: {}",
                program.display(),
                output.status,
                cut(&String::from_utf8_lossy(&output.stderr))
            ));
        }
        parse_cli_output(&String::from_utf8_lossy(&output.stdout))
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
    /// question, which the run waits on, and a notice to the user.
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
                let body = json!({
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
                let mut body = body;
                body[ESCALATES] = json!(record.decision_id);
                tx.run_event(&mut current, &body, true, now_ms())?;
                Ok(Some(current))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let Some(current) = escalated else {
            return self.superseded(&run.info.run_id, seq, record);
        };
        *run = current;
        super::announce();
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
    fn the_cli_reply_gives_the_structured_output_and_the_model() {
        let reply = json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "",
            "structured_output": {"action": "approve"},
            "modelUsage": {"claude-opus-5-5": {"inputTokens": 1}},
        });
        assert_eq!(
            parse_cli_output(&reply.to_string()),
            Ok((json!({"action": "approve"}), Some("claude-opus-5-5".into())))
        );
        let text =
            json!({"is_error": false, "result": "{\"action\": \"retry\", \"review\": \"x\"}"});
        assert_eq!(
            parse_cli_output(&text.to_string()).unwrap().0["action"],
            "retry"
        );
        assert!(parse_cli_output("not json").is_err());
        assert!(
            parse_cli_output(&json!({"is_error": true, "result": "limit"}).to_string())
                .unwrap_err()
                .contains("limit")
        );
        assert!(parse_cli_output(&json!({"result": "prose"}).to_string()).is_err());
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

    #[test]
    fn a_decision_id_is_stable_for_the_same_review() {
        let id = decision_id("r-abcdefgh", 7, "digest");
        assert_eq!(id, decision_id("r-abcdefgh", 7, "digest"));
        assert_ne!(id, decision_id("r-abcdefgh", 8, "digest"));
        assert!(id.starts_with("d-") && id.len() == 10, "{id}");
    }
}
