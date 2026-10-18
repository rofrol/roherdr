//! The replay of the golden set (`herdr todo review-eval`): the resident
//! coordinator's past catches, each a review that sent a worker's attempt
//! back, replayed blind against the automatic review ([`super::auto_review`]).
//!
//! Each case under [`GOLDEN_DIR`] holds the attempt's diff (`attempt.patch`
//! against the run's `base`) and what the coordinator caught in it
//! (`case.json`'s `expected`). The replay applies the patch to a temporary
//! git worktree at the base, commits it there, and builds the review's input
//! as the automatic review does ([`super::auto_review::assemble_input`]),
//! except for what needs a live run: no verify (the checks say "not
//! verified"), no run history, the task and the worker's reply from the case,
//! the item's text from `TODO.md` at the base. The input never holds the
//! expected entries. The call goes through the review's own path (the credit
//! proxy with the review tools, or `claude -p`) behind [`Reviewer`], so tests
//! use a fake one, and every call is recorded, failed and retried ones too.
//!
//! Scoring is mechanical: an expected entry is matched by a kept finding in
//! the same file whose line lies within [`LINE_WINDOW`] lines of the
//! entry's, when both have one; each finding matches one entry at most. The
//! primary metric is the recall of the severe entries ([`PRIMARY_METRIC`]),
//! and a severe entry one arm caught and another missed is a severe
//! regression of the arm that missed it.
//!
//! The cases are only catches made after the checklist's cut-off (the
//! `# Cut-off:` line of [`super::auto_review::CHECKLIST_FILE`]), so the
//! checklist was never distilled from what the replay scores.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::auto_review::{
    assemble_input, check_findings, checklist_of, cite_findings, parse_decision, review_model,
    InputParts, CHECKLIST_FILE, PROMPT,
};
use super::decision::{self, CreditCall, CreditCaller, Prompt, MAX_CALLS};
use super::git;
use super::review_tools::RepoTools;
use super::CreditStage;
use crate::workers::todo_titles;

/// The golden set's directory in the repository.
pub(super) const GOLDEN_DIR: &str = ".herdr/review-golden";
/// A case's description and its diff, in the case's directory.
const CASE_FILE: &str = "case.json";
const PATCH_FILE: &str = "attempt.patch";
/// The checklist's header line that names its cut-off.
const CUTOFF_PREFIX: &str = "# Cut-off: ";
/// How far a finding's line may lie from an expected entry's to match it.
pub(super) const LINE_WINDOW: u64 = 15;
/// The predeclared primary metric.
pub(super) const PRIMARY_METRIC: &str = "recall of the severe expected entries \
(matched severe / expected severe, per arm); a severe entry that another arm matched and this \
arm missed is a severe regression of this arm";
/// What the replay says about the checks, which it does not run.
const NOT_VERIFIED: &str = "not verified: a replay of a golden case runs no checks";

/// One case of the golden set, as `case.json` holds it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Case {
    pub(super) run: String,
    pub(super) item: String,
    pub(super) attempt: u32,
    pub(super) base: String,
    pub(super) commit: String,
    pub(super) subject: String,
    #[serde(default)]
    pub(super) allowed_paths: Vec<String>,
    /// When the catch was made, RFC 3339.
    pub(super) caught_at: String,
    /// Where `caught_at` was taken from.
    pub(super) caught_at_source: String,
    /// The dump of the review the case was taken from.
    pub(super) source: Value,
    /// A patch over the size limit is not stored, and the case not replayed.
    #[serde(default)]
    pub(super) patch_omitted: bool,
    #[serde(default)]
    pub(super) patch_omitted_reason: Option<String>,
    pub(super) task: String,
    pub(super) worker_final_text: String,
    pub(super) expected: Vec<Expected>,
}

/// One distinct catch of the coordinator's review.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Expected {
    pub(super) file: String,
    #[serde(default)]
    pub(super) line: Option<u64>,
    pub(super) summary: String,
    pub(super) severe: bool,
}

/// The cut-off the checklist's `# Cut-off:` header line names.
pub(super) fn checklist_cutoff(checklist: &str) -> Result<OffsetDateTime, String> {
    let line = checklist
        .lines()
        .find_map(|line| line.trim().strip_prefix(CUTOFF_PREFIX))
        .ok_or_else(|| format!("the checklist has no `{CUTOFF_PREFIX}` line"))?;
    let stamp = line.split_whitespace().next().unwrap_or_default();
    OffsetDateTime::parse(stamp, &Rfc3339)
        .map_err(|error| format!("the checklist's cut-off {stamp:?} is not RFC 3339: {error}"))
}

