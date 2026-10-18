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
//!   escalated, as is one of a commit that changes a guarded path (the
//!   checks, the review's checklist and its golden set) or of one whose
//!   guard could not be read: those only a coordinator approves, explicitly;
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

use super::auto_draft::decisions_named;
use super::context_pack::{self, LogPaths};
use super::decision::{self, Prompt, MAX_CALLS};
use super::escalations;
use super::review_diff;
use super::review_tools::{self, DiffLines, RepoTools};
use super::{
    event_of, git, is_gone, new_event, read_checks, CreditStage, Run, APPROVE, DECISIONS_FILE,
    MAX_ATTEMPTS, NO_DEADLINE, RETRY,
};
use crate::api::schema::{
    TodoEventKind, TodoRunEvent, TodoRunStatus, TodoStep, WorkerCheckOutcome, WorkerCommandTarget,
    WorkerVerdict, WorkerVerification, WorkerWaitUntil,
};
use crate::workers::{now_ms, WorkerError, WorkerSupervisor};

/// The diff the model gets is cut at this many bytes, except the files of
/// [`review_diff::FULL_PATHS`] and the repository's `[review] full_paths`,
/// which are never cut.
const DIFF_MAX: usize = 200_000;
/// The repository's review checklist: one line per bug class, each ending
/// with its source (a `DECISIONS.md` section title or a commit sha).
pub(super) const CHECKLIST_FILE: &str = ".herdr/review-checklist.md";
/// The checklist's lines the model gets, at most.
pub(super) const CHECKLIST_MAX: usize = 25;
/// The run events this module writes.
const CALL: &str = "run_review_call";
const DECISION: &str = "run_review_decision";
const SUPERSEDED: &str = "run_review_superseded";
const APPLIED: &str = "run_auto_reviewed";
const VERIFIED: &str = "run_review_verified";
/// The findings of a call that the server dropped for lack of a citation.
const DROPPED: &str = "run_review_findings_dropped";
/// One tool call of the review (name, arguments, bytes).
const TOOL_CALL: &str = "run_review_tool_call";
/// Whether the review had repository tools, and why not when it had none.
const TOOLS: &str = "run_review_tools";
/// Per review call: how many kept findings came from a checklist line and
/// how many were free, to see checklist anchoring.
const ORIGINS: &str = "run_review_origins";
const STOP_INTENT: &str = "run_review_stop_intent";
/// The key a `review` event raised by an escalation carries: the decision
/// that raised it. The server does not review such an event again.
pub(super) const ESCALATES: &str = "escalates";

/// The output's JSON schema, which `claude -p --json-schema` enforces and
/// [`parse_decision`] checks again.
const OUTPUT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"type":"string","enum":["approve","retry","escalate"]},"note":{"type":"string","description":"approve only: an optional note on the approval"},"cut_files_reviewed":{"type":"boolean","description":"approve only: false when the input's diff_cut_files is not empty (you did not see those files, and the server never applies an approval of a cut diff); true otherwise"},"review":{"type":"string","description":"retry only: what the next attempt must change; its worker gets this text"},"question":{"type":"string","description":"escalate only: the question for the user"},"options":{"type":"array","items":{"type":"string"},"minItems":2,"maxItems":4,"description":"escalate only: the answers the user can choose from, the recommended one first"},"findings":{"type":"array","description":"the problems you found; the server drops a finding that lacks file, line or verified_by, or whose file or line is not in the reviewed commit","items":{"type":"object","additionalProperties":false,"required":["file","line","verified_by","text","origin"],"properties":{"origin":{"type":"string","enum":["checklist","free"],"description":"checklist: the finding applies a line of review_checklist; free: you found it without one"},"checklist_line":{"type":"integer","description":"origin checklist only: the 1-based position of that line in review_checklist"},"file":{"type":"string","description":"the file the finding is about"},"line":{"type":"integer","description":"the line in that file, from the diff or the context pack"},"verified_by":{"type":"string","description":"what in the input shows it: the diff hunk, a check's evidence, a context pack entry"},"text":{"type":"string","description":"what is wrong"}}}}}}"#;

