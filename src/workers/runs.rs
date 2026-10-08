//! `herdr todo run`: a deterministic driver for one TODO item. The
//! coordinator chooses intent (the task text, the commit subject, the paths,
//! a registered check); the driver owns execution: preflight, starting a
//! headless worker in the repository's folder slot, turning what needs the
//! coordinator into run events, stopping the worker, verifying its commit
//! with the registered check (typed argv, never a shell string),
//! cherry-picking it onto the repository's `master`, running the registered
//! install, recording the coordinator's note or closing decision in
//! `TODO.md` (with `scripts/todo_edit.py`, committed by path), pushing
//! `master` to `origin` only as a fast-forward and deleting the run's merged
//! branches.
//!
//! A run lives in the worker store: its row in `runs` and its `run_*`
//! events, written in one transaction. Every side effect is recorded as an
//! intent before it and its result after it, and each step can run again
//! after a crash: the start and the stop carry command ids derived from the
//! run, the attention wait is level-triggered from the run's acknowledged
//! seq, the verify only reads, the cherry-pick first asks git whether
//! `master` already has the commit, the install first asks the registered
//! build id whether `master` is installed already, the TODO step looks for
//! its commit on `master` and the push compares `origin`'s `master` with
//! the local one. A server that starts drives every run in progress again
//! from its step ([`resume_runs_at_start`]).
//!
//! The run waits for the coordinator on its `run_event`s (a question the
//! worker policy left, the turn's end, a failed verify, a worker still alive
//! after its stop, a failed install, TODO edit or push): `todo.wait` returns the pending one with its evidence and
//! allowed actions, and `todo.resume` must name it; an answer to any other
//! event is refused as stale. No timer moves a run: only worker events and
//! the coordinator's answers do. A worker that ignores its stop is never
//! killed on a timer: the coordinator force-stops it.
//!
//! One server drives a run at a time: its driver holds the run's lock file
//! next to the store ([`WorkerSupervisor::take_run_lock`]). After a live
//! handoff the old server's drivers let go ([`WorkerSupervisor::let_go_of_runs`])
//! and the new server's driver takes the lock once the old one released it
//! (or ended, which releases it too).
//!
//! The checks, the install and the push run with the caller's environment,
//! which `todo.run` and every `todo.resume` send and the server keeps in
//! memory only: it may hold credentials. A check without it (a restart, a
//! resume that sent none) is `unavailable`, and an install or push without
//! it fails with a resume as the remedy; none of them runs in the server's
//! environment.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::verify::{tail, CheckCommand};
use super::{lock, now_ms, repository_of, todo_titles, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    TodoAction, TodoEventKind, TodoResumeParams, TodoRunEvent, TodoRunInfo, TodoRunParams,
    TodoRunStatus, TodoStep, TodoWaitParams, WorkerAnswerParams, WorkerAttentionReason,
    WorkerCommandTarget, WorkerInfo, WorkerKillParams, WorkerQuestion, WorkerQuestionState,
    WorkerStartParams, WorkerState, WorkerVerdict, WorkerVerifyParams, WorkerWaitUntil,
};

mod finish;

/// A run's attempts: the first worker and two retries; a retry asked after
/// the third blocks the run.
const MAX_ATTEMPTS: u32 = 3;
/// The folder slot every run's worker uses.
const SLOT: &str = "worker";
/// The repository's registered checks, relative to its checkout.
pub(super) const CHECKS_FILE: &str = ".herdr/checks.toml";
/// The free disk preflight asks for when the checks file names none: the
/// threshold `just guard` uses before a build.
const DEFAULT_MIN_FREE_GIB: f64 = 15.0;
/// A driver's wait on its worker has no deadline: the worker's events end
/// it, and a handoff wakes it ([`WorkerSupervisor::let_go_of_runs`]).
const NO_DEADLINE: Duration = Duration::MAX;
/// The TODO editor the todo step runs, relative to the repository.
const TODO_EDIT: &str = "scripts/todo_edit.py";
const TODO_FILE: &str = "TODO.md";
const DECISIONS_FILE: &str = "DECISIONS.md";

/// A run as the store holds it: what `todo.status` shows, and what it does
/// not (the checks' argv, the owner).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Run {
    pub(super) info: TodoRunInfo,
    /// The registered checks as preflight read them, in order.
    pub(super) checks: Vec<RunCheck>,
    pub(super) owner_pane: Option<String>,
    pub(super) owner_session: Option<String>,
    pub(super) finish: RunFinish,
}

/// What a run does after its cherry-pick, stored with it (`runs.finish`):
/// the registered install as preflight read it, the coordinator's TODO note
/// or closing decision, and the results [`TodoRunInfo`] shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RunFinish {
    /// None skips the install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) install: Option<InstallCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) installed_build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) todo_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pushed: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) kept_branches: Vec<String>,
}

/// `[install]` of `.herdr/checks.toml`: the program and arguments that
/// install `master` (no shell), and optionally one whose first output line
/// names the build that is installed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallCommand {
    pub(super) command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) build_id: Vec<String>,
}

impl Run {
    /// The finish column as the store writes it: the stored part with the
    /// results the run's info holds now.
    pub(super) fn finish_with_results(&self) -> RunFinish {
        RunFinish {
            installed_build: self.info.installed_build.clone(),
            todo_commit: self.info.todo_commit.clone(),
            pushed: self.info.pushed.clone(),
            kept_branches: self.info.kept_branches.clone(),
            ..self.finish.clone()
        }
    }
}

/// A registered check of a run: its name and its argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RunCheck {
    pub(super) name: String,
    pub(super) argv: Vec<String>,
}

/// `.herdr/checks.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecksFile {
    /// Name → argv: the program and its arguments, run without a shell.
    #[serde(default)]
    checks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    preflight: PreflightConfig,
    /// The command the run installs `master` with after the cherry-pick;
    /// none skips the install.
    #[serde(default)]
    install: Option<InstallCommand>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreflightConfig {
    /// The free disk, in GiB, below which preflight refuses a run.
    min_free_gib: Option<f64>,
}

/// Bumped and announced after every run write, so `todo.wait` wakes.
struct Changes {
    generation: Mutex<u64>,
    changed: Condvar,
}

static CHANGES: Changes = Changes {
    generation: Mutex::new(0),
    changed: Condvar::new(),
};

fn announce() {
    *lock(&CHANGES.generation) += 1;
    CHANGES.changed.notify_all();
    super::notify_clients();
}

/// The callers' environments of the runs this server drives, which their
/// checks run with: the last one `todo.run` or `todo.resume` sent. Never
/// stored: it may hold credentials. A live handoff carries them to the new
/// server in memory ([`envs_for_handoff`], [`restore_handed_off_envs`]);
/// after a restart there are none, and a check is `unavailable`.
static RUN_ENV: Mutex<BTreeMap<String, HashMap<String, String>>> = Mutex::new(BTreeMap::new());

/// The runs' caller environments, for the live handoff's payload: sent to
/// the new server over the handoff's socket, never written anywhere.
#[cfg(unix)]
pub(crate) fn envs_for_handoff() -> BTreeMap<String, HashMap<String, String>> {
    lock(&RUN_ENV).clone()
}

/// Takes the runs' caller environments the old server of a live handoff
/// sent, before this server resumes its runs. An environment a run already
/// has here is newer and stays.
#[cfg(unix)]
pub(crate) fn restore_handed_off_envs(envs: BTreeMap<String, HashMap<String, String>>) {
    let mut run_env = lock(&RUN_ENV);
    for (run_id, env) in envs {
        run_env.entry(run_id).or_insert(env);
    }
}

/// The runs a driver of this process works on, so one runs at a time,
/// each with whether another driver was asked for meanwhile.
static DRIVING: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());

struct Driving {
    run_id: String,
    released: bool,
}

impl Driving {
    /// The run's claim; none while another driver holds it, which is then
    /// asked to look at the run again before it lets go.
    fn claim(run_id: &str) -> Option<Self> {
        let mut driving = lock(&DRIVING);
        match driving.get_mut(run_id) {
            Some(again) => {
                *again = true;
                None
            }
            None => {
                driving.insert(run_id.to_owned(), false);
                Some(Self {
                    run_id: run_id.to_owned(),
                    released: false,
                })
            }
        }
    }

    /// Lets go of the claim, unless another driver was asked for since the
    /// run was last read: then `false`, and the holder reads it again.
    fn release(&mut self) -> bool {
        let mut driving = lock(&DRIVING);
        match driving.get_mut(&self.run_id) {
            Some(again) if *again => {
                *again = false;
                false
            }
            _ => {
                driving.remove(&self.run_id);
                drop(driving);
                self.released = true;
                #[cfg(test)]
                claim_released();
                true
            }
        }
    }
}