/// The golden set's cases by name, each checked: `case.json` parses, names
/// an existing `attempt.patch` (or says why it was omitted), has an
/// expected entry and was caught after `cutoff`.
pub(super) fn load_cases(
    golden: &Path,
    cutoff: OffsetDateTime,
) -> Result<Vec<(String, Case)>, String> {
    let mut names: Vec<String> = std::fs::read_dir(golden)
        .map_err(|error| format!("cannot read {}: {error}", golden.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut cases = Vec::new();
    for name in names {
        let dir = golden.join(&name);
        let text = std::fs::read_to_string(dir.join(CASE_FILE))
            .map_err(|error| format!("case {name}: cannot read {CASE_FILE}: {error}"))?;
        let case: Case = serde_json::from_str(&text)
            .map_err(|error| format!("case {name}: {CASE_FILE} does not parse: {error}"))?;
        if case.expected.is_empty() {
            return Err(format!("case {name} has no expected entry"));
        }
        if case.patch_omitted {
            if case
                .patch_omitted_reason
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(format!("case {name} omits its patch without a reason"));
            }
        } else if !dir.join(PATCH_FILE).is_file() {
            return Err(format!("case {name} has no {PATCH_FILE}"));
        }
        let caught = OffsetDateTime::parse(&case.caught_at, &Rfc3339).map_err(|error| {
            format!(
                "case {name}: caught_at {:?} is not RFC 3339: {error}",
                case.caught_at
            )
        })?;
        if caught <= cutoff {
            return Err(format!(
                "case {name} was caught at {}, not after the checklist's cut-off: it may have \
                 gone into the checklist",
                case.caught_at
            ));
        }
        cases.push((name, case));
    }
    Ok(cases)
}

/// One arm of the replay: the full input, or the input without one part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Arm {
    Full,
    WithoutContextPack,
    WithoutChecklist,
}

impl Arm {
    pub(super) fn name(self) -> &'static str {
        match self {
            Arm::Full => "full",
            Arm::WithoutContextPack => "without_context_pack",
            Arm::WithoutChecklist => "without_checklist",
        }
    }
}

/// What `herdr todo review-eval` replays.
#[derive(Debug, Clone, Default)]
pub(crate) struct EvalOptions {
    /// The cases by name; none replays every case.
    pub(crate) cases: Vec<String>,
    /// The ablation arms, run beside the full one.
    pub(crate) without_context_pack: bool,
    pub(crate) without_checklist: bool,
    /// Whether the calls go through the credit proxy (with the review
    /// tools) rather than `claude -p`.
    pub(crate) credits: bool,
}

impl EvalOptions {
    fn arms(&self) -> Vec<Arm> {
        let mut arms = vec![Arm::Full];
        if self.without_context_pack {
            arms.push(Arm::WithoutContextPack);
        }
        if self.without_checklist {
            arms.push(Arm::WithoutChecklist);
        }
        arms
    }
}

/// The review's model call, injectable: the credit proxy or `claude -p` in
/// use, a fake one in tests.
pub(super) trait Reviewer {
    /// Whether its calls take the read-only repository tools.
    fn has_tools(&self) -> bool;
    /// One call with `input`, `model` as `[review] model` gives it.
    fn call(
        &mut self,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
        tools: Option<&mut RepoTools<'_>>,
        booked_to: &str,
        item: &str,
    ) -> Result<(Value, Option<String>), String>;
    /// What the last call noted besides its answer (a credit call's cost, a
    /// credit refusal's fallback).
    fn take_notes(&mut self) -> Vec<Value> {
        Vec::new()
    }
}

/// `claude -p`, as the review of a run without credits calls it.
struct CliReviewer {
    program: PathBuf,
    dir: PathBuf,
}

impl Reviewer for CliReviewer {
    fn has_tools(&self) -> bool {
        false
    }

    fn call(
        &mut self,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
        _tools: Option<&mut RepoTools<'_>>,
        _booked_to: &str,
        _item: &str,
    ) -> Result<(Value, Option<String>), String> {
        decision::call_cli(&self.program, &self.dir, prompt, input, model)
    }
}

/// The credit proxy with the review tools, as a credit run's review calls
/// it, falling back once to `claude -p` after a credit refusal.
struct CreditReviewer {
    cli: CliReviewer,
    notes: Vec<Value>,
}

impl Reviewer for CreditReviewer {
    fn has_tools(&self) -> bool {
        true
    }

    fn call(
        &mut self,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
        tools: Option<&mut RepoTools<'_>>,
        booked_to: &str,
        item: &str,
    ) -> Result<(Value, Option<String>), String> {
        let caller = CreditCaller {
            run_id: booked_to,
            item,
            stage: CreditStage::Review,
            credit_model: None,
        };
        match decision::credit_call(&caller, prompt, input, model, tools) {
            Ok(CreditCall::Answered { reply, cost }) => {
                self.notes
                    .push(json!({"type": "run_credit_call", "run_spent_usd": cost}));
                decision::parse_message(&reply)
            }
            Ok(CreditCall::Refused(reason)) | Err(reason) => {
                self.notes
                    .push(json!({"type": "run_credit_call_fallback", "refusal": reason}));
                self.cli.call(prompt, input, model, None, booked_to, item)
            }
        }
    }

    fn take_notes(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.notes)
    }
}