const SYSTEM_PROMPT: &str = "You review one attempt of a headless coding worker that herdr's \
TODO driver ran for a TODO item. The input is JSON: the item's text, the worker's task, the \
exact commit subject and the paths it had to keep to, the worker's final reply, the diff of its \
commit against the run's base (`diff_cut_files` names the files the diff leaves out when it was \
cut), the verify the server ran on that commit before asking you (the exact subject, the paths, \
a clean tree, and every registered check with its outcome and evidence), a `context_pack` the \
server built without a model (the item's text, the DECISIONS.md sections it names, its run \
history with each earlier attempt's failure reason, the recent commits of the paths, and lines \
of code the item names) and, when the repository has one, a `review_checklist` of bug classes \
with their sources: check the diff against each line. Decide one action and answer only with \
JSON matching the schema:\n\
- approve (optional note) when the verify passed and the diff does what the item and the task \
ask, within the paths, and nothing in the worker's reply contradicts it;\n\
- retry (review) when something is missing or wrong that the worker can fix, a failed check \
included: the review says concretely what to change, and the next attempt's worker gets it;\n\
- escalate (question, 2 to 4 options, the recommended first) when the attempt needs the user's \
decision (scope, a product choice, a permission, a contradiction in the item), a check could \
not run, or you cannot judge it.\n\
When the input has `user_answer`, the user answered your earlier question: decide by it.\n\
You may read the repository as it is in the reviewed commit with the tools read_file, grep \
and git_log (a few calls only: the budget is small and, once spent, you must answer); use them \
to check a doubt before you name it. Every finding cites `file`, `line` and `verified_by` \
(what shows it: the diff hunk, a check's evidence, a context pack entry or a tool result) and \
its `origin`: `checklist` with `checklist_line` (the 1-based position in review_checklist) when \
it applies a checklist line, else `free`. The server checks that the file exists at the commit \
and the line is in it, and drops a finding that fails or lacks them, so name no problem you \
cannot show. An approval of a diff that \
was cut (`diff_cut_files` not empty) is never applied: say `cut_files_reviewed: false` and retry \
or escalate instead when the cut files matter.\n\
Never approve an attempt without a commit or one whose verify failed.";

pub(super) const PROMPT: Prompt<'static> = Prompt {
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
    /// Read by the server from the raw output ([`checked_output`]); the
    /// field is here so the schema check accepts it.
    #[serde(default)]
    #[allow(dead_code)]
    cut_files_reviewed: Option<bool>,
    /// Checked apart ([`cite_findings`]); any entry is accepted here.
    #[serde(default)]
    #[allow(dead_code)]
    findings: Option<Vec<Value>>,
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

/// The findings that cite their evidence (a nonempty `file`, a `line` of 1
/// or more and a nonempty `verified_by`) and the ones that do not, each
/// with what it lacks.
pub(super) fn cite_findings(output: &Value) -> (Vec<Value>, Vec<Value>) {
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for finding in output["findings"].as_array().into_iter().flatten() {
        let text = |field: &str| {
            finding[field]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
        };
        let missing: Vec<&str> = [
            ("file", text("file")),
            (
                "line",
                finding["line"].as_u64().is_some_and(|line| line >= 1),
            ),
            ("verified_by", text("verified_by")),
        ]
        .into_iter()
        .filter(|(_, present)| !present)
        .map(|(field, _)| field)
        .collect();
        if missing.is_empty() {
            kept.push(finding.clone());
        } else {
            dropped.push(json!({"finding": finding, "missing": missing}));
        }
    }
    (kept, dropped)
}