impl Drop for Driving {
    fn drop(&mut self) {
        if !self.released {
            lock(&DRIVING).remove(&self.run_id);
            #[cfg(test)]
            claim_released();
        }
    }
}

/// Test only: wakes [`wait_undriven`] when a driver lets go of its claim.
#[cfg(test)]
fn claim_released() {
    *lock(&CHANGES.generation) += 1;
    CHANGES.changed.notify_all();
}

/// Test only: blocks until no driver of this process claims the run, woken
/// when one lets go. A driver that recorded the event a test waited for
/// reads the run once more before it lets go; a test that resumes the run
/// and plans a crash before that would have two drivers, and the one the
/// crash missed drives on, which a real crash would have ended too.
#[cfg(all(test, unix))]
pub(super) fn wait_undriven(run_id: &str, hang_guard: Duration) {
    let started = std::time::Instant::now();
    let mut generation = lock(&CHANGES.generation);
    while lock(&DRIVING).contains_key(run_id) {
        assert!(
            started.elapsed() < hang_guard,
            "the driver of {run_id} hung"
        );
        generation = CHANGES
            .changed
            .wait_timeout(generation, Duration::from_millis(100))
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
}

/// Test only: the driver of a run of this repository returns before it
/// starts that step, as a server that ended there would.
#[cfg(test)]
static CRASH_BEFORE: Mutex<Vec<(String, TodoStep)>> = Mutex::new(Vec::new());

#[cfg(all(test, unix))]
pub(super) fn crash_before(repo: &str, step: TodoStep) {
    lock(&CRASH_BEFORE).push((repo.to_owned(), step));
}

/// Test only: the server forgets the run's caller environment, as a
/// restart does.
#[cfg(all(test, unix))]
pub(super) fn forget_env(run_id: &str) {
    lock(&RUN_ENV).remove(run_id);
}

/// Test only: the repositories whose driver returned at a planned crash.
#[cfg(test)]
static CRASHED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Test only: blocks until a driver of `repo` returned at a planned crash,
/// woken by its announcement.
#[cfg(all(test, unix))]
pub(super) fn wait_crashed(repo: &str, hang_guard: Duration) {
    let started = std::time::Instant::now();
    let mut generation = lock(&CHANGES.generation);
    loop {
        {
            let mut crashed = lock(&CRASHED);
            if let Some(index) = crashed.iter().position(|at| at == repo) {
                crashed.remove(index);
                return;
            }
        }
        assert!(started.elapsed() < hang_guard, "no crash of {repo}");
        generation = CHANGES
            .changed
            .wait_timeout(generation, Duration::from_millis(100))
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
}

/// Test only: the driver of a run of this repository returns right after
/// that step's side effect, before it records the result.
#[cfg(test)]
static CRASH_AFTER: Mutex<Vec<(String, TodoStep)>> = Mutex::new(Vec::new());

#[cfg(all(test, unix))]
pub(super) fn crash_after(repo: &str, step: TodoStep) {
    lock(&CRASH_AFTER).push((repo.to_owned(), step));
}

/// What a step returns when it returned at a planned crash.
#[cfg(test)]
const CRASHED_HERE: &str = "\0crashed";

#[cfg(test)]
fn crashes_after(repo: &str, step: TodoStep) -> Result<(), String> {
    let mut crashes = lock(&CRASH_AFTER);
    match crashes
        .iter()
        .position(|(at, after)| at == repo && *after == step)
    {
        Some(index) => {
            crashes.remove(index);
            Err(CRASHED_HERE.to_owned())
        }
        None => Ok(()),
    }
}

#[cfg(test)]
fn crashes_before(repo: &str, step: TodoStep) -> bool {
    let mut crashes = lock(&CRASH_BEFORE);
    match crashes
        .iter()
        .position(|(at, before)| at == repo && *before == step)
    {
        Some(index) => {
            crashes.remove(index);
            true
        }
        None => false,
    }
}

/// A new run id: `r-` and 8 lowercase base32 characters, from a hash of the
/// time, this process, a counter and the item.
fn new_run_id(repo: &str, item: &str) -> String {
    static RUNS: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let count = RUNS.fetch_add(1, Ordering::Relaxed);
    let digest = Sha256::digest(format!(
        "{nanos}:{}:{count}:{repo}:{item}",
        std::process::id()
    ));
    let bits = digest[..5]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let id: String = (0..8)
        .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 31) as usize] as char)
        .collect();
    format!("r-{id}")
}

/// The repository's folder slot, beside it as [`super::slot`] places it.
fn slot_dir(repo: &Path) -> Option<PathBuf> {
    repo.parent()
        .map(|parent| parent.join("herdr-worktrees").join(SLOT))
}

/// The attempt's branch: with the run's id, so a later run of the same
/// item never meets a branch an earlier one left.
fn branch_of(item: &str, run_id: &str, attempt: u32) -> String {
    format!("todo/{item}-{run_id}-{attempt}")
}

/// The commit types a subject may start with.
const COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "docs", "refactor", "test", "chore", "perf", "build", "ci", "style", "revert",
];

/// A lowercase conventional commit subject: `type(scope)!: description`,
/// one line, the scope optional, the description starting with no capital.
fn check_message(message: &str) -> Result<(), String> {
    if message.is_empty() || message.chars().any(char::is_control) {
        return Err("the message must be one nonempty line".into());
    }
    if message.trim() != message {
        return Err("the message must not start or end with spaces".into());
    }
    let (head, description) = message
        .split_once(": ")
        .ok_or("the message must read `type: description`")?;
    let head = head.strip_suffix('!').unwrap_or(head);
    let (kind, scope) = match head.split_once('(') {
        Some((kind, rest)) => (
            kind,
            Some(
                rest.strip_suffix(')')
                    .ok_or("the message's scope must close with `)`")?,
            ),
        ),
        None => (head, None),
    };
    if !COMMIT_TYPES.contains(&kind) {
        return Err(format!(
            "the message's type {kind:?} is not one of {}",
            COMMIT_TYPES.join(", ")
        ));
    }
    if let Some(scope) = scope {
        let valid = !scope.is_empty()
            && scope.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '/' | '.')
            });
        if !valid {
            return Err(format!("the message's scope {scope:?} must be lowercase"));
        }
    }
    match description.chars().next() {
        None | Some(' ') => Err("the message needs a description after `: `".into()),
        Some(first) if first.is_uppercase() => {
            Err("the message's description must start in lowercase".into())
        }
        Some(_) => Ok(()),
    }
}

/// Relative git glob pathspecs: no absolute path, no `..`, no pathspec
/// magic (`:`), nothing empty.
fn check_paths(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("the run needs at least one path glob".into());
    }
    for path in paths {
        let bad = path.trim().is_empty()
            || path.trim() != path
            || path.chars().any(char::is_control)
            || path.starts_with('/')
            || path.starts_with(':')
            || path.contains('\\')
            || path.split('/').any(|part| part == "..");
        if bad {
            return Err(format!(
                "path {path:?} is not a relative git glob (`src/**`, `AGENTS.md`)"
            ));
        }
    }
    Ok(())
}