/// A temporary directory of the replay, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir =
            std::env::temp_dir().join(format!("herdr-review-eval-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A detached git worktree of `repo`, removed (and pruned) when dropped,
/// also when the replay fails or panics.
struct Worktree {
    repo: PathBuf,
    path: PathBuf,
}

impl Worktree {
    fn add(repo: &Path, path: PathBuf, base: &str) -> Result<Self, String> {
        let path_text = path.display().to_string();
        git(
            repo,
            &["worktree", "add", "--quiet", "--detach", &path_text, base],
        )?;
        Ok(Self {
            repo: repo.to_owned(),
            path,
        })
    }

    /// Applies `patch` and commits it with `subject`; the commit's sha.
    fn commit_patch(&self, patch: &Path, subject: &str) -> Result<String, String> {
        let patch = patch.display().to_string();
        git(
            &self.path,
            &["apply", "--index", "--whitespace=nowarn", &patch],
        )?;
        git(
            &self.path,
            &[
                "-c",
                "user.name=herdr review-eval",
                "-c",
                "user.email=review-eval@herdr.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--no-verify",
                "--allow-empty",
                "-m",
                subject,
            ],
        )?;
        Ok(git(&self.path, &["rev-parse", "HEAD"])?.trim().to_owned())
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let path = self.path.display().to_string();
        if git(&self.repo, &["worktree", "remove", "--force", &path]).is_err() {
            let _ = std::fs::remove_dir_all(&self.path);
            let _ = git(&self.repo, &["worktree", "prune"]);
        }
    }
}

/// Whether `finding` matches `expected`: the same file and, when both have
/// a line, within [`LINE_WINDOW`] lines.
fn matches(finding: &Value, expected: &Expected) -> bool {
    let file = finding["file"].as_str().unwrap_or_default();
    let normal = |path: &str| path.trim().trim_start_matches("./").to_owned();
    if normal(file) != normal(&expected.file) {
        return false;
    }
    match (finding["line"].as_u64(), expected.line) {
        (Some(found), Some(line)) => found.abs_diff(line) <= LINE_WINDOW,
        _ => true,
    }
}

/// The mechanical score of the kept findings against the expected entries.
/// The entries with a line take their nearest matching finding first, then
/// the others take any; each finding matches one entry at most.
pub(super) fn score(kept: &[Value], expected: &[Expected]) -> Value {
    let mut used = vec![false; kept.len()];
    let mut matched_by: Vec<Option<usize>> = vec![None; expected.len()];
    let order = (0..expected.len())
        .filter(|&index| expected[index].line.is_some())
        .chain((0..expected.len()).filter(|&index| expected[index].line.is_none()));
    for index in order {
        let entry = &expected[index];
        let distance = |finding: &Value| match (finding["line"].as_u64(), entry.line) {
            (Some(found), Some(line)) => found.abs_diff(line),
            _ => 0,
        };
        let best = (0..kept.len())
            .filter(|&at| !used[at] && matches(&kept[at], entry))
            .min_by_key(|&at| distance(&kept[at]));
        if let Some(at) = best {
            used[at] = true;
            matched_by[index] = Some(at);
        }
    }
    let unmatched: Vec<usize> = (0..kept.len())
        .filter(|&at| !expected.iter().any(|entry| matches(&kept[at], entry)))
        .collect();
    let count = |severe_only: bool, matched_only: bool| {
        (0..expected.len())
            .filter(|&index| !severe_only || expected[index].severe)
            .filter(|&index| !matched_only || matched_by[index].is_some())
            .count()
    };
    let entries: Vec<Value> = expected
        .iter()
        .zip(&matched_by)
        .enumerate()
        .map(|(index, (entry, by))| {
            json!({"entry": index, "file": entry.file, "line": entry.line,
                   "severe": entry.severe, "matched_by_finding": by})
        })
        .collect();
    json!({
        "matched": count(false, true), "expected": count(false, false),
        "matched_severe": count(true, true), "expected_severe": count(true, false),
        "unmatched_findings": unmatched.len(), "unmatched": unmatched, "entries": entries,
    })
}

/// The arm's review input for the case in `tree` at `commit`, and the files
/// the diff's cut left out.
fn case_input(
    tree: &Path,
    case: &Case,
    commit: &str,
    arm: Arm,
    checklist: &(Vec<String>, usize),
) -> (Value, Vec<String>) {
    let item_text = git(tree, &["show", &format!("{}:TODO.md", case.base)])
        .ok()
        .and_then(|todo| todo_titles::item_text(&todo, &case.item))
        .unwrap_or_default();
    let diff_stat = git(tree, &["diff", "--stat", &case.base, commit]).ok();
    assemble_input(InputParts {
        repo: tree,
        base: Some(&case.base),
        decisions_rev: &case.base,
        commit: Some(commit),
        item: &case.item,
        item_text,
        task: &case.task,
        attempt: case.attempt,
        commit_subject: &case.subject,
        allowed_paths: &case.allowed_paths,
        worker_final_text: Some(&case.worker_final_text),
        diff_stat: diff_stat.as_deref(),
        event_note: None,
        history: Vec::new(),
        checks: json!({
            "registered": [],
            "added_when_the_diff_touches_their_paths": [],
            "verify_before_review": NOT_VERIFIED,
        }),
        context_pack: arm != Arm::WithoutContextPack,
        checklist: if arm == Arm::WithoutChecklist {
            (Vec::new(), 0)
        } else {
            checklist.clone()
        },
        user_answer: None,
    })
}

/// One arm of one case: its own worktree, the input, every call, the kept
/// and dropped findings and the score.
#[allow(clippy::too_many_arguments)] // The replay's context, passed as is.
fn replay_arm(
    repo: &Path,
    scratch: &Path,
    name: &str,
    case: &Case,
    arm: Arm,
    checklist: &(Vec<String>, usize),
    model: Option<&str>,
    reviewer: &mut dyn Reviewer,
) -> Result<Value, String> {
    let tree = Worktree::add(
        repo,
        scratch.join(format!("{name}-{}", arm.name())),
        &case.base,
    )?;
    let patch = std::fs::canonicalize(repo.join(GOLDEN_DIR).join(name).join(PATCH_FILE))
        .map_err(|error| format!("case {name}: {PATCH_FILE}: {error}"))?;
    let commit = tree
        .commit_patch(&patch, &case.subject)
        .map_err(|error| format!("case {name}: the patch does not apply at its base: {error}"))?;
    let (input, cut_files) = case_input(&tree.path, case, &commit, arm, checklist);
    let text = serde_json::to_string_pretty(&input).map_err(|error| error.to_string())?;
    if let Some(entry) = case
        .expected
        .iter()
        .find(|entry| text.contains(entry.summary.trim()))
    {
        return Err(format!(
            "case {name}: the input holds an expected entry's text ({}), so it is not blind",
            entry.summary
        ));
    }
    let digest = decision::digest(&text);
    let checklist_len = input["review_checklist"].as_array().map_or(0, Vec::len);
    let booked_to = format!("review-eval-{name}-{}", arm.name());
    let mut tools = reviewer
        .has_tools()
        .then(|| RepoTools::new(&tree.path, &commit))
        .flatten();
    let mut calls = Vec::new();
    let mut kept: Vec<Value> = Vec::new();
    let mut decided = None;
    for call in 1..=MAX_CALLS {
        let tool_calls_before = tools.as_ref().map_or(0, |tools| tools.calls.len());
        let answered = reviewer.call(
            &PROMPT,
            &text,
            model,
            tools.as_mut(),
            &booked_to,
            &case.item,
        );
        let tool_calls: Vec<Value> = tools
            .as_ref()
            .map(|tools| {
                tools.calls[tool_calls_before..]
                    .iter()
                    .map(|tool_call| tool_call.to_json())
                    .collect()
            })
            .unwrap_or_default();
        let mut record = json!({
            "call": call, "input_sha256": digest, "model": model,
            "notes": reviewer.take_notes(), "tool_calls": tool_calls,
        });
        match answered {
            Err(error) => record["error"] = json!(error),
            Ok((output, answered_model)) => {
                record["output"] = output.clone();
                record["answered_model"] = json!(answered_model);
                match parse_decision(&output) {
                    Err(error) => record["error"] = json!(error),
                    Ok(_) => {
                        let (cited, mut dropped) = cite_findings(&output);
                        let (checked, uncheckable) = check_findings(
                            &tree.path,
                            Some(&commit),
                            Some(&case.base),
                            cited,
                            checklist_len,
                        );
                        dropped.extend(uncheckable);
                        record["kept"] = json!(checked);
                        record["dropped"] = json!(dropped);
                        kept = checked;
                        decided = output["action"].as_str().map(str::to_owned);
                    }
                }
            }
        }
        calls.push(record);
        if decided.is_some() {
            break;
        }
    }
    Ok(json!({
        "arm": arm.name(), "replay_commit": commit, "input_sha256": digest,
        "diff_cut_files": cut_files, "has_context_pack": input.get("context_pack").is_some(),
        "has_checklist": input.get("review_checklist").is_some(),
        "tools": tools.is_some(), "calls": calls, "decision": decided,
        "kept": kept, "score": score(&kept, &case.expected),
    }))
}

/// The severe entries of each arm that another arm matched and it missed.
fn severe_regressions(name: &str, case: &Case, arms: &[Value]) -> Vec<Value> {
    let matched =
        |arm: &Value, index: usize| !arm["score"]["entries"][index]["matched_by_finding"].is_null();
    let mut regressions = Vec::new();
    for (index, entry) in case.expected.iter().enumerate().filter(|(_, e)| e.severe) {
        for arm in arms.iter().filter(|arm| !matched(arm, index)) {
            let caught_by: Vec<&Value> = arms
                .iter()
                .filter(|other| matched(other, index))
                .map(|other| &other["arm"])
                .collect();
            if !caught_by.is_empty() {
                regressions.push(json!({
                    "case": name, "arm": arm["arm"], "entry": index, "file": entry.file,
                    "line": entry.line, "caught_by": caught_by,
                }));
            }
        }
    }
    regressions
}

/// Replays the cases of `repo`'s golden set with `reviewer`, in worktrees
/// under `scratch`; the report.
pub(super) fn replay(
    repo: &Path,
    options: &EvalOptions,
    model: Option<&str>,
    reviewer: &mut dyn Reviewer,
    scratch: &Path,
) -> Result<Value, String> {
    let checklist_text = std::fs::read_to_string(repo.join(CHECKLIST_FILE))
        .map_err(|error| format!("cannot read {CHECKLIST_FILE}: {error}"))?;
    let cutoff = checklist_cutoff(&checklist_text)?;
    let mut cases = load_cases(&repo.join(GOLDEN_DIR), cutoff)?;
    if let Some(unknown) = options
        .cases
        .iter()
        .find(|name| !cases.iter().any(|(case, _)| case == *name))
    {
        return Err(format!("no case {unknown} in {GOLDEN_DIR}"));
    }
    if !options.cases.is_empty() {
        cases.retain(|(name, _)| options.cases.contains(name));
    }
    let checklist = checklist_of(repo);
    let arms = options.arms();
    let mut reports = Vec::new();
    let mut regressions = Vec::new();
    let mut totals: Vec<Value> = arms
        .iter()
        .map(|arm| {
            json!({"arm": arm.name(), "matched": 0, "expected": 0, "matched_severe": 0,
                   "expected_severe": 0, "unmatched_findings": 0})
        })
        .collect();
    for (name, case) in &cases {
        if case.patch_omitted {
            reports.push(json!({"case": name, "skipped": case.patch_omitted_reason}));
            continue;
        }
        let mut replayed = Vec::new();
        for arm in &arms {
            replayed.push(replay_arm(
                repo, scratch, name, case, *arm, &checklist, model, reviewer,
            )?);
        }
        for (total, arm) in totals.iter_mut().zip(&replayed) {
            for key in [
                "matched",
                "expected",
                "matched_severe",
                "expected_severe",
                "unmatched_findings",
            ] {
                total[key] = json!(
                    total[key].as_u64().unwrap_or(0) + arm["score"][key].as_u64().unwrap_or(0)
                );
            }
        }
        regressions.extend(severe_regressions(name, case, &replayed));
        reports.push(
            json!({"case": name, "run": case.run, "attempt": case.attempt,
                            "commit": case.commit, "arms": replayed}),
        );
    }
    for total in &mut totals {
        let (matched, expected) = (
            total["matched_severe"].as_u64().unwrap_or(0),
            total["expected_severe"].as_u64().unwrap_or(0),
        );
        total["severe_recall"] = if expected == 0 {
            Value::Null
        } else {
            json!(matched as f64 / expected as f64)
        };
    }
    Ok(json!({
        "checklist_cutoff": cutoff.format(&Rfc3339).unwrap_or_default(),
        "primary_metric": PRIMARY_METRIC,
        "line_window": LINE_WINDOW,
        "model": model,
        "arms": arms.iter().map(|arm| arm.name()).collect::<Vec<_>>(),
        "cases": reports,
        "totals": totals,
        "severe_regressions": regressions,
    }))
}

/// The report's summary for a terminal: per case and arm, then the totals,
/// the primary metric first.
pub(super) fn summary(report: &Value) -> String {
    let ratio = |score: &Value, matched: &str, of: &str| {
        format!(
            "{}/{}",
            score[matched].as_u64().unwrap_or(0),
            score[of].as_u64().unwrap_or(0)
        )
    };
    let mut lines = vec![format!(
        "primary metric: {}",
        report["primary_metric"].as_str().unwrap_or_default()
    )];
    for case in report["cases"].as_array().into_iter().flatten() {
        let name = case["case"].as_str().unwrap_or_default();
        if let Some(reason) = case.get("skipped") {
            lines.push(format!(
                "{name}: skipped ({})",
                reason.as_str().unwrap_or("")
            ));
            continue;
        }
        for arm in case["arms"].as_array().into_iter().flatten() {
            let score = &arm["score"];
            lines.push(format!(
                "{name} [{}]: severe {}, all {}, unmatched findings {}, decision {}",
                arm["arm"].as_str().unwrap_or_default(),
                ratio(score, "matched_severe", "expected_severe"),
                ratio(score, "matched", "expected"),
                score["unmatched_findings"],
                arm["decision"].as_str().unwrap_or("none"),
            ));
        }
    }
    for total in report["totals"].as_array().into_iter().flatten() {
        lines.push(format!(
            "total [{}]: severe recall {} ({}), all {}, unmatched findings {}",
            total["arm"].as_str().unwrap_or_default(),
            ratio(total, "matched_severe", "expected_severe"),
            total["severe_recall"],
            ratio(total, "matched", "expected"),
            total["unmatched_findings"],
        ));
    }
    for regression in report["severe_regressions"]
        .as_array()
        .into_iter()
        .flatten()
    {
        lines.push(format!(
            "severe regression: {} [{}] missed entry {} ({}) that {} caught",
            regression["case"].as_str().unwrap_or_default(),
            regression["arm"].as_str().unwrap_or_default(),
            regression["entry"],
            regression["file"].as_str().unwrap_or_default(),
            regression["caught_by"],
        ));
    }
    lines.join("\n")
}

/// `herdr todo review-eval` in `repo`: the replay with the review's own
/// model call, the report and its summary.
pub(crate) fn review_eval(repo: &Path, options: &EvalOptions) -> Result<(Value, String), String> {
    let scratch = Scratch::new()?;
    let cli = CliReviewer {
        program: PathBuf::from("claude"),
        dir: scratch.0.join("call"),
    };
    std::fs::create_dir_all(&cli.dir).map_err(|error| error.to_string())?;
    let model = review_model();
    let report = if options.credits {
        let mut reviewer = CreditReviewer {
            cli,
            notes: Vec::new(),
        };
        replay(repo, options, model.as_deref(), &mut reviewer, &scratch.0)?
    } else {
        let mut reviewer = cli;
        replay(repo, options, model.as_deref(), &mut reviewer, &scratch.0)?
    };
    let summary = summary(&report);
    Ok((report, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn the_checklist_names_its_cut_off_as_a_timestamp() {
        let text = std::fs::read_to_string(root().join(CHECKLIST_FILE)).unwrap();
        let cutoff = checklist_cutoff(&text).unwrap();
        assert_eq!(
            cutoff,
            OffsetDateTime::parse("2026-10-12T18:46:06+02:00", &Rfc3339).unwrap()
        );
        assert!(checklist_cutoff("# Checklist\n- a line (source: abc1234)\n").is_err());
        assert!(checklist_cutoff("# Cut-off: yesterday\n").is_err());
        assert!(checklist_cutoff("# Cut-off: 2026-10-12 18:46\n").is_err());
    }

    #[test]
    fn every_golden_case_parses_has_its_patch_and_was_caught_after_the_cut_off() {
        let text = std::fs::read_to_string(root().join(CHECKLIST_FILE)).unwrap();
        let cutoff = checklist_cutoff(&text).unwrap();
        let cases = load_cases(&root().join(GOLDEN_DIR), cutoff).unwrap();
        assert!(!cases.is_empty());
        for (name, case) in &cases {
            assert!(!case.expected.is_empty(), "{name}");
            let caught = OffsetDateTime::parse(&case.caught_at, &Rfc3339).unwrap();
            assert!(caught > cutoff, "{name}");
            assert!(
                case.patch_omitted
                    || root()
                        .join(GOLDEN_DIR)
                        .join(name)
                        .join(PATCH_FILE)
                        .is_file(),
                "{name}"
            );
            for entry in &case.expected {
                assert!(
                    !entry.file.is_empty() && !entry.summary.trim().is_empty(),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn a_case_caught_before_the_cut_off_or_without_an_entry_is_refused() {
        let dir = std::env::temp_dir().join(format!("herdr-golden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("c")).unwrap();
        let cutoff = OffsetDateTime::parse("2026-10-12T18:46:06+02:00", &Rfc3339).unwrap();
        let mut case = synthetic_case("b", "c");
        case.caught_at = "2026-10-12T18:00:00+02:00".into();
        std::fs::write(dir.join("c").join(PATCH_FILE), "").unwrap();
        let write = |case: &Case| {
            std::fs::write(
                dir.join("c").join(CASE_FILE),
                serde_json::to_string(case).unwrap(),
            )
            .unwrap()
        };
        write(&case);
        assert!(load_cases(&dir, cutoff).unwrap_err().contains("cut-off"));
        case.caught_at = "2026-10-13T00:00:00+02:00".into();
        case.expected.clear();
        write(&case);
        assert!(load_cases(&dir, cutoff)
            .unwrap_err()
            .contains("no expected"));
        std::fs::write(dir.join("c").join(CASE_FILE), "{").unwrap();
        assert!(load_cases(&dir, cutoff)
            .unwrap_err()
            .contains("does not parse"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn synthetic_case(base: &str, commit: &str) -> Case {
        Case {
            run: "r-test".into(),
            item: "t-abcdefgh".into(),
            attempt: 1,
            base: base.into(),
            commit: commit.into(),
            subject: "feat: change a".into(),
            allowed_paths: vec!["src/**".into()],
            caught_at: "2026-10-13T00:00:00+02:00".into(),
            caught_at_source: "test".into(),
            source: json!({}),
            patch_omitted: false,
            patch_omitted_reason: None,
            task: "Change src/a.rs.".into(),
            worker_final_text: "Done.\nWORKER-DONE abc | changed a".into(),
            expected: vec![
                Expected {
                    file: "src/a.rs".into(),
                    line: Some(30),
                    summary: "SECRET-EXPECTED-ONE the loop never ends".into(),
                    severe: true,
                },
                Expected {
                    file: "src/b.rs".into(),
                    line: None,
                    summary: "SECRET-EXPECTED-TWO b loses its entry".into(),
                    severe: false,
                },
            ],
        }
    }

    /// A reviewer that answers from a script and keeps every input.
    struct Fake {
        answers: Vec<Result<Value, String>>,
        inputs: Vec<String>,
    }

    impl Reviewer for Fake {
        fn has_tools(&self) -> bool {
            false
        }

        fn call(
            &mut self,
            _prompt: &Prompt<'_>,
            input: &str,
            _model: Option<&str>,
            _tools: Option<&mut RepoTools<'_>>,
            _booked_to: &str,
            _item: &str,
        ) -> Result<(Value, Option<String>), String> {
            self.inputs.push(input.to_owned());
            if self.answers.is_empty() {
                return Err("the fake has no answer left".into());
            }
            self.answers
                .remove(0)
                .map(|output| (output, Some("fake-model".into())))
        }
    }

    /// A repository with a checklist, a TODO item and a golden case whose
    /// patch changes src/a.rs and src/b.rs.
    fn golden_repo() -> (PathBuf, String) {
        use super::super::review_tools::tests::repo_with;
        let a: String = (1..=60).map(|n| format!("a{n}\n")).collect();
        let checklist = "# Checklist\n# Cut-off: 2026-10-12T18:46:06+02:00 (test)\n\
                         - A timing assumption. (source: abc1234)\n";
        let (dir, base) = repo_with(&[
            ("src/a.rs", &a),
            ("src/b.rs", "b1\nb2\n"),
            (CHECKLIST_FILE, checklist),
            (
                "TODO.md",
                "# TODO\n\n- [ ] Change a [t-abcdefgh]\n  The item's text.\n",
            ),
        ]);
        let changed: String = (1..=60)
            .map(|n| {
                if n == 30 {
                    "changed\n".to_owned()
                } else {
                    format!("a{n}\n")
                }
            })
            .collect();
        std::fs::write(dir.join("src/a.rs"), changed).unwrap();
        std::fs::write(dir.join("src/b.rs"), "b1\nb2\nb3\n").unwrap();
        let patch = git(&dir, &["diff"]).unwrap();
        git(&dir, &["checkout", "--", "src"]).unwrap();
        let case_dir = dir.join(GOLDEN_DIR).join("r-test-a1");
        std::fs::create_dir_all(&case_dir).unwrap();
        std::fs::write(case_dir.join(PATCH_FILE), patch).unwrap();
        std::fs::write(
            case_dir.join(CASE_FILE),
            serde_json::to_string_pretty(&synthetic_case(&base, "0000000")).unwrap(),
        )
        .unwrap();
        (dir, base)
    }

    fn worktrees(dir: &Path) -> usize {
        git(dir, &["worktree", "list", "--porcelain"])
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count()
    }

    fn finding(file: &str, line: u64) -> Value {
        json!({"file": file, "line": line, "verified_by": "the diff hunk", "text": "t",
               "origin": "free"})
    }

    #[test]
    fn the_replay_is_blind_scores_mechanically_and_removes_its_worktrees() {
        let (dir, base) = golden_repo();
        let scratch = Scratch::new().unwrap();
        let retry = json!({"action": "retry", "review": "fix", "findings": [
            // Matches the severe entry: same file, 2 lines away.
            finding("src/a.rs", 32),
            // Out of range of it: 20 lines away, matches nothing.
            finding("src/a.rs", 50),
            // Matches the entry without a line.
            finding("src/b.rs", 3),
        ]});
        let mut fake = Fake {
            answers: vec![Err("a failed call".into()), Ok(retry)],
            inputs: Vec::new(),
        };
        let options = EvalOptions {
            without_context_pack: true,
            without_checklist: true,
            ..EvalOptions::default()
        };
        let mut answers = vec![
            Ok(json!({"action": "approve"})),
            Ok(json!({"action": "approve"})),
        ];
        // The full arm fails once, then answers; each ablation arm approves.
        fake.answers.append(&mut answers);
        let report = replay(&dir, &options, Some("m"), &mut fake, &scratch.0).unwrap();
        assert_eq!(fake.inputs.len(), 4);
        for input in &fake.inputs {
            assert!(!input.contains("SECRET-EXPECTED"), "the input is not blind");
            assert!(input.contains("not verified"));
            assert!(input.contains("The item's text."));
        }
        let arms = &report["cases"][0]["arms"];
        let full = &arms[0];
        assert_eq!(full["arm"], "full");
        assert_eq!(full["calls"].as_array().unwrap().len(), 2);
        assert_eq!(full["calls"][0]["error"], "a failed call");
        assert_eq!(
            full["calls"][1]["input_sha256"],
            full["calls"][0]["input_sha256"]
        );
        assert_eq!(full["calls"][1]["kept"].as_array().unwrap().len(), 3);
        assert_eq!(full["calls"][1]["kept"][0]["origin"], "free");
        assert!(full["has_context_pack"] == true && full["has_checklist"] == true);
        let score = &full["score"];
        assert_eq!(score["matched"], 2);
        assert_eq!(score["expected"], 2);
        assert_eq!(score["matched_severe"], 1);
        assert_eq!(score["expected_severe"], 1);
        assert_eq!(score["unmatched_findings"], 1);
        assert_eq!(score["unmatched"], json!([1]));
        assert_eq!(score["entries"][0]["matched_by_finding"], 0);
        // Each ablation arm drops its part of the input.
        assert_eq!(arms[1]["arm"], "without_context_pack");
        assert!(arms[1]["has_context_pack"] == false && arms[1]["has_checklist"] == true);
        assert_eq!(arms[2]["arm"], "without_checklist");
        assert!(arms[2]["has_context_pack"] == true && arms[2]["has_checklist"] == false);
        assert!(
            fake.inputs[1].contains("context_pack") && fake.inputs[1].contains("review_checklist")
        );
        assert!(!fake.inputs[2].contains("\"context_pack\""));
        assert!(!fake.inputs[3].contains("\"review_checklist\""));
        // The arms that approved missed the severe entry the full arm caught.
        let regressions = report["severe_regressions"].as_array().unwrap();
        assert_eq!(regressions.len(), 2);
        assert_eq!(regressions[0]["caught_by"], json!(["full"]));
        assert_eq!(report["totals"][0]["severe_recall"], 1.0);
        assert_eq!(report["totals"][1]["severe_recall"], 0.0);
        assert!(summary(&report).contains("severe regression"));
        assert_eq!(worktrees(&dir), 1, "the replay left a worktree");
        // The replay commit is not the case's and lies on the base.
        let commit = full["replay_commit"].as_str().unwrap();
        assert_eq!(
            git(&dir, &["rev-parse", &format!("{commit}^")])
                .unwrap()
                .trim(),
            base
        );
        drop(scratch);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failing_reviewer_records_its_calls_and_an_error_leaves_no_worktree() {
        let (dir, _) = golden_repo();
        let scratch = Scratch::new().unwrap();
        let mut fake = Fake {
            answers: vec![Err("down".into()), Ok(json!({"action": "merge"}))],
            inputs: Vec::new(),
        };
        let report = replay(&dir, &EvalOptions::default(), None, &mut fake, &scratch.0).unwrap();
        let full = &report["cases"][0]["arms"][0];
        assert_eq!(full["calls"].as_array().unwrap().len(), MAX_CALLS);
        assert!(full["calls"][1]["error"]
            .as_str()
            .unwrap()
            .contains("merge"));
        assert_eq!(full["calls"][1]["output"]["action"], "merge");
        assert!(full["decision"].is_null());
        assert_eq!(full["score"]["matched"], 0);
        assert_eq!(worktrees(&dir), 1);
        // A patch that does not apply is an error, and its worktree goes too.
        let patch = dir.join(GOLDEN_DIR).join("r-test-a1").join(PATCH_FILE);
        std::fs::write(&patch, "not a patch\n").unwrap();
        let error = replay(&dir, &EvalOptions::default(), None, &mut fake, &scratch.0).unwrap_err();
        assert!(error.contains("does not apply"), "{error}");
        assert_eq!(worktrees(&dir), 1);
        assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
        let unknown = EvalOptions {
            cases: vec!["nope".into()],
            ..EvalOptions::default()
        };
        assert!(replay(&dir, &unknown, None, &mut fake, &scratch.0)
            .unwrap_err()
            .contains("no case nope"));
        drop(scratch);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_input_holding_an_expected_entry_is_refused_as_not_blind() {
        let (dir, _) = golden_repo();
        let case_file = dir.join(GOLDEN_DIR).join("r-test-a1").join(CASE_FILE);
        let mut case: Case =
            serde_json::from_str(&std::fs::read_to_string(&case_file).unwrap()).unwrap();
        case.task = format!("Change a. {}", case.expected[0].summary);
        std::fs::write(&case_file, serde_json::to_string(&case).unwrap()).unwrap();
        let scratch = Scratch::new().unwrap();
        let mut fake = Fake {
            answers: Vec::new(),
            inputs: Vec::new(),
        };
        let error = replay(&dir, &EvalOptions::default(), None, &mut fake, &scratch.0).unwrap_err();
        assert!(error.contains("not blind"), "{error}");
        assert!(fake.inputs.is_empty());
        assert_eq!(worktrees(&dir), 1);
        drop(scratch);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scoring_matches_each_finding_once_within_the_line_window() {
        let entry = |file: &str, line: Option<u64>, severe: bool| Expected {
            file: file.into(),
            line,
            summary: "s".into(),
            severe,
        };
        let expected = vec![
            entry("a.rs", Some(100), true),
            entry("a.rs", None, false),
            entry("b.rs", Some(10), true),
        ];
        let kept = vec![
            finding("./a.rs", 115),
            finding("b.rs", 26),
            finding("c.rs", 1),
        ];
        let score = score(&kept, &expected);
        // a.rs:115 is exactly 15 lines from 100; the file-only entry finds
        // no finding left; b.rs:26 is 16 lines away.
        assert_eq!(score["matched"], 1);
        assert_eq!(score["matched_severe"], 1);
        assert_eq!(score["entries"][1]["matched_by_finding"], Value::Null);
        assert_eq!(score["unmatched"], json!([1, 2]));
    }
}