/// The mechanical check of the cited findings: each one's `file` must be a
/// regular file at `commit` and its `line` within it (for a file the diff
/// changed, `in_diff` records whether the line is in the diff), and its
/// `origin` is `checklist` with a `checklist_line` among the
/// `checklist_len` lines, or `free`. A finding that fails is dropped with
/// what failed, as an uncited one is. Without a commit nothing can be
/// checked against it and only the origin is.
pub(super) fn check_findings(
    repo: &Path,
    commit: Option<&str>,
    base: Option<&str>,
    findings: Vec<Value>,
    checklist_len: usize,
) -> (Vec<Value>, Vec<Value>) {
    let diff = match (base, commit) {
        (Some(base), Some(commit)) => Some(DiffLines::read(repo, base, commit)),
        _ => None,
    };
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for mut finding in findings {
        let mut failed: Vec<String> = Vec::new();
        if finding["origin"].as_str() == Some("checklist") {
            let line = finding["checklist_line"].as_u64().map(|line| line as usize);
            if !line.is_some_and(|line| (1..=checklist_len).contains(&line)) {
                failed.push(format!(
                    "checklist_line (a line of 1 to {checklist_len} of the checklist)"
                ));
            }
        } else {
            finding["origin"] = json!("free");
            if let Some(object) = finding.as_object_mut() {
                object.remove("checklist_line");
            }
        }
        if let Some(commit) = commit {
            let file = finding["file"].as_str().unwrap_or_default().to_owned();
            let line = finding["line"].as_u64().unwrap_or_default() as usize;
            match review_tools::blob_at(repo, commit, &file) {
                Err(error) => failed.push(format!("file at the commit ({error})")),
                Ok(text) if line > text.lines().count() => failed.push(format!(
                    "line within the file ({file} has {} lines)",
                    text.lines().count()
                )),
                Ok(_) => {
                    if let Some(in_diff) = diff.as_ref().and_then(|d| d.touches(&file, line)) {
                        finding["in_diff"] = json!(in_diff);
                    }
                }
            }
        }
        if failed.is_empty() {
            kept.push(finding);
        } else {
            dropped.push(json!({"finding": finding, "missing": failed}));
        }
    }
    (kept, dropped)
}

/// The counts of the kept findings by origin, with the checklist lines
/// they cite.
fn origin_counts(findings: &[Value]) -> Value {
    let lines: Vec<&Value> = findings
        .iter()
        .filter(|finding| finding["origin"] == "checklist")
        .map(|finding| &finding["checklist_line"])
        .collect();
    json!({"checklist": lines.len(), "free": findings.len() - lines.len(),
           "checklist_lines": lines})
}

/// The decision as recorded and applied: the checked decision with the
/// model's `cut_files_reviewed` and its cited findings.
fn checked_output(decision: &Decision, output: &Value, findings: Vec<Value>) -> Value {
    let mut checked = decision.to_json();
    if let Some(reviewed) = output["cut_files_reviewed"].as_bool() {
        checked["cut_files_reviewed"] = json!(reviewed);
    }
    if !findings.is_empty() {
        checked["findings"] = Value::Array(findings);
    }
    checked
}

/// A retry's review text with the findings the decision cited.
fn with_findings(review: &str, output: Option<&Value>) -> String {
    let lines: Vec<String> = output
        .and_then(|output| output["findings"].as_array())
        .into_iter()
        .flatten()
        .map(|finding| {
            format!(
                "- {}:{} {} (verified by: {})",
                finding["file"].as_str().unwrap_or_default(),
                finding["line"],
                finding["text"].as_str().unwrap_or_default().trim(),
                finding["verified_by"].as_str().unwrap_or_default().trim(),
            )
        })
        .collect();
    if lines.is_empty() {
        return review.to_owned();
    }
    format!("{review}\n\nFindings:\n{}", lines.join("\n"))
}

/// The review checklist's lines (at most [`CHECKLIST_MAX`]) and how many
/// the cut left out; none for a repository without the file.
pub(super) fn checklist_of(repo: &Path) -> (Vec<String>, usize) {
    let text = std::fs::read_to_string(repo.join(CHECKLIST_FILE)).unwrap_or_default();
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let left_out = lines.len().saturating_sub(CHECKLIST_MAX);
    (lines.into_iter().take(CHECKLIST_MAX).collect(), left_out)
}

/// The pieces of a review's input: read from a run by the automatic review,
/// from a golden case by its replay ([`super::review_eval`]).
pub(super) struct InputParts<'a> {
    /// The repository the diff, the context pack and `[review] full_paths`
    /// are read from.
    pub(super) repo: &'a Path,
    pub(super) base: Option<&'a str>,
    /// The revision whose `DECISIONS.md` the context pack quotes.
    pub(super) decisions_rev: &'a str,
    pub(super) commit: Option<&'a str>,
    pub(super) item: &'a str,
    pub(super) item_text: String,
    pub(super) task: &'a str,
    pub(super) attempt: u32,
    pub(super) commit_subject: &'a str,
    pub(super) allowed_paths: &'a [String],
    pub(super) worker_final_text: Option<&'a str>,
    pub(super) diff_stat: Option<&'a str>,
    pub(super) event_note: Option<&'a str>,
    pub(super) history: Vec<Value>,
    pub(super) checks: Value,
    /// Whether the input has the context pack (a replay's ablation drops it).
    pub(super) context_pack: bool,
    /// The checklist's lines and how many the cut left out
    /// ([`checklist_of`]); none leaves `review_checklist` out.
    pub(super) checklist: (Vec<String>, usize),
    pub(super) user_answer: Option<&'a Value>,
}