fn read_checks(repo: &Path) -> Result<ChecksFile, String> {
    let path = repo.join(CHECKS_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// The free space of the file system `dir` is on, in GiB, from `df -Pk`.
fn free_gib(dir: &Path) -> Result<f64, String> {
    let output = Command::new("df")
        .arg("-Pk")
        .arg(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run df: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let available = text
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|kib| kib.parse::<f64>().ok())
        .ok_or_else(|| {
            format!(
                "cannot read the free disk space of {}: {}",
                dir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
    Ok(available / (1024.0 * 1024.0))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_with(dir, args, None)
}

/// A command with `env` (without `HERDR_*`) in place of the server's
/// environment when given.
fn command_with(program: &str, env: Option<&HashMap<String, String>>) -> Command {
    let mut command = Command::new(program);
    if let Some(env) = env {
        command
            .env_clear()
            .envs(env.iter().filter(|(name, _)| !name.starts_with("HERDR_")));
    }
    command
}

/// `git` in `dir`, with `env` in place of the server's environment when
/// given (a push or a fetch needs the caller's credentials).
fn git_with(
    dir: &Path,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
) -> Result<String, String> {
    let output = command_with("git", env)
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The task a run hands its worker: the coordinator's text, then the
/// contract the verify checks, so the worker does not invent its own: the
/// exact commit subject with no body or trailers, the paths it may touch
/// and its last line.
fn worker_task(run: &TodoRunInfo) -> String {
    format!(
        "{}\n\n---\nCommit your work as exactly one commit whose message is exactly this \
         subject, with no body and no trailers:\n{}\nTouch only these paths (git globs): \
         {}\nEnd your last reply with the line `WORKER-DONE <sha> | <summary>`, or \
         `WORKER-BLOCKED <reason>` when you cannot finish.\n",
        run.task.trim_end(),
        run.message,
        run.paths.join(" "),
    )
}

fn is_gone(worker: &WorkerInfo) -> bool {
    matches!(worker.state, WorkerState::Exited | WorkerState::Lost)
}

fn pending_questions(questions: &[WorkerQuestion]) -> Vec<WorkerQuestion> {
    questions
        .iter()
        .filter(|question| question.state == WorkerQuestionState::Pending)
        .cloned()
        .collect()
}

/// A step's wire name (`cherry_pick`).
fn step_name(step: TodoStep) -> String {
    serde_json::to_value(step)
        .ok()
        .and_then(|step| step.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What `todo.resume` takes for an event of `kind` while it is pending (or,
/// for `blocked`, while it is the blocked run's last event): every one of
/// them also takes `abort`.
fn actions_for(kind: TodoEventKind) -> Vec<TodoAction> {
    match kind {
        TodoEventKind::Question => vec![TodoAction::Answer, TodoAction::Abort],
        TodoEventKind::Review => vec![TodoAction::Approve, TodoAction::Retry, TodoAction::Abort],
        TodoEventKind::VerifyFailed => {
            vec![TodoAction::Retry, TodoAction::Verify, TodoAction::Abort]
        }
        TodoEventKind::StillAlive => vec![TodoAction::ForceStop, TodoAction::Abort],
        TodoEventKind::InstallFailed => vec![
            TodoAction::RetryInstall,
            TodoAction::SkipInstall,
            TodoAction::Abort,
        ],
        TodoEventKind::TodoFailed => {
            vec![
                TodoAction::RetryTodo,
                TodoAction::SkipTodo,
                TodoAction::Abort,
            ]
        }
        TodoEventKind::PushFailed => vec![TodoAction::RetryPush, TodoAction::Abort],
        TodoEventKind::Blocked => vec![TodoAction::Abort],
        _ => Vec::new(),
    }
}

fn new_event(kind: TodoEventKind) -> TodoRunEvent {
    TodoRunEvent {
        event_id: 0,
        kind,
        actions: Vec::new(),
        questions: Vec::new(),
        diff_stat: None,
        commits: Vec::new(),
        result_text: None,
        verification: None,
        error: None,
        ts_ms: now_ms(),
    }
}

/// A stored `run_event` as `todo.wait` returns it: its id, and its actions
/// only while the run waits on it, or while it is the `blocked` event of a
/// run that is still blocked (the run's latest event, as every caller
/// passes).
fn event_of(seq: i64, body: &Value, run: &TodoRunInfo) -> TodoRunEvent {
    let mut event: TodoRunEvent = serde_json::from_value(body["event"].clone())
        .unwrap_or_else(|_| new_event(TodoEventKind::Unknown));
    event.event_id = seq;
    let open = (run.status == TodoRunStatus::Waiting && run.pending_event == Some(seq))
        || (run.status == TodoRunStatus::Blocked && event.kind == TodoEventKind::Blocked);
    event.actions = if open {
        actions_for(event.kind)
    } else {
        Vec::new()
    };
    event
}

fn store_error(error: rusqlite::Error) -> WorkerError {
    WorkerError::Io(std::io::Error::other(format!(
        "the worker store failed: {error}"
    )))
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// What preflight found.
struct Preflighted {
    base: String,
    checks: Vec<RunCheck>,
    install: Option<InstallCommand>,
}

impl WorkerSupervisor {
    fn run_store(&self) -> Result<&super::store::Store, WorkerError> {
        self.shared
            .store
            .as_ref()
            .map_err(|error| WorkerError::Io(std::io::Error::other(error.clone())))
    }

    fn load_run(&self, run_id: &str) -> Result<Run, WorkerError> {
        self.run_store()?
            .run(run_id)
            .map_err(store_error)?
            .ok_or_else(|| WorkerError::RunNotFound(format!("run {run_id} not found")))
    }

    /// Starts a run of `params.item`: preflight, then a driver that takes it
    /// from the worker's start on. A refused preflight starts nothing.
    pub(crate) fn todo_run(&self, params: TodoRunParams) -> Result<TodoRunInfo, WorkerError> {
        let cwd = Path::new(&params.cwd);
        if !cwd.is_absolute() {
            return Err(WorkerError::Invalid(format!(
                "cwd must be absolute: {}",
                params.cwd
            )));
        }
        let repo = repository_of(cwd).ok_or_else(|| {
            WorkerError::Preflight(format!("{} is not in a git repository", params.cwd))
        })?;
        let store = self.run_store()?;
        if let Some(active) = store.active_run(&repo).map_err(store_error)? {
            return Err(Self::run_active(&active));
        }
        let preflighted = self.preflight(&params, Path::new(&repo))?;
        let at = now_ms();
        let run_id = new_run_id(&repo, &params.item);
        let mut run = Run {
            info: TodoRunInfo {
                run_id: run_id.clone(),
                repo: repo.clone(),
                item: params.item.clone(),
                step: TodoStep::Start,
                status: TodoRunStatus::Running,
                attempt: 1,
                base: Some(preflighted.base.clone()),
                worker_id: None,
                branch: Some(branch_of(&params.item, &run_id, 1)),
                task: params.task.clone(),
                message: params.message.clone(),
                paths: params.paths.clone(),
                checks: params.checks.clone(),
                last_acked_seq: None,
                pending_event: None,
                error: None,
                picked: None,
                created_ms: at,
                updated_ms: at,
                installed_build: None,
                todo_commit: None,
                pushed: None,
                kept_branches: Vec::new(),
            },
            checks: preflighted.checks.clone(),
            owner_pane: params.owner_pane_id.clone(),
            owner_session: params.owner_session_id.clone(),
            finish: RunFinish {
                install: preflighted.install.clone(),
                ..RunFinish::default()
            },
        };
        let event = json!({
            "type": "run_created",
            "item": params.item,
            "base": preflighted.base,
            "checks": preflighted.checks,
            "install": preflighted.install,
            "message": params.message,
            "paths": params.paths,
            "task": params.task,
            "step": run.info.step,
        });
        match store.transaction(|tx| tx.run_event(&mut run, &event, false, at)) {
            Ok(_) => {}
            // Another run of the repository committed since the check.
            Err(error) if is_unique_violation(&error) => {
                return Err(match store.active_run(&repo) {
                    Ok(Some(active)) => Self::run_active(&active),
                    _ => WorkerError::RunActive(format!(
                        "repository {repo} already has a run in progress"
                    )),
                });
            }
            Err(error) => return Err(store_error(error)),
        }
        if let Some(env) = params.env {
            lock(&RUN_ENV).insert(run_id.clone(), env);
        }
        announce();
        self.spawn_driver(&run_id);
        Ok(run.info)
    }

    fn run_active(active: &Run) -> WorkerError {
        WorkerError::RunActive(format!(
            "repository {} already has run {} of {} in progress ({}); `herdr todo status {}` shows it",
            active.info.repo,
            active.info.run_id,
            active.info.item,
            step_name(active.info.step),
            active.info.run_id
        ))
    }

    /// Checks everything a run needs before it starts anything: the task,
    /// the message, the paths, the item in `TODO.md`, the registered check,
    /// the free disk, a free and clean folder slot, and `master`'s commit.
    fn preflight(&self, params: &TodoRunParams, repo: &Path) -> Result<Preflighted, WorkerError> {
        let refuse = |why: String| WorkerError::Preflight(format!("preflight: {why}"));
        super::check_item_id(&params.item)?;
        if params.task.trim().is_empty() {
            return Err(refuse("the task text is empty".into()));
        }
        check_message(&params.message).map_err(refuse)?;
        check_paths(&params.paths).map_err(refuse)?;
        if !todo_titles::read_titles(repo).contains_key(&params.item) {
            return Err(refuse(format!(
                "item {} is not in {}",
                params.item,
                repo.join("TODO.md").display()
            )));
        }
        let checks = read_checks(repo).map_err(refuse)?;
        if params.checks.is_empty() {
            return Err(refuse(format!(
                "the run needs at least one check of {CHECKS_FILE}"
            )));
        }
        let mut registered = Vec::new();
        for name in &params.checks {
            if registered
                .iter()
                .any(|check: &RunCheck| &check.name == name)
            {
                return Err(refuse(format!("check {name:?} is named twice")));
            }
            let argv = checks.checks.get(name).cloned().ok_or_else(|| {
                refuse(format!(
                    "no check {name:?} in {CHECKS_FILE}; it has: {}",
                    checks.checks.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
            })?;
            if argv.first().is_none_or(|program| program.is_empty()) {
                return Err(refuse(format!(
                    "check {name:?} in {CHECKS_FILE} has no program"
                )));
            }
            registered.push(RunCheck {
                name: name.clone(),
                argv,
            });
        }
        if let Some(install) = &checks.install {
            if install
                .command
                .first()
                .is_none_or(|program| program.is_empty())
            {
                return Err(refuse(format!("[install] in {CHECKS_FILE} has no program")));
            }
            if install
                .build_id
                .first()
                .is_some_and(|program| program.is_empty())
            {
                return Err(refuse(format!(
                    "[install] build_id in {CHECKS_FILE} has no program"
                )));
            }
        }
        let min_free = checks
            .preflight
            .min_free_gib
            .unwrap_or(DEFAULT_MIN_FREE_GIB);
        let free = free_gib(repo).map_err(refuse)?;
        if free < min_free {
            return Err(refuse(format!(
                "{free:.1} GiB free on {}, less than {min_free} GiB",
                repo.display()
            )));
        }
        self.check_slot_free(repo).map_err(refuse)?;
        let base = git(repo, &["rev-parse", "--verify", "master^{commit}"])
            .map_err(|error| refuse(format!("the repository has no master commit: {error}")))?
            .trim()
            .to_owned();
        Ok(Preflighted {
            base,
            checks: registered,
            install: checks.install,
        })
    }

    /// The folder slot is free: no worker of this server runs in it, and
    /// it has nothing uncommitted. A slot not created yet is free.
    fn check_slot_free(&self, repo: &Path) -> Result<(), String> {
        let Some(slot) = slot_dir(repo) else {
            return Err(format!("{} has no parent directory", repo.display()));
        };
        let Ok(real) = slot.canonicalize() else {
            return Ok(());
        };
        let busy: Vec<String> = {
            let registry = lock(&self.shared.registry);
            registry
                .workers
                .values()
                .filter(|entry| !entry.status.process_gone())
                .filter(|entry| {
                    Path::new(&entry.status.cwd)
                        .canonicalize()
                        .is_ok_and(|cwd| cwd == real)
                })
                .map(|entry| entry.status.worker_id.clone())
                .collect()
        };
        if !busy.is_empty() {
            return Err(format!(
                "folder slot {} is busy: {} runs there; stop it first",
                real.display(),
                busy.join(", ")
            ));
        }
        let status = git(&real, &["status", "--porcelain"])?;
        if !status.trim().is_empty() {
            return Err(format!(
                "folder slot {} has uncommitted changes:\n{}",
                real.display(),
                status.trim_end()
            ));
        }
        Ok(())
    }

    /// The run as it is now. Asked while the run waits for its worker's
    /// exit after the stop and the worker's process is still alive (its
    /// stop is recorded, its exit is not), it first raises a `still_alive`
    /// event the run then waits on: the coordinator's question is the
    /// observable fact, no clock is. `todo.wait` does not raise it, so a
    /// wait right after `approve` does not report a stop still in flight.
    pub(crate) fn todo_status(&self, run_id: &str) -> Result<TodoRunInfo, WorkerError> {
        let run = self.load_run(run_id)?;
        if !Self::stopping(&run) {
            return Ok(run.info);
        }
        let Some(worker_id) = run.info.worker_id.clone() else {
            return Ok(run.info);
        };
        let worker = self.status(&worker_id)?;
        if is_gone(&worker) || worker.stop_requested_ms.is_none() {
            return Ok(run.info);
        }
        let mut event = new_event(TodoEventKind::StillAlive);
        event.error = Some(format!(
            "worker {worker_id} has not exited since its stop (SIGTERM); force-stop it with \
             SIGKILL, or wait for it to exit"
        ));
        let store = self.run_store()?;
        let raised = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(run_id)? else {
                    return Ok(None);
                };
                // The driver moved on (the worker exited) or it was raised.
                if !Self::stopping(&current) || current.info.worker_id != run.info.worker_id {
                    return Ok(Some((current, false)));
                }
                current.info.status = TodoRunStatus::Waiting;
                let body = json!({
                    "type": "run_event",
                    "kind": event.kind,
                    "step": current.info.step,
                    "status": current.info.status,
                    "attempt": current.info.attempt,
                    "event": event,
                });
                tx.run_event(&mut current, &body, true, now_ms())?;
                Ok(Some((current, true)))
            })
            .map_err(store_error)?;
        let Some((current, raised)) = raised else {
            return Ok(run.info);
        };
        if raised {
            announce();
        }
        Ok(current.info)
    }

    /// Whether the run's driver is at its stop, waiting for the worker's
    /// exit.
    fn stopping(run: &Run) -> bool {
        run.info.status == TodoRunStatus::Running
            && matches!(
                run.info.step,
                TodoStep::Stop | TodoStep::Restart | TodoStep::Abort
            )
    }

    /// The runs, of the repository `repo` is in when given, oldest first.
    pub(crate) fn todo_runs(&self, repo: Option<&str>) -> Result<Vec<TodoRunInfo>, WorkerError> {
        let repo = repo.map(|dir| repository_of(Path::new(dir)).unwrap_or_else(|| dir.to_owned()));
        Ok(self
            .run_store()?
            .runs(repo.as_deref())
            .map_err(store_error)?
            .into_iter()
            .map(|run| run.info)
            .collect())
    }

    /// Blocks until the run waits on an event after `after`, or has ended
    /// (then its last event, however old). Woken by the run's writes; the
    /// liveness check only lets a caller that went away give up. Returns
    /// `None` then.
    pub(crate) fn todo_wait(
        &self,
        params: &TodoWaitParams,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<(TodoRunEvent, TodoRunInfo)>, WorkerError> {
        let store = self.run_store()?;
        let mut generation = lock(&CHANGES.generation);
        loop {
            let run = self.load_run(&params.run_id)?;
            if let Some((seq, body)) = store
                .latest_run_event(&params.run_id)
                .map_err(store_error)?
            {
                let ended = matches!(
                    run.info.status,
                    TodoRunStatus::Done | TodoRunStatus::Blocked | TodoRunStatus::Aborted
                );
                let waits_on = run.info.status == TodoRunStatus::Waiting
                    && run.info.pending_event == Some(seq);
                let fresh = params.after.is_none_or(|after| seq > after);
                if ended || (waits_on && fresh) {
                    return Ok(Some((event_of(seq, &body, &run.info), run.info)));
                }
            }
            let seen = *generation;
            let (guard, waited) = CHANGES
                .changed
                .wait_timeout(generation, liveness_check)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            generation = guard;
            if waited.timed_out() && *generation == seen && !keep_waiting() {
                return Ok(None);
            }
        }
    }

    /// The coordinator's answer to the run's pending event: `answer` sends
    /// the worker the answer and keeps waiting on it, `approve` takes the
    /// turn to the stop, verify and cherry-pick, `retry` starts the next
    /// attempt with new task text (after the third, the run is blocked once
    /// the worker stopped), `verify` runs the verify again, `force-stop`
    /// SIGKILLs a worker still alive after its stop, `abort` takes the run
    /// to its abort step, which stops the worker and ends the run
    /// `aborted`. An event that is not the pending one (for a blocked run:
    /// its `blocked` event, which takes only `abort`) is refused as stale.
    /// The environment it carries replaces the one the run's checks run
    /// with; none leaves them none.
    pub(crate) fn todo_resume(&self, params: TodoResumeParams) -> Result<TodoRunInfo, WorkerError> {
        let run = self.load_run(&params.run_id)?;
        let store = self.run_store()?;
        let latest = store
            .latest_run_event(&params.run_id)
            .map_err(store_error)?;
        // A blocked run gets no event after its `blocked` one: no driver
        // writes it, so this stays its last event until an abort.
        let blocked_event = latest.as_ref().map(|(seq, _)| *seq);
        let stale = |run: &Run| {
            let open = match run.info.status {
                TodoRunStatus::Waiting => run.info.pending_event == Some(params.event),
                TodoRunStatus::Blocked => blocked_event == Some(params.event),
                _ => false,
            };
            if open {
                return None;
            }
            Some(WorkerError::EventStale(
                match (run.info.status, run.info.pending_event) {
                    (TodoRunStatus::Waiting, Some(pending)) => format!(
                        "event {} is not the one run {} waits on ({pending})",
                        params.event, run.info.run_id
                    ),
                    (TodoRunStatus::Blocked, _) => format!(
                        "event {} is not run {}'s blocked event ({})",
                        params.event,
                        run.info.run_id,
                        blocked_event.map(|seq| seq.to_string()).unwrap_or_default()
                    ),
                    _ => format!(
                        "run {} waits on no event; event {} is stale",
                        run.info.run_id, params.event
                    ),
                },
            ))
        };
        if let Some(error) = stale(&run) {
            return Err(error);
        }
        let kind = latest
            .filter(|(seq, _)| *seq == params.event)
            .map(|(seq, body)| event_of(seq, &body, &run.info).kind)
            .unwrap_or(TodoEventKind::Unknown);
        if !actions_for(kind).contains(&params.action) {
            return Err(WorkerError::Invalid(format!(
                "event {} takes {}",
                params.event,
                actions_for(kind)
                    .iter()
                    .filter_map(|action| serde_json::to_value(action).ok())
                    .filter_map(|action| action.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join(" or ")
            )));
        }
        if params.note.is_some() || params.close.is_some() {
            if params.action != TodoAction::Approve {
                return Err(WorkerError::Invalid(
                    "only approve takes a TODO note or a closing decision".into(),
                ));
            }
            if params.note.is_some() && params.close.is_some() {
                return Err(WorkerError::Invalid(
                    "a TODO note and a closing decision exclude each other".into(),
                ));
            }
            let text = params.note.as_deref().or(params.close.as_deref());
            if text.is_some_and(|text| text.trim().is_empty()) {
                return Err(WorkerError::Invalid(
                    "the TODO note or closing decision is empty".into(),
                ));
            }
        }
        let mut note = None;
        let mut exhausted = false;
        match params.action {
            TodoAction::Answer => {
                let worker_id = run
                    .info
                    .worker_id
                    .clone()
                    .ok_or_else(|| WorkerError::Invalid("the run has no worker".into()))?;
                let answer = WorkerAnswerParams {
                    worker_id,
                    request_id: params.request_id.clone(),
                    decision: params.decision,
                    answers: params.answers.clone(),
                    message: params.message.clone(),
                    command_id: Some(format!("{}:{}:answer", run.info.run_id, params.event)),
                };
                match self.answer(&answer) {
                    Ok(_) => {}
                    // Answered elsewhere (the user's `?` list) or withdrawn:
                    // nothing left to answer, so the run goes on.
                    Err(error @ (WorkerError::QuestionGone(_) | WorkerError::NoQuestion(_))) => {
                        note = Some(error.to_string());
                    }
                    Err(error) => return Err(error),
                }
            }
            TodoAction::Retry => {
                let task = params.task.as_deref().unwrap_or_default();
                if task.trim().is_empty() {
                    return Err(WorkerError::Invalid(
                        "retry needs the next attempt's task text".into(),
                    ));
                }
                exhausted = run.info.attempt >= MAX_ATTEMPTS;
            }
            // The kill follows the recorded resume: the driver waiting for
            // the worker's exit goes on as soon as it dies, and its write
            // would make this event stale before the resume is recorded.
            TodoAction::ForceStop => {
                if run.info.worker_id.is_none() {
                    return Err(WorkerError::Invalid("the run has no worker".into()));
                }
            }
            TodoAction::Approve
            | TodoAction::Verify
            | TodoAction::RetryInstall
            | TodoAction::SkipInstall
            | TodoAction::RetryTodo
            | TodoAction::SkipTodo
            | TodoAction::RetryPush
            | TodoAction::Abort
            | TodoAction::Unknown => {}
        }
        // Before the run moves on: a driver still waiting on the worker's
        // stop may reach the verify as soon as the resume is recorded.
        match &params.env {
            Some(env) => lock(&RUN_ENV).insert(params.run_id.clone(), env.clone()),
            None => lock(&RUN_ENV).remove(&params.run_id),
        };
        let event = json!({
            "type": "run_resumed",
            "event": params.event,
            "action": params.action,
            "task": params.task,
            "request_id": params.request_id,
            "env_sent": params.env.is_some(),
            "note": note,
            "todo_note": params.note,
            "close": params.close,
            "message": (params.action == TodoAction::Abort).then_some(&params.message),
        });
        let outcome = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&params.run_id)? else {
                    return Ok(Err(WorkerError::RunNotFound(format!(
                        "run {} not found",
                        params.run_id
                    ))));
                };
                if let Some(error) = stale(&current) {
                    return Ok(Err(error));
                }
                let was_blocked = current.info.status == TodoRunStatus::Blocked;
                current.info.status = TodoRunStatus::Running;
                match params.action {
                    TodoAction::Answer => current.info.step = TodoStep::Attention,
                    TodoAction::Approve => {
                        current.info.step = TodoStep::Stop;
                        current.finish.note = params.note.clone();
                        current.finish.close = params.close.clone();
                    }
                    TodoAction::RetryInstall => current.info.step = TodoStep::Install,
                    TodoAction::SkipInstall | TodoAction::RetryTodo => {
                        current.info.step = TodoStep::Todo
                    }
                    TodoAction::SkipTodo | TodoAction::RetryPush => {
                        current.info.step = TodoStep::Push
                    }
                    TodoAction::Verify => current.info.step = TodoStep::Verify,
                    // A retry after the last attempt only stops the worker
                    // (a retry asked at a review: it still runs in the
                    // slot); the restart then blocks the run.
                    TodoAction::Retry => {
                        current.info.step = TodoStep::Restart;
                        if !exhausted {
                            current.info.task = params.task.clone().unwrap_or_default();
                        }
                    }
                    // The driver's abort step stops the worker, then ends
                    // the run. An abort at that step (of its `still_alive`
                    // event) keeps the first reason.
                    TodoAction::Abort if current.info.step != TodoStep::Abort => {
                        let at = if was_blocked {
                            format!("blocked at its {} step", step_name(current.info.step))
                        } else {
                            format!("at its {} step", step_name(current.info.step))
                        };
                        let reason = params
                            .message
                            .as_deref()
                            .map(str::trim)
                            .filter(|reason| !reason.is_empty())
                            .map(|reason| format!(": {reason}"))
                            .unwrap_or_default();
                        current.info.error =
                            Some(format!("the coordinator aborted the run {at}{reason}"));
                        current.info.step = TodoStep::Abort;
                    }
                    // The stop's wait goes on, at the run's step.
                    TodoAction::ForceStop | TodoAction::Abort | TodoAction::Unknown => {}
                }
                tx.run_event(&mut current, &event, false, now_ms())?;
                Ok(Ok(current))
            })
            .map_err(store_error)?;
        let run = outcome?;
        announce();
        if params.action == TodoAction::ForceStop {
            if let Some(worker_id) = run.info.worker_id.clone() {
                let kill = WorkerKillParams {
                    worker_id,
                    force: false,
                    caller_pane_id: run.owner_pane.clone(),
                    command_id: Some(format!("{}:{}:force-stop", run.info.run_id, params.event)),
                };
                match self.kill_command(&kill) {
                    Ok(_) => {}
                    // It exited meanwhile: nothing left to stop.
                    Err(error @ (WorkerError::NeedsForce(_) | WorkerError::NotRunning(_))) => {
                        self.run_note(
                            &run.info.run_id,
                            json!({"type": "run_force_stop", "note": error.to_string()}),
                        );
                    }
                    // The run still waits for the exit; asked again,
                    // `todo.status` raises a new `still_alive` event.
                    Err(error) => return Err(error),
                }
            }
        }
        self.spawn_driver(&run.info.run_id);
        Ok(run.info)
    }

    /// Drives every run in progress again, as a server that starts does:
    /// a running one from its step; a waiting one only acknowledges its
    /// worker's events again (an ack lost to a crash).
    pub(crate) fn resume_runs(&self) {
        let runs = match self.run_store().map(|store| store.runs(None)) {
            Ok(Ok(runs)) => runs,
            Ok(Err(error)) => {
                warn!(%error, "cannot read the todo runs");
                return;
            }
            Err(_) => return,
        };
        for run in runs {
            match run.info.status {
                TodoRunStatus::Running => self.spawn_driver(&run.info.run_id),
                TodoRunStatus::Waiting => {
                    if let (Some(worker_id), Some(seq)) =
                        (&run.info.worker_id, run.info.last_acked_seq)
                    {
                        self.ack_quietly(worker_id, seq);
                    }
                }
                _ => {}
            }
        }
    }

    /// Lets go of every run this server drives, after a live handoff
    /// succeeded: each driver returns at its next step or liveness check,
    /// without blocking its run, and releases the run's lock for the new
    /// server's driver. No new driver starts here.
    pub(crate) fn let_go_of_runs(&self) {
        {
            // Under the registry lock, which a driver's wait re-checks the
            // flag under before it blocks: no wakeup is lost between them.
            let _registry = lock(&self.shared.registry);
            self.shared.handed_off.store(true, Ordering::SeqCst);
            // Wake the drivers blocked on a worker's events.
            self.shared.changed.notify_all();
        }
        announce();
    }

    fn handed_off(&self) -> bool {
        self.shared.handed_off.load(Ordering::SeqCst)
    }

    /// The run's lock file, next to the store.
    pub(super) fn run_lock_path(&self, run_id: &str) -> PathBuf {
        self.shared.dir.join(format!("run-{run_id}.lock"))
    }

    /// Takes the run's lock, which the driving server holds while it
    /// drives the run. Held by another server (the old one of a live
    /// handoff, still at a step), it notes that and blocks until that
    /// server lets go or ends: the operating system releases the lock
    /// then, so no timer decides when.
    fn take_run_lock(&self, run_id: &str) -> Result<File, String> {
        let path = self.run_lock_path(run_id);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("cannot open the run lock {}: {error}", path.display()))?;
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(format!("cannot lock {}: {error}", path.display()))
            }
        }
        self.run_note(
            run_id,
            json!({"type": "run_lock_wait", "pid": std::process::id()}),
        );
        file.lock()
            .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
        self.run_note(
            run_id,
            json!({"type": "run_lock_taken", "pid": std::process::id()}),
        );
        Ok(file)
    }

    /// Appends an event to the run without writing its row.
    fn run_note(&self, run_id: &str, event: Value) {
        let noted = self
            .run_store()
            .map_err(|error| error.to_string())
            .and_then(|store| {
                store
                    .transaction(|tx| tx.run_note(run_id, &event, now_ms()))
                    .map_err(|error| error.to_string())
            });
        match noted {
            Ok(_) => announce(),
            Err(error) => warn!(run_id, error, "cannot record a todo run's note"),
        }
    }

    fn spawn_driver(&self, run_id: &str) {
        let supervisor = self.clone();
        let id = run_id.to_owned();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-todo-run", move || {
            supervisor.drive(&id);
        }) {
            warn!(%error, run_id, "cannot start the todo run's driver");
        }
    }

    /// Runs the run's steps until it waits, ends or is blocked. Each step
    /// reads the run from the store, so a driver that starts after a crash
    /// goes on from where the last one recorded.
    fn drive(&self, run_id: &str) {
        if self.handed_off() {
            return;
        }
        let Some(mut driving) = Driving::claim(run_id) else {
            return;
        };
        let _held = match self.take_run_lock(run_id) {
            Ok(held) => held,
            Err(error) => {
                warn!(run_id, error, "cannot take the todo run's lock");
                return;
            }
        };
        loop {
            if self.handed_off() {
                self.run_note(
                    run_id,
                    json!({"type": "run_let_go", "pid": std::process::id()}),
                );
                return;
            }
            let mut run = match self.load_run(run_id) {
                Ok(run) => run,
                Err(error) => {
                    warn!(%error, run_id, "cannot read the todo run");
                    return;
                }
            };
            if run.info.status != TodoRunStatus::Running {
                if matches!(
                    run.info.status,
                    TodoRunStatus::Done | TodoRunStatus::Blocked | TodoRunStatus::Aborted
                ) {
                    lock(&RUN_ENV).remove(run_id);
                    // Ended: no driver needs its lock again.
                    let _ = std::fs::remove_file(self.run_lock_path(run_id));
                }
                if driving.release() {
                    return;
                }
                continue;
            }
            #[cfg(test)]
            if crashes_before(&run.info.repo, run.info.step) {
                drop(driving);
                lock(&CRASHED).push(run.info.repo.clone());
                announce();
                return;
            }
            let stepped = match run.info.step {
                TodoStep::Preflight | TodoStep::Start => self.step_start(&mut run),
                TodoStep::Attention => self.step_attention(&mut run),
                TodoStep::Stop => self.step_stop(&mut run),
                TodoStep::Restart => self.step_restart(&mut run),
                TodoStep::Verify => self.step_verify(&mut run),
                TodoStep::CherryPick => self.step_cherry_pick(&mut run),
                TodoStep::Install => self.step_install(&mut run),
                TodoStep::Todo => self.step_todo(&mut run),
                TodoStep::Push => self.step_push(&mut run),
                TodoStep::Cleanup => self.step_cleanup(&mut run),
                TodoStep::Abort => self.step_abort(&mut run),
                step @ (TodoStep::Review | TodoStep::Done | TodoStep::Unknown) => {
                    Err(format!("a running run cannot be at step {step:?}"))
                }
            };
            if let Err(why) = stepped {
                #[cfg(test)]
                if why == CRASHED_HERE {
                    drop(driving);
                    lock(&CRASHED).push(run.info.repo.clone());
                    announce();
                    return;
                }
                // A step a handoff cut off is the new server's to run.
                if !self.handed_off() {
                    self.block_run(&mut run, why);
                }
            }
        }
    }

    /// Appends a run event and writes the run as it is after it.
    fn run_step(&self, run: &mut Run, event: Value) -> Result<i64, String> {
        self.run_write(run, event, false)
    }

    fn run_write(&self, run: &mut Run, event: Value, pending: bool) -> Result<i64, String> {
        let store = self.run_store().map_err(|error| error.to_string())?;
        let seq = store
            .transaction(|tx| tx.run_event(run, &event, pending, now_ms()))
            .map_err(|error| format!("the worker store failed: {error}"))?;
        announce();
        Ok(seq)
    }

    /// Records a coordinator-facing event; one of a waiting run is the
    /// event it waits on.
    fn record_run_event(&self, run: &mut Run, event: &TodoRunEvent) -> Result<i64, String> {
        let pending = run.info.status == TodoRunStatus::Waiting;
        self.run_write(
            run,
            json!({
                "type": "run_event",
                "kind": event.kind,
                "step": run.info.step,
                "status": run.info.status,
                "attempt": run.info.attempt,
                "event": event,
            }),
            pending,
        )
    }

    /// Ends the run as blocked with why; nothing more happens to it.
    fn block_run(&self, run: &mut Run, why: String) {
        warn!(run_id = %run.info.run_id, why, "todo run blocked");
        run.info.status = TodoRunStatus::Blocked;
        run.info.error = Some(why.clone());
        let mut event = new_event(TodoEventKind::Blocked);
        event.error = Some(why);
        if let Err(error) = self.record_run_event(run, &event) {
            warn!(run_id = %run.info.run_id, error, "cannot record the blocked todo run");
        }
        lock(&RUN_ENV).remove(&run.info.run_id);
    }

    fn ack_quietly(&self, worker_id: &str, seq: i64) {
        if let Err(error) = self.ack(worker_id, seq) {
            warn!(%error, worker_id, seq, "cannot acknowledge the todo run's worker");
        }
    }

    /// The branch's commits since the base and their diff stat, in the
    /// worker's directory.
    fn evidence(run: &Run, worker: &WorkerInfo) -> (Option<String>, Vec<String>) {
        let Some(base) = run.info.base.as_deref() else {
            return (None, Vec::new());
        };
        let dir = Path::new(&worker.cwd);
        let diff_stat = git(dir, &["diff", "--stat", base, "HEAD"])
            .ok()
            .map(|stat| stat.trim_end().to_owned())
            .filter(|stat| !stat.is_empty());
        let commits = git(dir, &["rev-list", "--reverse", &format!("{base}..HEAD")])
            .map(|out| out.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        (diff_stat, commits)
    }

    /// Starts the attempt's worker in the folder slot, on the attempt's
    /// branch from the base, with a command id derived from the run: a
    /// driver after a crash gets the same worker back instead of a second.
    fn step_start(&self, run: &mut Run) -> Result<(), String> {
        self.release_slot(run)?;
        let command_id = format!("{}:{}:start", run.info.run_id, run.info.attempt);
        let branch = run
            .info
            .branch
            .clone()
            .unwrap_or_else(|| branch_of(&run.info.item, &run.info.run_id, run.info.attempt));
        run.info.step = TodoStep::Start;
        run.info.branch = Some(branch.clone());
        self.run_step(
            run,
            json!({
                "type": "run_start_intent",
                "attempt": run.info.attempt,
                "branch": branch,
                "command_id": command_id,
            }),
        )?;
        let params = WorkerStartParams {
            cwd: run.info.repo.clone(),
            prompt: worker_task(&run.info),
            model: None,
            name: None,
            workspace_id: None,
            folder_slot: Some(SLOT.to_owned()),
            branch: Some(branch),
            base: run.info.base.clone(),
            fresh_build: false,
            owner_pane_id: run.owner_pane.clone(),
            owner_session_id: run.owner_session.clone(),
            item: Some(run.info.item.clone()),
            command_id: Some(command_id.clone()),
        };
        let worker_id = match self.start(&params) {
            Ok(worker) => worker.worker_id,
            // A start a crash cut off: the receipt names its worker when
            // it got that far.
            Err(error @ WorkerError::CommandInterrupted(_)) => self
                .run_store()
                .ok()
                .and_then(|store| store.receipt(&command_id).ok().flatten())
                .and_then(|receipt| receipt.worker_id)
                .ok_or_else(|| format!("starting the worker: {error}"))?,
            Err(error) => return Err(format!("starting the worker: {error}")),
        };
        run.info.worker_id = Some(worker_id.clone());
        run.info.step = TodoStep::Attention;
        self.run_step(
            run,
            json!({"type": "run_started", "worker_id": worker_id, "step": run.info.step}),
        )?;
        Ok(())
    }

    /// Waits for the worker's next question, turn end or end after the
    /// run's acknowledged seq. A question the worker policy did not decide
    /// becomes a `question` event; a turn's end or the worker's end a
    /// `review` event. Either way the run then waits for the coordinator,
    /// and the worker's events up to there are acknowledged: the run owns
    /// them now.
    fn step_attention(&self, run: &mut Run) -> Result<(), String> {
        let worker_id = run
            .info
            .worker_id
            .clone()
            .ok_or("the run has no worker at its attention step")?;
        // A question asked before the cursor still pending: another one was
        // answered while it waited, and the wait would not show it again.
        let now = self.status(&worker_id).map_err(|error| error.to_string())?;
        let still_pending = pending_questions(&now.questions);
        let (reason, questions, seq, worker) = if !still_pending.is_empty() {
            let seq = now.seq.unwrap_or_default();
            (WorkerAttentionReason::Question, still_pending, seq, now)
        } else {
            let attention = self
                .wait_attention(&worker_id, run.info.last_acked_seq, NO_DEADLINE, || {
                    !self.handed_off()
                })
                .map_err(|error| error.to_string())?
                .ok_or("the wait for the worker ended")?;
            (
                attention.reason,
                pending_questions(&attention.questions),
                attention.seq,
                attention.worker,
            )
        };
        run.info.last_acked_seq = Some(seq);
        let event = match reason {
            WorkerAttentionReason::Question if questions.is_empty() => {
                // Its answer is being written: nothing to ask.
                self.run_step(run, json!({"type": "run_acked", "seq": seq}))?;
                self.ack_quietly(&worker_id, seq);
                return Ok(());
            }
            WorkerAttentionReason::Question => {
                let mut event = new_event(TodoEventKind::Question);
                event.questions = questions;
                event
            }
            _ => {
                run.info.step = TodoStep::Review;
                let (diff_stat, commits) = Self::evidence(run, &worker);
                let mut event = new_event(TodoEventKind::Review);
                event.diff_stat = diff_stat;
                event.commits = commits;
                event.result_text = worker.last_result.and_then(|result| result.text);
                event
            }
        };
        run.info.status = TodoRunStatus::Waiting;
        self.record_run_event(run, &event)?;
        self.ack_quietly(&worker_id, seq);
        Ok(())
    }

    /// Stops the attempt's worker, when it still runs, and waits for its
    /// exit, which its own exit event reports. The stop carries a command
    /// id derived from the run and the run's owner pane, so the exit it
    /// causes is acknowledged. No clock decides that the worker is stuck:
    /// the wait ends only with its exit. A coordinator who asks for the
    /// run's state meanwhile gets a `still_alive` event
    /// ([`Self::todo_status`]); its `force-stop` SIGKILLs the worker, or
    /// the worker exits by itself.
    fn stop_worker(&self, run: &mut Run) -> Result<(), String> {
        let Some(worker_id) = run.info.worker_id.clone() else {
            return Ok(());
        };
        let worker = self.status(&worker_id).map_err(|error| error.to_string())?;
        if !is_gone(&worker) {
            self.run_step(
                run,
                json!({"type": "run_stop_intent", "worker_id": worker_id}),
            )?;
            let target = WorkerCommandTarget {
                worker_id: worker_id.clone(),
                caller_pane_id: run.owner_pane.clone(),
                command_id: Some(format!("{}:{}:stop", run.info.run_id, run.info.attempt)),
            };
            let stopped = match self.stop_command(&target) {
                // A stop a crash cut off: send it again without the id.
                Err(WorkerError::CommandInterrupted(_)) => {
                    self.stop_command(&WorkerCommandTarget {
                        command_id: None,
                        ..target
                    })
                }
                other => other,
            };
            match stopped {
                Ok(_) | Err(WorkerError::NotRunning(_)) => {}
                Err(error) => return Err(format!("stopping worker {worker_id}: {error}")),
            }
            self.wait(&worker_id, WorkerWaitUntil::Exit, NO_DEADLINE, || {
                !self.handed_off()
            })
            .map_err(|error| error.to_string())?
            .ok_or("this server handed the run off")?;
            // `todo.status` may have raised a `still_alive` event meanwhile,
            // and a force-stop answered it; the run goes on either way, and
            // that event is stale.
            *run = self
                .load_run(&run.info.run_id)
                .map_err(|error| error.to_string())?;
            run.info.status = TodoRunStatus::Running;
        }
        let worker = self.status(&worker_id).map_err(|error| error.to_string())?;
        if let Some(seq) = worker.seq {
            run.info.last_acked_seq = Some(seq);
            self.ack_quietly(&worker_id, seq);
        }
        Ok(())
    }

    fn step_stop(&self, run: &mut Run) -> Result<(), String> {
        self.stop_worker(run)?;
        // Aborted at a `still_alive` event: the abort step goes on.
        if run.info.step == TodoStep::Abort {
            return Ok(());
        }
        run.info.step = TodoStep::Verify;
        self.run_step(
            run,
            json!({"type": "run_stopped", "worker_id": run.info.worker_id, "step": run.info.step}),
        )?;
        Ok(())
    }

    /// Ends the attempt's worker and sets up the next attempt: a new
    /// branch from the same base, the task text the retry gave.
    fn step_restart(&self, run: &mut Run) -> Result<(), String> {
        self.stop_worker(run)?;
        if run.info.step == TodoStep::Abort {
            return Ok(());
        }
        if run.info.attempt >= MAX_ATTEMPTS {
            return Err(format!(
                "the run used its {MAX_ATTEMPTS} attempts; a retry was asked after the last"
            ));
        }
        let previous = run.info.worker_id.take();
        run.info.attempt += 1;
        run.info.branch = Some(branch_of(
            &run.info.item,
            &run.info.run_id,
            run.info.attempt,
        ));
        run.info.last_acked_seq = None;
        run.info.step = TodoStep::Start;
        self.run_step(
            run,
            json!({
                "type": "run_restarted",
                "previous_worker_id": previous,
                "attempt": run.info.attempt,
                "branch": run.info.branch,
                "step": run.info.step,
            }),
        )?;
        Ok(())
    }

    /// Ends an aborted run: stops its worker when it still runs and waits
    /// for its exit event ([`Self::stop_worker`]), then records the run
    /// `aborted` with the coordinator's reason and what it leaves as it is:
    /// its attempts' branches with their commits since the base, and a
    /// commit already picked onto `master`. Nothing is deleted or reverted.
    fn step_abort(&self, run: &mut Run) -> Result<(), String> {
        self.stop_worker(run)?;
        let repo = PathBuf::from(&run.info.repo);
        let mut branches: Vec<String> = (1..=run.info.attempt)
            .map(|attempt| branch_of(&run.info.item, &run.info.run_id, attempt))
            .chain(run.info.branch.clone())
            .collect();
        branches.dedup();
        let mut left = Vec::new();
        let mut commits = Vec::new();
        for branch in branches {
            let Ok(head) = git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}^{{commit}}"),
                ],
            ) else {
                continue;
            };
            let since_base = run.info.base.as_deref().and_then(|base| {
                git(
                    &repo,
                    &["rev-list", "--reverse", &format!("{base}..{branch}")],
                )
                .ok()
            });
            let own: Vec<String> = since_base
                .as_deref()
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            left.push(format!(
                "{branch} at {} ({} commit{} since the base)",
                &head.trim()[..head.trim().len().min(12)],
                own.len(),
                if own.len() == 1 { "" } else { "s" }
            ));
            for commit in own {
                if !commits.contains(&commit) {
                    commits.push(commit);
                }
            }
        }
        let mut why = run
            .info
            .error
            .clone()
            .unwrap_or_else(|| "the coordinator aborted the run".to_owned());
        if left.is_empty() {
            why.push_str("; the run has no branch left");
        } else {
            why.push_str(&format!("; branches left as they are: {}", left.join(", ")));
        }
        if let Some(picked) = &run.info.picked {
            why.push_str(&format!("; commit {picked} stays on master"));
        }
        run.info.status = TodoRunStatus::Aborted;
        run.info.error = Some(why.clone());
        let mut event = new_event(TodoEventKind::Aborted);
        event.error = Some(why);
        event.commits = commits;
        self.record_run_event(run, &event)?;
        Ok(())
    }

    /// Verifies the attempt's commit with the registered check, run as its
    /// argv. Verified goes on to the cherry-pick; anything else is a
    /// `verify_failed` event asking for the next attempt's task.
    fn step_verify(&self, run: &mut Run) -> Result<(), String> {
        let worker_id = run
            .info
            .worker_id
            .clone()
            .ok_or("the run has no worker to verify")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        self.run_step(
            run,
            json!({"type": "run_verify_intent", "worker_id": worker_id, "checks": run.info.checks}),
        )?;
        let env = lock(&RUN_ENV).get(&run.info.run_id).cloned();
        let no_env = "this server has not got the caller's environment for the check (a \
                      restart, or a `herdr todo resume` that sent none); `herdr todo resume \
                      --action verify` from the coordinator's shell sends it and verifies again";
        let params = WorkerVerifyParams {
            worker_id: worker_id.clone(),
            base,
            expected_message: run.info.message.clone(),
            allowed_paths: run.info.paths.clone(),
            command: None,
            generated: Vec::new(),
            env,
        };
        let checks = run.checks.clone();
        let commands = checks
            .iter()
            .map(|check| {
                let command = if params.env.is_some() {
                    CheckCommand::Argv(&check.argv)
                } else {
                    CheckCommand::Unavailable(no_env)
                };
                (Some(check.name.as_str()), command)
            })
            .collect();
        let verification = self
            .verify_with(&params, commands)
            .map_err(|error| format!("verifying worker {worker_id}: {error}"))?;
        if verification.verdict == WorkerVerdict::Verified {
            run.info.step = TodoStep::CherryPick;
            self.run_step(
                run,
                json!({
                    "type": "run_verified",
                    "head": verification.head,
                    "commits": verification.commits,
                    "step": run.info.step,
                }),
            )?;
            return Ok(());
        }
        let mut event = new_event(TodoEventKind::VerifyFailed);
        if let Ok(worker) = self.status(&worker_id) {
            event.diff_stat = Self::evidence(run, &worker).0;
        }
        event.commits = verification.commits.clone();
        event.verification = Some(verification);
        run.info.status = TodoRunStatus::Waiting;
        self.record_run_event(run, &event)?;
        Ok(())
    }

    /// Picks the attempt's verified commit onto `master` in the
    /// repository's shared checkout. Refused (blocked) when that checkout
    /// is not on `master`, has uncommitted changes or a cherry-pick in
    /// progress, or its `master` no longer contains the base; a conflict is
    /// aborted and blocks too. A commit `master` already has (a pick a crash
    /// did not record) is not picked again.
    fn step_cherry_pick(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
        let branch = run.info.branch.clone().ok_or("the run has no branch")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        let commit = git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )?
        .trim()
        .to_owned();
        let on = git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|branch| branch.trim().to_owned())
            .unwrap_or_default();
        if on != "master" {
            return Err(format!(
                "the shared checkout {} is on {}, not master; commit {commit} was not picked",
                repo.display(),
                if on.is_empty() {
                    "a detached HEAD"
                } else {
                    &on
                }
            ));
        }
        if git(
            &repo,
            &["rev-parse", "--quiet", "--verify", "CHERRY_PICK_HEAD"],
        )
        .is_ok()
        {
            return Err(format!(
                "the shared checkout {} has a cherry-pick in progress",
                repo.display()
            ));
        }
        let dirty = git(&repo, &["status", "--porcelain", "--untracked-files=no"])?;
        if !dirty.trim().is_empty() {
            return Err(format!(
                "the shared checkout {} has uncommitted changes; commit {commit} was not picked:\n{}",
                repo.display(),
                dirty.trim_end()
            ));
        }
        if git(&repo, &["merge-base", "--is-ancestor", &base, "HEAD"]).is_err() {
            return Err(format!(
                "master in {} no longer contains the run's base {base} (rewritten?); commit \
                 {commit} was not picked",
                repo.display()
            ));
        }
        // Master already has the commit itself (a pick made in the same
        // second reproduces its sha), or its change: `git cherry` marks
        // that `-`.
        let parent = format!("{commit}^");
        let applied = git(&repo, &["merge-base", "--is-ancestor", &commit, "HEAD"]).is_ok()
            || git(&repo, &["cherry", "HEAD", &commit, &parent])?
                .lines()
                .any(|line| line.starts_with("- "));
        if !applied {
            let head = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
            self.run_step(
                run,
                json!({"type": "run_cherry_pick_intent", "commit": commit, "master": head}),
            )?;
            if let Err(error) = git(&repo, &["cherry-pick", &commit]) {
                let aborted = git(&repo, &["cherry-pick", "--abort"]);
                return Err(format!(
                    "cherry-picking {commit} onto master failed{}: {error}",
                    if aborted.is_ok() {
                        " and was aborted"
                    } else {
                        ""
                    }
                ));
            }
        }
        // The commit on master carrying the change: the newest one since
        // the base with the run's subject.
        let picked = git(&repo, &["log", "--format=%H %s", &format!("{base}..HEAD")])?
            .lines()
            .find_map(|line| {
                let (sha, subject) = line.split_once(' ')?;
                (subject == run.info.message).then(|| sha.to_owned())
            })
            .ok_or_else(|| format!("master has no commit {:?} after the pick", run.info.message))?;
        run.info.picked = Some(picked.clone());
        run.info.step = TodoStep::Install;
        self.run_step(
            run,
            json!({"type": "run_picked", "commit": picked, "step": run.info.step}),
        )?;
        Ok(())
    }
}