/// What the model reviews: the item, the task, the contract, the worker's
/// final reply, the diff of the commit against the base, the checks, the
/// context pack and the checklist, and the user's answer when there is
/// one; with the files the diff's cut left out.
pub(super) fn assemble_input(parts: InputParts<'_>) -> (Value, Vec<String>) {
    let repo = parts.repo;
    let full_paths = read_checks(repo)
        .map(|checks| checks.review.full_paths)
        .unwrap_or_default();
    let cut = match (parts.base, parts.commit) {
        (Some(base), Some(commit)) => review_diff::cut(
            git(repo, &["diff", base, commit])
                .unwrap_or_else(|error| format!("(the diff could not be read: {error})")),
            DIFF_MAX,
            &full_paths,
        ),
        _ => review_diff::CutDiff {
            text: "(the attempt has no commit since the base)".to_owned(),
            cut_files: Vec::new(),
        },
    };
    let mut input = json!({
        "item": {"id": parts.item, "text": parts.item_text},
        "task": parts.task,
        "attempt": parts.attempt,
        "max_attempts": MAX_ATTEMPTS,
        "commit_subject": parts.commit_subject,
        "allowed_paths": parts.allowed_paths,
        "base": parts.base,
        "commit": parts.commit,
        "worker_final_text": parts.worker_final_text,
        "diff_stat": parts.diff_stat,
        "diff": cut.text,
        "diff_truncated": !cut.cut_files.is_empty(),
        "diff_cut_files": cut.cut_files,
        "event_note": parts.event_note,
        "checks": parts.checks,
    });
    if parts.context_pack {
        let decisions = git(
            repo,
            &["show", &format!("{}:{DECISIONS_FILE}", parts.decisions_rev)],
        )
        .map(|text| decisions_named(&text, &parts.item_text, parts.item))
        .unwrap_or_default();
        input["context_pack"] = context_pack::build(
            repo,
            parts.base,
            &parts.item_text,
            decisions,
            parts.history,
            LogPaths::Globs(parts.allowed_paths),
        );
    }
    let (checklist, checklist_cut) = parts.checklist;
    if !checklist.is_empty() {
        input["review_checklist"] = json!(checklist);
        if checklist_cut > 0 {
            input["review_checklist_cut"] = json!(format!(
                "{checklist_cut} lines past {CHECKLIST_MAX} were left out"
            ));
        }
    }
    if let Some(answer) = parts.user_answer {
        input["user_answer"] = answer.clone();
    }
    let cut_files = input["diff_cut_files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| file.as_str().map(str::to_owned))
        .collect();
    (input, cut_files)
}

/// The model of the review's call: `[review] model` of herdr's config; none
/// keeps the run's own.
pub(super) fn review_model() -> Option<String> {
    crate::config::load_live_config()
        .ok()
        .and_then(|loaded| loaded.config.review.model)
        .map(|model| model.trim().to_owned())
        .filter(|model| !model.is_empty())
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
    /// The files the diff the model got left out (it was cut).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cut_files: Vec<String>,
    /// The verdict of the verify run before the review, of `commit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<WorkerVerdict>,
    /// The paths that verify flagged as guarded (the checks, the review's
    /// checklist and its golden set): an approval of such a commit is never
    /// the server's, only the coordinator's, and only explicitly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    guarded_paths: Vec<String>,
    /// Why the verify could not read those paths, when it could not: nothing
    /// approves a commit whose guard is unknown, whatever the verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guarded_error: Option<String>,
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
pub(super) fn failed_checks(verification: &WorkerVerification) -> Vec<String> {
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
        let added = self.path_checks_due(run, &worker_id, &base)?;
        let mut checked = run.clone();
        for (check, _) in &added {
            checked.info.checks.push(check.name.clone());
            checked.checks.push(check.clone());
        }
        let verification = self.verify_attempt(&checked, &worker_id, &base)?;
        let note = json!({
            "type": VERIFIED, "event": seq, "attempt": run.info.attempt, "commit": commit,
            "verdict": verification.verdict, "checks_added": added, "verification": verification,
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
                for (check, _) in &added {
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
        let (input, cut_files) = self.review_input(run, event, body);
        let text = serde_json::to_string_pretty(&input).map_err(|error| error.to_string())?;
        let digest = decision::digest(&text);
        let store = self.run_store().map_err(|error| error.to_string())?;
        let calls = store
            .run_events_of(&run.info.run_id, CALL)
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let (mut errors, answered) =
            decision::recorded_calls(&calls, ("event", &json!(seq)), &digest);
        let mut decided = answered.filter(|(output, _)| parse_decision(output).is_ok());
        let model = review_model();
        let repo = Path::new(&run.info.repo);
        let commit = event.commits.last().cloned();
        let checklist_len = checklist_of(repo).0.len();
        // One budget of tools for the whole review, however many calls it
        // takes. Only a credit call has them: `claude -p` keeps its own
        // stateless, tool-less call.
        let mut tools = commit
            .as_deref()
            .filter(|_| run.finish.anthropic_credits)
            .and_then(|commit| RepoTools::new(repo, commit));
        if decided.is_none() {
            let note = match &tools {
                Some(_) => json!({"type": TOOLS, "event": seq, "available": true,
                    "max_calls": review_tools::MAX_CALLS, "max_bytes": review_tools::MAX_BYTES}),
                None => json!({"type": TOOLS, "event": seq, "available": false,
                    "reason": "only a credit run's review call has repository tools"}),
            };
            self.write_note(&run.info.run_id, &note)?;
        }
        let mut recorded_tool_calls = 0;
        while decided.is_none() && errors.len() < MAX_CALLS {
            let call = errors.len() + 1;
            let called = self.call_model_tools(
                run,
                CreditStage::Review,
                &PROMPT,
                &text,
                model.as_deref(),
                tools.as_mut(),
            );
            if let Some(tools) = &tools {
                for (index, tool_call) in tools.calls.iter().enumerate().skip(recorded_tool_calls) {
                    let mut note = tool_call.to_json();
                    note["type"] = json!(TOOL_CALL);
                    note["event"] = json!(seq);
                    note["call"] = json!(call);
                    note["index"] = json!(index + 1);
                    self.write_note(&run.info.run_id, &note)?;
                }
                recorded_tool_calls = tools.calls.len();
            }
            let answered = called.and_then(|(output, model)| {
                let decision = parse_decision(&output)?;
                let (cited, mut dropped) = cite_findings(&output);
                let (cited, uncheckable) = check_findings(
                    repo,
                    commit.as_deref(),
                    run.info.base.as_deref(),
                    cited,
                    checklist_len,
                );
                dropped.extend(uncheckable);
                Ok((checked_output(&decision, &output, cited), dropped, model))
            });
            let note = match &answered {
                Ok((output, dropped, model)) => {
                    if !dropped.is_empty() {
                        let note = json!({
                            "type": DROPPED, "event": seq, "call": call, "dropped": dropped,
                        });
                        self.write_note(&run.info.run_id, &note)?;
                    }
                    let kept = output["findings"].as_array().cloned().unwrap_or_default();
                    let mut counts = origin_counts(&kept);
                    counts["type"] = json!(ORIGINS);
                    counts["event"] = json!(seq);
                    counts["call"] = json!(call);
                    counts["dropped"] = json!(dropped.len());
                    self.write_note(&run.info.run_id, &counts)?;
                    json!({
                        "type": CALL, "event": seq, "call": call, "input_digest": digest,
                        "model": model, "output": output,
                    })
                }
                Err(error) => json!({
                    "type": CALL, "event": seq, "call": call, "input_digest": digest,
                    "error": error,
                }),
            };
            self.write_note(&run.info.run_id, &note)?;
            match answered {
                Ok((output, _, model)) => decided = Some((output, model)),
                Err(error) => errors.push(error),
            }
        }
        let (output, model) = match decided {
            Some((output, model)) => (Some(output), model),
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
            cut_files,
            verdict: verification.map(|verification| verification.verdict),
            guarded_paths: verification
                .map(|verification| verification.guarded_paths.clone())
                .unwrap_or_default(),
            guarded_error: verification.and_then(guarded_error_of),
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
    fn review_input(&self, run: &Run, event: &TodoRunEvent, body: &Value) -> (Value, Vec<String>) {
        let repo = Path::new(&run.info.repo);
        let verification = run
            .current
            .verification
            .as_deref()
            .and_then(|verification| serde_json::from_str::<Value>(verification).ok());
        assemble_input(InputParts {
            repo,
            base: run.info.base.as_deref(),
            decisions_rev: run.info.base.as_deref().unwrap_or(run.target_branch()),
            commit: event.commits.last().map(String::as_str),
            item: &run.info.item,
            item_text: self.item_text_of(run),
            task: &run.info.task,
            attempt: run.info.attempt,
            commit_subject: &run.info.message,
            allowed_paths: &run.info.paths,
            worker_final_text: event.result_text.as_deref(),
            diff_stat: event.diff_stat.as_deref(),
            event_note: event.error.as_deref(),
            history: self.run_history_of(run),
            checks: json!({
                "registered": run.checks,
                "added_when_the_diff_touches_their_paths": run.finish.path_checks(),
                "verify_before_review": verification,
            }),
            context_pack: true,
            checklist: checklist_of(repo),
            user_answer: body["user_answer"]
                .is_object()
                .then(|| &body["user_answer"]),
        })
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
        let outcome =
            match decision {
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
                Ok(Some(Decision::Approve { .. }))
                    if record.verdict == Some(WorkerVerdict::Failed) =>
                {
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
                Ok(Some(Decision::Approve { .. })) if !record.cut_files.is_empty() => {
                    Outcome::Escalate(
                        format!(
                        "The automatic review approved commit {}, but the diff it read was cut \
                         and it did not read {} (cut_files_reviewed: {}); review {}.",
                        record.commit.as_deref().unwrap_or("none"),
                        record.cut_files.join(", "),
                        record.output.as_ref().map_or("not given".to_owned(), |output| {
                            output["cut_files_reviewed"].to_string()
                        }),
                        if record.cut_files.len() == 1 { "that file" } else { "those files" },
                    ),
                        review_options(),
                    )
                }
                Ok(Some(Decision::Approve { .. })) if !record.guarded_paths.is_empty() => {
                    Outcome::Escalate(
                        format!(
                            "The automatic review approved commit {}, but that commit changes {}, \
                         which decide whether a commit is acceptable (the checks the verify runs, \
                         the review's checklist and its golden set). Only a coordinator approves \
                         it, after reading them and only explicitly (`herdr todo resume --action \
                         approve --accept-guarded-paths`); nothing here approves it for you.",
                            record.commit.as_deref().unwrap_or("none"),
                            record.guarded_paths.join(", "),
                        ),
                        guarded_options(),
                    )
                }
                Ok(Some(Decision::Approve { .. })) if record.guarded_error.is_some() => {
                    Outcome::Escalate(
                        format!(
                            "The automatic review approved commit {}, but the verify could not \
                         read the paths that decide whether a commit is acceptable: {}. Nothing \
                         here approves a commit whose guard is unknown; review it.",
                            record.commit.as_deref().unwrap_or("none"),
                            record.guarded_error.as_deref().unwrap_or("unknown"),
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
                Ok(Some(Decision::Retry { review })) => {
                    let review = with_findings(&review, record.output.as_ref());
                    self.retry(run, seq, record, &review)?
                }
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
        // A queue run's own approval closes its item, so the queue moves on.
        let close = run.finish.queued.then(|| {
            super::queue::queue_close(run, Some(commit), note.as_deref(), "the server's review")
        });
        let body = json!({
            "type": APPLIED, "event": seq, "decision_id": record.decision_id,
            "action": "approve", "note": note, "commit": commit, "base": record.base,
            "close": close,
        });
        self.apply_change(run, seq, record, &body, |current| {
            if current.finish.queued && current.finish.note.is_none() {
                current.finish.close = close.clone();
            }
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
        let next = super::usage_gate::Holder::Run {
            run_id: run.info.run_id.clone(),
            attempt: run.info.attempt + 1,
        };
        let usage = match self.usage_gate(
            &run.info.repo,
            run.finish.agent.unwrap_or_default(),
            run.finish.worker_model(),
            false,
            Some(&next),
            None,
        ) {
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
            pane_id: None,
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

/// Why the verify could not read the guarded paths, when its `guarded`
/// check says it could not.
fn guarded_error_of(verification: &WorkerVerification) -> Option<String> {
    verification
        .checks
        .iter()
        .find(|check| check.check == "guarded" && check.outcome == WorkerCheckOutcome::Unavailable)
        .map(|check| decision::cut(&check.detail))
}

/// The choices an approval the server will not make leaves the coordinator:
/// it reads the guarded paths first, then approves explicitly.
fn guarded_options() -> Vec<String> {
    vec![
        "approve the attempt after reading the guarded files".into(),
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
    fn findings_without_a_citation_are_dropped_with_what_they_lack() {
        let output = json!({"action": "retry", "review": "x", "findings": [
            {"file": "src/a.rs", "line": 3, "verified_by": "the diff hunk", "text": "ok"},
            {"file": "src/b.rs", "line": 4, "text": "no evidence"},
            {"file": "", "line": 0, "verified_by": " ", "text": "nothing"},
            {"line": 7, "verified_by": "diff", "text": "no file"},
            {"file": "c.rs", "line": "7", "verified_by": "diff"},
        ]});
        let (kept, dropped) = cite_findings(&output);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["file"], "src/a.rs");
        let missing: Vec<&Value> = dropped.iter().map(|drop| &drop["missing"]).collect();
        assert_eq!(
            missing,
            [
                &json!(["verified_by"]),
                &json!(["file", "line", "verified_by"]),
                &json!(["file"]),
                &json!(["line"]),
            ]
        );
        assert!(parse_decision(&output).is_ok());
        let (kept, dropped) = cite_findings(&json!({"action": "approve"}));
        assert!(kept.is_empty() && dropped.is_empty());
    }

    #[test]
    fn findings_are_checked_against_the_commit_and_tagged_by_origin() {
        use super::super::review_tools::tests::repo_with;
        let (dir, base) = repo_with(&[("src/a.rs", "1\n2\n3\n"), ("b.rs", "x\n")]);
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        std::fs::write(dir.join("src/a.rs"), "1\n2\nthree\nfour\n").unwrap();
        run(&["commit", "-qam", "second"]);
        let commit = run(&["rev-parse", "HEAD"]).trim().to_owned();
        let f = |file: &str, line: u64, extra: Value| {
            let mut finding = json!({"file": file, "line": line, "verified_by": "v", "text": "t"});
            for (key, value) in extra.as_object().unwrap() {
                finding[key] = value.clone();
            }
            finding
        };
        let findings = vec![
            f(
                "src/a.rs",
                4,
                json!({"origin": "checklist", "checklist_line": 2}),
            ),
            f("src/a.rs", 1, json!({"origin": "free"})),
            f("b.rs", 1, json!({})),
            f("src/a.rs", 5, json!({"origin": "free"})),
            f("src/none.rs", 1, json!({"origin": "free"})),
            f("link", 1, json!({"origin": "free"})),
            f("../x", 1, json!({"origin": "free"})),
            f(
                "b.rs",
                1,
                json!({"origin": "checklist", "checklist_line": 9}),
            ),
            f("b.rs", 1, json!({"origin": "checklist"})),
            f("b.rs", 1, json!({"origin": "free", "checklist_line": 1})),
        ];
        let (kept, dropped) = check_findings(&dir, Some(&commit), Some(&base), findings, 3);
        assert_eq!(kept.len(), 4);
        assert_eq!(kept[0]["in_diff"], true);
        assert_eq!(kept[0]["origin"], "checklist");
        assert_eq!(kept[1]["in_diff"], false);
        assert!(kept[2].get("in_diff").is_none());
        assert_eq!(kept[2]["origin"], "free");
        assert!(kept[3].get("checklist_line").is_none());
        let why: Vec<String> = dropped
            .iter()
            .map(|d| d["missing"][0].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(dropped.len(), 6);
        assert!(
            why[0].contains("line within the file (src/a.rs has 4 lines)"),
            "{why:?}"
        );
        assert!(why[1].starts_with("file at the commit"), "{why:?}");
        assert!(why[2].contains("symlink"), "{why:?}");
        assert!(why[3].contains("inside the repository"), "{why:?}");
        assert!(why[4].starts_with("checklist_line"), "{why:?}");
        assert!(why[5].starts_with("checklist_line"), "{why:?}");
        assert_eq!(
            origin_counts(&kept),
            json!({"checklist": 1, "free": 3, "checklist_lines": [2]})
        );
        // Without a commit only the origin is checked.
        let (kept, _) = check_findings(&dir, None, None, vec![f("zz", 99, json!({}))], 3);
        assert_eq!(kept[0]["origin"], "free");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_checked_output_keeps_the_cut_flag_and_the_cited_findings() {
        let output = json!({"action": "approve", "cut_files_reviewed": false});
        let decision = parse_decision(&output).unwrap();
        let finding = json!({"file": "a", "line": 1, "verified_by": "d", "text": "t"});
        let checked = checked_output(&decision, &output, vec![finding.clone()]);
        assert_eq!(checked["cut_files_reviewed"], false);
        assert_eq!(checked["findings"], json!([finding]));
        assert_eq!(parse_decision(&checked), Ok(decision));
        let plain = checked_output(&Decision::Approve { note: None }, &json!({}), vec![]);
        assert_eq!(plain, json!({"action": "approve"}));
    }

    #[test]
    fn a_retry_carries_its_cited_findings_to_the_worker() {
        let output = json!({"findings": [
            {"file": "src/a.rs", "line": 3, "verified_by": "hunk 2", "text": " bad "},
        ]});
        assert_eq!(
            with_findings("fix it", Some(&output)),
            "fix it\n\nFindings:\n- src/a.rs:3 bad (verified by: hunk 2)"
        );
        assert_eq!(with_findings("fix it", Some(&json!({}))), "fix it");
        assert_eq!(with_findings("fix it", None), "fix it");
    }

    #[test]
    fn the_checklist_is_read_when_the_repository_has_one_and_cut_at_25_lines() {
        let dir = std::env::temp_dir().join(format!("herdr-checklist-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".herdr")).unwrap();
        assert_eq!(checklist_of(&dir), (vec![], 0));
        let lines: String = (0..30)
            .map(|i| format!("- class {i} (source: abc{i})\n"))
            .collect();
        std::fs::write(dir.join(CHECKLIST_FILE), format!("# Checklist\n\n{lines}")).unwrap();
        let (kept, left_out) = checklist_of(&dir);
        assert_eq!((kept.len(), left_out), (CHECKLIST_MAX, 5));
        assert_eq!(kept[0], "- class 0 (source: abc0)");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_committed_checklist_is_short_and_each_line_names_its_source() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (lines, left_out) = checklist_of(root);
        assert!(!lines.is_empty() && left_out == 0, "{left_out}");
        let decisions = std::fs::read_to_string(root.join("DECISIONS.md")).unwrap_or_default();
        let shallow = git(root, &["rev-parse", "--is-shallow-repository"])
            .map_or(true, |text| text.trim() != "false");
        for line in &lines {
            let source = line
                .strip_suffix(')')
                .and_then(|line| line.rsplit_once("(source: "))
                .map(|(_, source)| source)
                .unwrap_or_else(|| panic!("no source at the end of: {line}"));
            match source.strip_prefix("DECISIONS.md \"") {
                Some(title) => {
                    let title = title.trim_end_matches('"');
                    assert!(
                        decisions
                            .lines()
                            .any(|l| l.starts_with("## ") && l[3..].starts_with(title)),
                        "no DECISIONS.md section {title:?}"
                    );
                }
                None => {
                    assert!(
                        source.len() >= 7 && source.chars().all(|c| c.is_ascii_hexdigit()),
                        "{source}"
                    );
                    if !shallow {
                        assert!(
                            git(root, &["cat-file", "-e", &format!("{source}^{{commit}}")]).is_ok(),
                            "no commit {source}"
                        );
                    }
                }
            }
        }
    }
}