/// Drives the runs a previous server left in progress, from a thread so
/// the server's start does not wait for the store.
pub(crate) fn resume_runs_at_start() {
    let spawned = crate::thread_spawn::spawn_named("herdr-todo-resume", || {
        super::supervisor().resume_runs();
    });
    if let Err(error) = spawned {
        warn!(%error, "cannot resume the todo runs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_are_r_and_eight_base32_characters() {
        let id = new_run_id("/repo", "t-abcd2345");
        assert_eq!(id.len(), 10, "{id}");
        assert!(id.starts_with("r-"), "{id}");
        assert!(id[2..]
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)));
        assert_ne!(id, new_run_id("/repo", "t-abcd2345"));
    }

    #[test]
    fn a_message_is_a_lowercase_conventional_subject() {
        for good in [
            "feat: herdr todo run drives an item",
            "fix(workers): a stale event is refused",
            "refactor!: drop the old path",
            "docs: `TODO.md` says so",
        ] {
            assert_eq!(check_message(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "feat: Capital start",
            "Feat: upper type",
            "feature: unknown type",
            "feat:no space",
            "feat: two\nlines",
            " feat: leading space",
            "feat(Scope): upper scope",
            "feat(: open scope",
            "feat: ",
        ] {
            assert!(check_message(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn paths_are_relative_git_globs() {
        assert_eq!(
            check_paths(&[
                "src/**".into(),
                "AGENTS.md".into(),
                ".herdr/checks.toml".into()
            ]),
            Ok(())
        );
        for bad in [
            "",
            " ",
            "/etc/passwd",
            "../up",
            "src/../x",
            ":(top)x",
            "a\\b",
        ] {
            assert!(check_paths(&[bad.into()]).is_err(), "{bad:?}");
        }
        assert!(check_paths(&[]).is_err());
    }

    #[test]
    fn the_checks_file_maps_names_to_argv() {
        let checks: ChecksFile = toml::from_str(
            "[checks]\nworkers = [\"cargo\", \"nextest\", \"run\", \"-E\", \"test(/workers::/)\"]\n\
             [preflight]\nmin_free_gib = 2.5\n",
        )
        .unwrap();
        assert_eq!(
            checks.checks["workers"],
            ["cargo", "nextest", "run", "-E", "test(/workers::/)"]
        );
        assert_eq!(checks.preflight.min_free_gib, Some(2.5));
        // A shell string is not an argv.
        assert!(toml::from_str::<ChecksFile>("[checks]\nworkers = \"cargo test\"\n").is_err());
        assert!(toml::from_str::<ChecksFile>("[other]\nx = 1\n").is_err());
    }

    #[test]
    fn the_repositorys_checks_file_parses() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let checks = read_checks(repo).unwrap();
        assert!(checks.checks.contains_key("workers"), "{checks:?}");
        assert!(checks.checks.values().all(|argv| !argv.is_empty()));
        let install = checks.install.unwrap();
        assert!(!install.command.is_empty() && !install.build_id.is_empty());
    }
}
