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
//! A run lives in the worker store: its row in `runs`, its current
//! attempt's row in `attempts` and its `run_*` events, written in one
//! transaction; a commit it lands is recorded in `landings` and carries the
//! `Herdr-Item` and `Herdr-Run` trailers. Every side effect is recorded as an
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
    TodoAction, TodoDraftRunParams, TodoEventKind, TodoLanding, TodoLandingSource, TodoNextRun,
    TodoResumeParams, TodoRunEvent, TodoRunInfo, TodoRunParams, TodoRunStatus, TodoStep,
    TodoWaitParams, WorkerAnswerParams, WorkerAttentionReason, WorkerCommandTarget, WorkerInfo,
    WorkerKillParams, WorkerQuestion, WorkerQuestionState, WorkerStartParams, WorkerState,
    WorkerVerdict, WorkerVerification, WorkerVerifyParams, WorkerWaitUntil,
};

mod auto_answer;
mod auto_draft;
mod auto_review;
pub(super) mod decision;
pub(super) mod escalations;
mod finish;
mod prepare;
pub(super) mod queue;
mod usage_gate;

#[cfg(all(test, unix))]
pub(crate) use auto_answer::settle_inline;

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
    /// The owner pane's workspace, which lists the run's workers.
    pub(super) workspace: Option<String>,
    /// The coordination tenure that owns the run: the owner pane's when
    /// the run was claimed, moved with it on a resume or a handoff.
    pub(super) owner_coordinator: Option<String>,
    pub(super) finish: RunFinish,
    /// The current attempt's row of `attempts`, beside what [`TodoRunInfo`]
    /// shows of it (its number, worker, branch and task).
    pub(super) current: Attempt,
}

/// What a run keeps of an attempt beside its number, worker, branch and
/// task: written with the run's row, one `attempts` row per attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Attempt {
    /// The commit the attempt is verified and reviewed against: the run's
    /// base.
    pub(super) base: Option<String>,
    /// The previous attempt, when this one followed a retry.
    pub(super) from_attempt: Option<u32>,
    /// The previous attempt's commit the attempt's branch starts from,
    /// cherry-picked by the driver; none when it starts from the base.
    pub(super) from_commit: Option<String>,
    /// The attempt's commit as the coordinator's decision saw it: the one
    /// an approval names, or the one a retry carries on.
    pub(super) commit: Option<String>,
    /// The event the coordinator decided on, the decision (`approve`,
    /// `retry`, `abort`) and, for a retry, its text: the review.
    pub(super) review_event: Option<i64>,
    pub(super) review_decision: Option<String>,
    pub(super) review_text: Option<String>,
    /// The attempt's latest verdict ([`WorkerVerification`] as JSON).
    pub(super) verification: Option<String>,
}

/// An attempt's decision as `attempts.review_decision` holds it.
pub(super) const APPROVE: &str = "approve";
const RETRY: &str = "retry";
const ABORT: &str = "abort";

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
    /// The `tests` check a run stored by an older build added for an API
    /// change; read only, [`RunFinish::path_checks`] holds it now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) contract_check: Option<RunCheck>,
    /// The registered [`PATH_CHECKS`] as preflight read them, those the run
    /// does not name: the verify adds each when the attempt's diff touches
    /// its paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) path_checks: Vec<RunCheck>,
    /// The item a close named to start once the run is done, with its
    /// run's parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next: Option<TodoNextRun>,
    /// Why a close named no next item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) stop_reason: Option<String>,
    /// When the driver recorded its intent to start `next`: a run of that
    /// item created since then is the start a crash cut off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next_intent_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) next_refusal: Option<String>,
    /// The stop reason was handed to the server's notifications.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) stop_notified: bool,
    /// The server reviews the run's `review` events itself
    /// ([`auto_review`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) auto_review: bool,
    /// The server answers the questions of the run's worker itself
    /// ([`auto_answer`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) auto_answer: bool,
    /// The server drafts the run's task, subject, paths and checks itself
    /// ([`auto_draft`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) draft: bool,
    /// The repository's queue started the run ([`queue`]): its own
    /// review's approval closes the item.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) queued: bool,
    /// The repository's granted `prepare` as preflight read it from the
    /// base commit, with its grant: the grants the run runs with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) prepare: Option<super::capabilities::PreparePlan>,
    /// The prepare ran and succeeded ([`prepare`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) prepared: bool,
}

/// A check the verify adds by itself, when the repository registers it, to
/// a run that does not name it and whose diff touches one of its paths.
struct PathCheck {
    name: &'static str,
    /// Path prefixes, relative to the repository.
    paths: &'static [&'static str],
    /// What the check verifies there, for the worker's contract.
    why: &'static str,
}

/// The paths the Rust build reads: the verify's `lint`, `windows-lint` and
/// `tests` cover each of them.
#[cfg(test)]
const RUST_PATHS: [&str; 9] = [
    "src/",
    "tests/",
    "crates/",
    "build.rs",
    "Cargo.toml",
    "Cargo.lock",
    "clippy.toml",
    "rust-toolchain.toml",
    ".cargo/",
];

/// The checks the verify adds by path, one for each recipe the install's
/// `just check` runs, so a commit the verify passes does not fail there:
/// `lint` (`cargo fmt --check` and clippy), `windows-lint`, `tests`, the
/// full suite, which holds the frozen client endpoint contract tests
/// (AGENTS.md, "Stable client endpoint contract"), `maintenance`, the
/// maintenance script tests, which hold the config reference docs check,
/// and the UI hot-path architecture, integration asset and docs contract
/// tests. Each check's paths are the files its recipe reads from the
/// repository. `just_check_runs_only_recipes_the_verify_adds_by_path`
/// keeps this list and the justfile's `check` in step.
const PATH_CHECKS: [PathCheck; 7] = [
    PathCheck {
        name: "lint",
        paths: &[
            "src/",
            "tests/",
            "crates/",
            "build.rs",
            "Cargo.toml",
            "Cargo.lock",
            "clippy.toml",
            "rust-toolchain.toml",
            ".cargo/",
        ],
        why: "so a Rust change is verified with `just check`'s formatting check and clippy",
    },
    PathCheck {
        name: "windows-lint",
        paths: &[
            "src/",
            "tests/",
            "crates/",
            "build.rs",
            "Cargo.toml",
            "Cargo.lock",
            "clippy.toml",
            "rust-toolchain.toml",
            ".cargo/",
            "vendor/",
            "scripts/windows_cross.py",
        ],
        why: "so a Rust change is verified with clippy for Windows, like `just check`",
    },
    PathCheck {
        name: "tests",
        paths: &[
            "src/",
            "tests/",
            "crates/",
            "build.rs",
            "Cargo.toml",
            "Cargo.lock",
            "clippy.toml",
            "rust-toolchain.toml",
            ".cargo/",
            "assets/",
            "vendor/",
            ".herdr/",
            "distribution/",
            "skills/",
            "docs/next/api/",
            "scripts/todo_edit.py",
            "scripts/target_sweep.py",
            "scripts/herdr_live.sh",
        ],
        why: "so a change there is verified with the full test suite, \
              the frozen client contract tests among them",
    },
    PathCheck {
        name: "maintenance",
        paths: &[
            "src/config/",
            "src/integration/assets/",
            "docs/next/",
            "scripts/",
            "plugins/",
            "vendor/",
            "distribution/",
            ".github/workflows/",
            "Cargo.toml",
            "CHANGELOG.md",
        ],
        why: "so a change there is verified with the maintenance script tests, \
              the config reference docs check among them",
    },
    PathCheck {
        name: "ui-architecture",
        paths: &[
            "src/ui",
            "src/app/",
            "src/server/",
            "scripts/test_ui_hot_path_architecture.py",
        ],
        why: "so a change there is verified with the UI hot-path architecture tests",
    },
    PathCheck {
        name: "integration-assets",
        paths: &["src/integration/assets/"],
        why: "so a change there is verified with the integration asset tests",
    },
    PathCheck {
        name: "docs-contract",
        paths: &["scripts/docs/"],
        why: "so a change there is verified with the docs contract tests",
    },
];

impl PathCheck {
    fn of(name: &str) -> Option<&'static PathCheck> {
        PATH_CHECKS.iter().find(|check| check.name == name)
    }

    /// Whether a diff's changed paths, one per line, touch the check's paths.
    fn touched_by(&self, changed: &str) -> bool {
        changed
            .lines()
            .any(|path| self.paths.iter().any(|prefix| path.starts_with(prefix)))
    }

    /// The paths as the contract and the events name them.
    fn paths_text(&self) -> String {
        self.paths
            .iter()
            .map(|path| format!("`{path}`"))
            .collect::<Vec<_>>()
            .join(" or ")
    }
}

impl RunFinish {
    /// The grants the run runs with, as clients see them.
    pub(super) fn run_grants(&self) -> Vec<crate::api::schema::TodoRunGrant> {
        self.prepare
            .iter()
            .map(super::capabilities::PreparePlan::run_grant)
            .collect()
    }

    /// The path checks the verify may add: those preflight stored, after
    /// the `tests` check of a run an older build stored.
    pub(super) fn path_checks(&self) -> Vec<RunCheck> {
        let mut checks: Vec<RunCheck> = self.contract_check.iter().cloned().collect();
        for check in &self.path_checks {
            if !checks.iter().any(|known| known.name == check.name) {
                checks.push(check.clone());
            }
        }
        checks
    }
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
            next_run_id: self.info.next_run_id.clone(),
            next_refusal: self.info.next_refusal.clone(),
            ..self.finish.clone()
        }
    }

    /// Whether the run is done and what its close named after it is not
    /// settled yet: the next item's start, or the stop reason's notice.
    pub(super) fn follow_up_pending(&self) -> bool {
        self.info.status == TodoRunStatus::Done
            && ((self.finish.next.is_some()
                && self.info.next_run_id.is_none()
                && self.info.next_refusal.is_none())
                || (self.finish.stop_reason.is_some() && !self.finish.stop_notified))
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
    let mut envs = lock(&RUN_ENV).clone();
    envs.extend(queue::envs_for_handoff());
    envs
}

/// Takes the runs' caller environments the old server of a live handoff
/// sent, before this server resumes its runs. An environment a run already
/// has here is newer and stays.
#[cfg(unix)]
pub(crate) fn restore_handed_off_envs(envs: BTreeMap<String, HashMap<String, String>>) {
    let mut run_env = lock(&RUN_ENV);
    for (key, env) in envs {
        if let Some((run_id, env)) = queue::restore_handed_off_env(key, env) {
            run_env.entry(run_id).or_insert(env);
        }
    }
}

/// The runs a driver of this process works on, so one runs at a time,
/// each with whether another driver was asked for meanwhile.
static DRIVING: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());

struct Driving {
    run_id: String,
    released: bool,
    /// The run's lock while this driver holds it. It goes before the claim
    /// does: a driver that claims the run next in this process then finds
    /// the lock free instead of waiting for it as if another server held it.
    held: Option<File>,
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
                    held: None,
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
                self.held = None;
                driving.remove(&self.run_id);
                drop(driving);
                self.released = true;
                claim_released();
                true
            }
        }
    }
}

impl Drop for Driving {
    fn drop(&mut self) {
        if !self.released {
            let mut driving = lock(&DRIVING);
            self.held = None;
            driving.remove(&self.run_id);
            drop(driving);
            claim_released();
        }
    }
}

/// Wakes the waits on a driver's claim when it lets go: a `todo.wait` on
/// a done run whose next start it cut off ([`WorkerSupervisor::starting_next`]),
/// and a test's [`wait_undriven`].
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

/// Test only: blocks until `done` holds, read again at every run or queue
/// write the store announces. `done` runs without the announcement's lock.
#[cfg(all(test, unix))]
pub(super) fn wait_until(what: &str, hang_guard: Duration, mut done: impl FnMut() -> bool) {
    // delay: not a wait for the condition (each announced write wakes it),
    // the test harness's re-check of its hang guard, as `wait_undriven`'s.
    const HANG_CHECK: Duration = Duration::from_millis(100);
    let started = std::time::Instant::now();
    loop {
        let seen = *lock(&CHANGES.generation);
        if done() {
            return;
        }
        assert!(started.elapsed() < hang_guard, "{what} did not happen");
        let generation = lock(&CHANGES.generation);
        if *generation == seen {
            drop(CHANGES.changed.wait_timeout(generation, HANG_CHECK));
        }
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

/// The checks `names` names, in order, with their argv: each registered in
/// the checks file, with a program, and named once.
fn registered_checks(checks: &ChecksFile, names: &[String]) -> Result<Vec<RunCheck>, String> {
    if names.is_empty() {
        return Err(format!("the run needs at least one check of {CHECKS_FILE}"));
    }
    let mut registered = Vec::new();
    for name in names {
        if registered
            .iter()
            .any(|check: &RunCheck| &check.name == name)
        {
            return Err(format!("check {name:?} is named twice"));
        }
        let argv = checks.checks.get(name).cloned().ok_or_else(|| {
            format!(
                "no check {name:?} in {CHECKS_FILE}; it has: {}",
                checks.checks.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
        if argv.first().is_none_or(|program| program.is_empty()) {
            return Err(format!("check {name:?} in {CHECKS_FILE} has no program"));
        }
        registered.push(RunCheck {
            name: name.clone(),
            argv,
        });
    }
    Ok(registered)
}

/// The [`PATH_CHECKS`] the verify may add to a run: those the repository
/// registers (with a program) and the run does not name.
fn path_checks_of(checks: &ChecksFile, registered: &[RunCheck]) -> Vec<RunCheck> {
    PATH_CHECKS
        .iter()
        .filter(|path_check| !registered.iter().any(|check| check.name == path_check.name))
        .filter_map(|path_check| {
            checks
                .checks
                .get(path_check.name)
                .filter(|argv| argv.first().is_some_and(|program| !program.is_empty()))
                .map(|argv| RunCheck {
                    name: path_check.name.to_owned(),
                    argv: argv.clone(),
                })
        })
        .collect()
}

fn read_checks(repo: &Path) -> Result<ChecksFile, String> {
    let path = repo.join(CHECKS_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

/// Preflight's free disk check: at least `min_free_gib` (or
/// [`DEFAULT_MIN_FREE_GIB`]) free on the file system `repo` is on.
fn check_free_disk(repo: &Path, preflight: &PreflightConfig) -> Result<(), String> {
    let min_free = min_free_gib(preflight);
    let free = free_gib(repo)?;
    if free < min_free {
        return Err(format!(
            "{free:.1} GiB free on {}, less than {min_free} GiB",
            repo.display()
        ));
    }
    Ok(())
}

/// Whether preflight's free disk check fails for `repo` now; a checks file
/// or a free space it cannot read is no shortage.
pub(super) fn short_of_disk(repo: &Path) -> bool {
    read_checks(repo)
        .is_ok_and(|checks| free_gib(repo).is_ok_and(|free| free < min_free_gib(&checks.preflight)))
}

fn min_free_gib(preflight: &PreflightConfig) -> f64 {
    preflight.min_free_gib.unwrap_or(DEFAULT_MIN_FREE_GIB)
}

/// Runs the folder slot's target sweep ([`super::slot::bound_target`]),
/// which frees the slot's build artifacts; a slot not created yet has
/// nothing to sweep.
pub(super) fn sweep_slot(repo: &Path) -> Result<(), String> {
    match slot_dir(repo).filter(|slot| slot.is_dir()) {
        Some(slot) => super::slot::bound_target(&slot).map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

/// The free space of the file system `dir` is on, in GiB, from `df -Pk`.
/// A test reads it from `free-gib-for-test` beside `dir` when that exists.
fn free_gib(dir: &Path) -> Result<f64, String> {
    #[cfg(test)]
    if let Some(free) = dir
        .parent()
        .and_then(|parent| std::fs::read_to_string(parent.join("free-gib-for-test")).ok())
        .and_then(|text| text.trim().parse::<f64>().ok())
    {
        return Ok(free);
    }
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

pub(super) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
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
/// exact commit subject with no body or trailers, the paths it may touch,
/// that it runs everything in the foreground (herdr denies background waits,
/// [`super::policy::background_wait_denial`]), the checks the verify runs,
/// which it runs first where its sandbox lets it, and its last line; for a
/// later attempt, first what its branch starts from ([`carry_note`]).
fn worker_task(run: &Run, carried: Option<&str>) -> String {
    let info = &run.info;
    format!(
        "{}\n\n---\n{}Run everything in the foreground: no `run_in_background`, no Monitor. \
         You are a headless worker: nothing wakes you after your turn ends.\nCommit your work as exactly one commit whose message is exactly this \
         subject, with no body and no trailers:\n{}\nTouch only these paths (git globs): \
         {}\n{}Advertised client methods keep their v1 shape: add a new method instead of \
         changing one (AGENTS.md, Stable client endpoint contract).\nEnd your last reply with \
         the line `WORKER-DONE <sha> | <summary>`, or `WORKER-BLOCKED <reason>` when you \
         cannot finish.\n",
        info.task.trim_end(),
        carried.unwrap_or_default(),
        info.message,
        info.paths.join(" "),
        checks_note(run),
    )
}

/// The contract's part about the verify's checks: each by name and argv,
/// the [`PATH_CHECKS`] the verify adds for their paths, and that the worker
/// runs them before its last line and reports each result.
fn checks_note(run: &Run) -> String {
    let argv = |check: &RunCheck| serde_json::to_string(&check.argv).unwrap_or_default();
    let mut note =
        String::from("The verify runs these checks in your folder, each as its argv (no shell):\n");
    for check in &run.checks {
        note.push_str(&format!("- `{}`: `{}`\n", check.name, argv(check)));
    }
    for check in run.finish.path_checks() {
        let Some(path_check) = PathCheck::of(&check.name) else {
            continue;
        };
        note.push_str(&format!(
            "- `{}`: `{}`, added by the verify when your diff touches {}, {}\n",
            check.name,
            argv(&check),
            path_check.paths_text(),
            path_check.why,
        ));
    }
    note.push_str(
        "Before your last line, run every one of them you can in your sandbox (`windows-lint` \
         works there) and report each one's result, or the sandbox error that stopped it.\n",
    );
    note
}

/// The next attempt's task: the previous attempt's with its review.
fn next_task(task: &str, attempt: u32, review: &str) -> String {
    format!(
        "{}\n\nReview of attempt {attempt}:\n{}",
        task.trim_end(),
        review.trim()
    )
}

/// What a later attempt's worker is told about the commit its branch
/// starts from, before the run's contract.
fn carry_note(run: &Run, start: Option<&str>) -> Option<String> {
    let from = run.current.from_attempt?;
    Some(match (&run.current.from_commit, start) {
        (Some(commit), Some(_)) => format!(
            "Your branch already holds attempt {from}'s commit {commit}, cherry-picked onto \
             the base: build on it and amend that commit (`git commit --amend`), so the \
             branch keeps exactly one commit.\n"
        ),
        _ => format!(
            "Your branch starts from the base, without a commit of attempt {from}; its branch \
             {} shows what that attempt did.\n",
            branch_of(&run.info.item, &run.info.run_id, from)
        ),
    })
}

/// A `Herdr-Run` trailer's value: the run and the attempt.
fn run_trailer(run: &TodoRunInfo) -> String {
    format!("{}/{}", run.run_id, run.attempt)
}

/// The previous attempt's commit cherry-picked onto `base` without a
/// worktree (`git merge-tree`, then `git commit-tree` with its author,
/// committer, dates and message, so the same pick makes the same sha
/// again). The inner error is the conflict.
fn carry_commit(repo: &Path, base: &str, from: &str) -> Result<Result<String, String>, String> {
    let parent = git(
        repo,
        &["rev-parse", "--verify", &format!("{from}^{{commit}}^")],
    )
    .map_err(|error| format!("commit {from} has no parent to pick it from: {error}"))?
    .trim()
    .to_owned();
    let merged = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            &format!("--merge-base={parent}"),
            base,
            from,
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    let out = String::from_utf8_lossy(&merged.stdout);
    match merged.status.code() {
        Some(0) => {}
        Some(1) => {
            let conflict: Vec<&str> = out
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            return Ok(Err(format!(
                "attempt commit {from} does not cherry-pick onto the base {base}: {}",
                conflict.join("; ")
            )));
        }
        _ => {
            return Err(format!(
                "git merge-tree {base} {from}: {}",
                String::from_utf8_lossy(&merged.stderr).trim()
            ))
        }
    }
    let tree = out.lines().next().unwrap_or_default().trim().to_owned();
    let fields = git(
        repo,
        &[
            "log",
            "-1",
            "--date=raw",
            "--format=%an%x00%ae%x00%ad%x00%cn%x00%ce%x00%cd%x00%B",
            from,
        ],
    )?;
    let fields: Vec<&str> = fields.splitn(7, '\0').collect();
    let [author, author_email, author_date, committer, committer_email, committer_date, message] =
        fields[..]
    else {
        return Err(format!("cannot read commit {from}'s author and message"));
    };
    let created = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit-tree", &tree, "-p", base, "-m", message.trim()])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", author)
        .env("GIT_AUTHOR_EMAIL", author_email)
        .env("GIT_AUTHOR_DATE", author_date)
        .env("GIT_COMMITTER_NAME", committer)
        .env("GIT_COMMITTER_EMAIL", committer_email)
        .env("GIT_COMMITTER_DATE", committer_date)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !created.status.success() {
        return Err(format!(
            "git commit-tree {tree}: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        ));
    }
    Ok(Ok(String::from_utf8_lossy(&created.stdout)
        .trim()
        .to_owned()))
}

/// A branch's commits since `base`, oldest first, and their diff stat.
fn branch_evidence(repo: &Path, base: &str, branch: &str) -> (Option<String>, Vec<String>) {
    let diff_stat = git(repo, &["diff", "--stat", base, branch])
        .ok()
        .map(|stat| stat.trim_end().to_owned())
        .filter(|stat| !stat.is_empty());
    let commits = git(
        repo,
        &["rev-list", "--reverse", &format!("{base}..{branch}")],
    )
    .map(|out| out.lines().map(str::to_owned).collect())
    .unwrap_or_default();
    (diff_stat, commits)
}

/// A sha or a prefix of one, as `--commit` takes it: 7 to 64 hex digits.
fn check_commit(commit: &str) -> Result<String, WorkerError> {
    let commit = commit.trim().to_ascii_lowercase();
    if (7..=64).contains(&commit.len()) && commit.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(commit)
    } else {
        Err(WorkerError::Invalid(format!(
            "{commit:?} is not a commit sha (7 to 64 hex digits)"
        )))
    }
}

/// The `Herdr-Item` and `Herdr-Run` trailers of `commit` in `repo`: the
/// item, the run and the attempt.
fn landing_trailers(repo: &Path, commit: &str) -> Option<(String, String, u32)> {
    let trailers = git(
        repo,
        &["log", "-1", "--format=%(trailers:only,unfold)", commit],
    )
    .ok()?;
    let value = |key: &str| {
        trailers.lines().rev().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(key)
                .then(|| value.trim().to_owned())
        })
    };
    let (run_id, attempt) = value("Herdr-Run")?
        .split_once('/')
        .and_then(|(run, attempt)| Some((run.to_owned(), attempt.parse().ok()?)))?;
    Some((value("Herdr-Item").unwrap_or_default(), run_id, attempt))
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
        TodoEventKind::RetryConflict => vec![TodoAction::Retry, TodoAction::Abort],
        TodoEventKind::Draft => vec![TodoAction::Retry, TodoAction::Abort],
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

pub(super) fn store_error(error: rusqlite::Error) -> WorkerError {
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

/// Claude's usage at 0%: what a test's usage gate reads from the
/// "provider" unless the test sets another answer.
#[cfg(test)]
pub(super) fn low_usage_for_test() -> crate::api::schema::ProviderUsage {
    use crate::api::schema::{ProviderUsage, ProviderUsageStatus, UsageWindow};
    let mut claude = ProviderUsage::pending("claude", "Claude");
    claude.status = ProviderUsageStatus::Ok;
    claude.windows = vec![UsageWindow {
        id: "five_hour".into(),
        label: "5h".into(),
        used_percent: 0,
        resets_at: None,
        observed_at: None,
        freshness: None,
        error_kind: None,
    }];
    claude
}

/// What preflight found.
struct Preflighted {
    base: String,
    checks: Vec<RunCheck>,
    install: Option<InstallCommand>,
    path_checks: Vec<RunCheck>,
    /// The granted prepare of the base commit's operations file.
    prepare: Option<super::capabilities::PreparePlan>,
}

impl WorkerSupervisor {
    pub(super) fn run_store(&self) -> Result<&super::store::Store, WorkerError> {
        self.shared
            .store
            .as_ref()
            .map_err(|error| WorkerError::Io(std::io::Error::other(error.clone())))
    }

    /// Claude's usage read from the provider now, in this call (a test's
    /// injected answer instead); never the usage poller's cached report.
    fn read_claude_usage(&self) -> Result<crate::api::schema::ProviderUsage, String> {
        #[cfg(test)]
        {
            lock(&self.shared.claude_usage).clone()
        }
        #[cfg(not(test))]
        {
            crate::usage::read_claude_now()
        }
    }

    #[cfg(all(test, unix))]
    pub(super) fn set_usage_for_test(
        &self,
        answer: Result<crate::api::schema::ProviderUsage, String>,
    ) {
        *lock(&self.shared.claude_usage) = answer;
    }

    /// The usage gate ([`usage_gate`]) for a new run or attempt in `repo`:
    /// refused as `usage_gate` unless `ignore_usage`; otherwise the
    /// decision the run's events record. A refusal closes the repository's
    /// gate and an admission of a closed one reopens it, in the store; an
    /// override changes nothing there.
    pub(super) fn usage_gate(&self, repo: &str, ignore_usage: bool) -> Result<Value, WorkerError> {
        let store = self.run_store()?;
        let closed = store.usage_gate_closed(repo).map_err(store_error)?;
        let answer = self.read_claude_usage();
        let decision = usage_gate::decide(answer.as_ref().map_err(String::as_str), closed);
        let recorded = usage_gate::decision_json(&decision, ignore_usage);
        match decision {
            usage_gate::Decision::Refuse { .. } if ignore_usage => {}
            usage_gate::Decision::Refuse { blockers } => {
                store
                    .set_usage_gate(repo, true, &recorded, now_ms())
                    .map_err(store_error)?;
                return Err(WorkerError::UsageGate(usage_gate::refusal_message(
                    &blockers,
                )));
            }
            usage_gate::Decision::Admit { reopened: true } => store
                .set_usage_gate(repo, false, &recorded, now_ms())
                .map_err(store_error)?,
            usage_gate::Decision::Admit { reopened: false } => {}
        }
        Ok(recorded)
    }

    pub(super) fn load_run(&self, run_id: &str) -> Result<Run, WorkerError> {
        self.run_store()?
            .run(run_id)
            .map_err(store_error)?
            .ok_or_else(|| WorkerError::RunNotFound(format!("run {run_id} not found")))
    }

    /// Starts a run of `params.item`: preflight, then a driver that takes it
    /// from the worker's start on. A refused preflight starts nothing.
    pub(crate) fn todo_run(&self, params: TodoRunParams) -> Result<TodoRunInfo, WorkerError> {
        self.create_run(params, false, None)
    }

    /// Starts a run of `params.item` whose task, subject, paths and checks
    /// the server drafts itself ([`auto_draft`]): preflight of what does not
    /// depend on them, then a driver that drafts them and starts the worker
    /// as [`Self::todo_run`] would. It reviews and answers by itself too.
    pub(crate) fn todo_draft_run(
        &self,
        params: TodoDraftRunParams,
    ) -> Result<TodoRunInfo, WorkerError> {
        let params = TodoRunParams {
            cwd: params.cwd,
            item: params.item,
            task: String::new(),
            message: String::new(),
            paths: Vec::new(),
            checks: Vec::new(),
            owner_pane_id: params.owner_pane_id,
            owner_session_id: params.owner_session_id,
            workspace_id: params.workspace_id,
            env: params.env,
            ignore_usage: params.ignore_usage,
            auto_review: true,
            auto_answer: true,
        };
        self.create_run(params, true, None)
    }

    /// Creates the run and starts its driver. A start of the repository's
    /// queue (`queue`) claims the queue's fencing token in the transaction
    /// that creates the run: refused as `todo_run_active` when another start
    /// moved it first.
    fn create_run(
        &self,
        params: TodoRunParams,
        draft: bool,
        queue: Option<&queue::QueueStart>,
    ) -> Result<TodoRunInfo, WorkerError> {
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
        let preflighted = self.preflight(&params, Path::new(&repo), draft)?;
        let usage = self.usage_gate(&repo, params.ignore_usage)?;
        // The item as the claim records it: its text and every id in TODO.md
        // now, which the close compares with to name the follow-ups.
        let todo = std::fs::read_to_string(Path::new(&repo).join(TODO_FILE)).unwrap_or_default();
        let item_text = todo_titles::item_text(&todo, &params.item);
        let item_ids = todo_titles::item_ids(&todo);
        let at = now_ms();
        let run_id = new_run_id(&repo, &params.item);
        let owner_coordinator = match params.owner_pane_id.as_deref() {
            Some(pane) => self
                .coordinator_of_pane(pane)
                .map(|tenure| tenure.coordinator_id),
            // Claimed outside a pane while a headless item coordinator
            // coordinates the repository (`todo.next`): its run.
            None => store
                .active_coordinator_of(&repo)
                .map_err(store_error)?
                .filter(|tenure| tenure.headless)
                .map(|tenure| tenure.id),
        };
        let mut run = Run {
            info: TodoRunInfo {
                run_id: run_id.clone(),
                repo: repo.clone(),
                item: params.item.clone(),
                step: if draft {
                    TodoStep::Draft
                } else {
                    TodoStep::Start
                },
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
                next_item: None,
                next_run_id: None,
                next_refusal: None,
                stop_reason: None,
                auto_review: params.auto_review,
                auto_answer: params.auto_answer,
                drafted: draft,
                queued: queue.is_some(),
                grants: preflighted
                    .prepare
                    .iter()
                    .map(super::capabilities::PreparePlan::run_grant)
                    .collect(),
            },
            checks: preflighted.checks.clone(),
            owner_pane: params.owner_pane_id.clone(),
            owner_session: params.owner_session_id.clone(),
            workspace: params.workspace_id.clone(),
            owner_coordinator: owner_coordinator.clone(),
            finish: RunFinish {
                install: preflighted.install.clone(),
                path_checks: preflighted.path_checks.clone(),
                auto_review: params.auto_review,
                auto_answer: params.auto_answer,
                draft,
                queued: queue.is_some(),
                prepare: preflighted.prepare.clone(),
                ..RunFinish::default()
            },
            current: Attempt {
                base: Some(preflighted.base.clone()),
                ..Attempt::default()
            },
        };
        let event = json!({
            "type": "run_created",
            "item": params.item,
            "base": preflighted.base,
            "checks": preflighted.checks,
            "install": preflighted.install,
            "path_checks": preflighted.path_checks,
            // The grants the run runs with, none without a prepare.
            "grants": preflighted.prepare,
            "message": params.message,
            "paths": params.paths,
            "task": params.task,
            "step": run.info.step,
            "owner_pane": params.owner_pane_id,
            "owner_session": params.owner_session_id,
            "workspace": params.workspace_id,
            "owner_coordinator": owner_coordinator,
            "ignore_usage": params.ignore_usage,
            "auto_review": params.auto_review,
            "auto_answer": params.auto_answer,
            "draft": draft,
            "queue_token": queue.map(|start| start.token + 1),
            "usage_gate": usage,
            "item_text": item_text,
            "item_ids": item_ids,
        });
        let claimed = store.transaction(|tx| {
            if let Some(start) = queue {
                let reason = format!("run {run_id} of {} started by the queue", params.item);
                let claim = start.run(&run_id, &params.item);
                if !tx.queue_claim(&repo, start.token, &claim, &reason, at)? {
                    return Ok(false);
                }
            }
            tx.run_event(&mut run, &event, false, at)?;
            Ok(true)
        });
        match claimed {
            Ok(true) => {}
            Ok(false) => {
                return Err(WorkerError::RunActive(format!(
                    "the queue of {repo} started another run or was paused since it was read (its \
                     fencing token moved)"
                )))
            }
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
        // A headless item coordinator's run uses the environment of the
        // pane that started it, not the coordinator's sandboxed one.
        let env = match (&params.owner_pane_id, &owner_coordinator) {
            (None, Some(_)) => super::item_coordinators::coordinator_env(&repo).or(params.env),
            _ => params.env,
        };
        if let Some(env) = env {
            lock(&RUN_ENV).insert(run_id.clone(), env);
        }
        announce();
        self.spawn_driver(&run_id);
        Ok(run.info)
    }

    /// Whether `caller` may resume the run, and why it takes the run over
    /// when it does: `None` for its owner (or anyone, for a run nobody
    /// owns resumed from outside a pane). A run owned by another pane that
    /// is still there is refused; once that pane or its agent is gone
    /// (herdr's own events, never a timer), the caller takes it over. A
    /// caller bound to the tenure that owns the run is its owner wherever
    /// it runs now: the run moves to its pane.
    fn run_takeover(
        &self,
        run: &Run,
        caller: Option<&str>,
        caller_coordinator: Option<&str>,
    ) -> Result<Option<String>, WorkerError> {
        match (run.owner_pane.as_deref(), caller) {
            (Some(owner), Some(caller)) if owner == caller => Ok(None),
            (_, Some(caller))
                if caller_coordinator.is_some()
                    && caller_coordinator == run.owner_coordinator.as_deref() =>
            {
                Ok(Some(format!(
                    "its coordinator {} is bound to pane {caller} now",
                    run.owner_coordinator.as_deref().unwrap_or_default()
                )))
            }
            (None, None) => Ok(None),
            (None, Some(_)) => Ok(Some("the run had no owner".to_owned())),
            (Some(owner), _) => match self.gone_owner(owner) {
                Some(cause) => Ok(Some(format!("its owner pane {owner} is gone: {cause}"))),
                None => Err(Self::owned_elsewhere(run)),
            },
        }
    }

    fn owned_elsewhere(run: &Run) -> WorkerError {
        let owner = run.owner_pane.as_deref().unwrap_or("none");
        let session = run
            .owner_session
            .as_deref()
            .map(|session| format!(" (agent session {session})"))
            .unwrap_or_default();
        WorkerError::RunOwnedElsewhere(format!(
            "run {} belongs to pane {owner}{session}; resume it from that pane, or from another \
             once that pane or its agent is gone, which takes the run over",
            run.info.run_id
        ))
    }

    /// The owner panes of the runs that have not ended.
    pub(super) fn run_owner_panes(&self) -> Vec<String> {
        let runs = match self.run_store().map(|store| store.runs(None)) {
            Ok(Ok(runs)) => runs,
            Ok(Err(error)) => {
                warn!(%error, "cannot read the todo runs' owners");
                return Vec::new();
            }
            Err(_) => return Vec::new(),
        };
        runs.into_iter()
            .filter(|run| {
                matches!(
                    run.info.status,
                    TodoRunStatus::Running | TodoRunStatus::Waiting | TodoRunStatus::Blocked
                )
            })
            .filter_map(|run| run.owner_pane)
            .collect()
    }

    pub(super) fn run_active(active: &Run) -> WorkerError {
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
    /// the free disk, a free and clean folder slot, `master`'s commit, and
    /// the operations file at that commit: its requests enforceable and its
    /// prepare granted ([`super::capabilities`]).
    /// A run the server drafts has no task, message, paths or checks yet:
    /// its draft is checked when it comes ([`auto_draft`]).
    fn preflight(
        &self,
        params: &TodoRunParams,
        repo: &Path,
        draft: bool,
    ) -> Result<Preflighted, WorkerError> {
        let refuse = |why: String| WorkerError::Preflight(format!("preflight: {why}"));
        super::check_item_id(&params.item)?;
        if !draft {
            if params.task.trim().is_empty() {
                return Err(refuse("the task text is empty".into()));
            }
            check_message(&params.message).map_err(refuse)?;
            check_paths(&params.paths).map_err(refuse)?;
        }
        if !todo_titles::read_titles(repo).contains_key(&params.item) {
            return Err(refuse(format!(
                "item {} is not in {}",
                params.item,
                repo.join("TODO.md").display()
            )));
        }
        let checks = read_checks(repo).map_err(refuse)?;
        let registered = if draft {
            Vec::new()
        } else {
            registered_checks(&checks, &params.checks).map_err(refuse)?
        };
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
        check_free_disk(repo, &checks.preflight).map_err(refuse)?;
        self.check_slot_free(repo).map_err(refuse)?;
        let base = git(repo, &["rev-parse", "--verify", "master^{commit}"])
            .map_err(|error| refuse(format!("the repository has no master commit: {error}")))?
            .trim()
            .to_owned();
        let path_checks = path_checks_of(&checks, &registered);
        let prepare = self.plan_operations(repo, &base)?;
        Ok(Preflighted {
            base,
            checks: registered,
            install: checks.install,
            path_checks,
            prepare,
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
        self.todo_status_raw(run_id).map(decision::shown)
    }

    fn todo_status_raw(&self, run_id: &str) -> Result<TodoRunInfo, WorkerError> {
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

    /// Whether a done run's driver of this process is still starting the
    /// next item its close named: `todo.wait` returns once that start or
    /// its refusal (a `next_refused` event) is recorded, so a refusal is
    /// the wait's result, not an event after it. Without a driver (a server
    /// that ended before the start; the one that starts settles it) the
    /// wait returns the run as it is.
    fn starting_next(run: &Run) -> bool {
        run.info.status == TodoRunStatus::Done
            && run.finish.next.is_some()
            && run.info.next_run_id.is_none()
            && run.info.next_refusal.is_none()
            && lock(&DRIVING).contains_key(&run.info.run_id)
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

    /// The runs, of the repository `repo` is in when given, oldest first;
    /// with `commit`, only the run that landed it ([`Self::landing_of`],
    /// whose trailers are read in `repo`), and that landing.
    pub(crate) fn todo_runs(
        &self,
        repo: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(Vec<TodoRunInfo>, Option<TodoLanding>), WorkerError> {
        if let Some(commit) = commit {
            let landing = self.landing_of(repo, commit)?;
            let runs = self
                .run_store()?
                .run(&landing.run_id)
                .map_err(store_error)?
                .map(|run| run.info)
                .into_iter()
                .collect();
            return Ok((runs, Some(landing)));
        }
        let repo = repo.map(|dir| repository_of(Path::new(dir)).unwrap_or_else(|| dir.to_owned()));
        let runs = self
            .run_store()?
            .runs(repo.as_deref())
            .map_err(store_error)?
            .into_iter()
            .map(|run| decision::shown(run.info))
            .collect();
        Ok((runs, None))
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
            let latest = store
                .latest_run_event(&params.run_id)
                .map_err(store_error)?;
            // A decision step that fails shows as a blocked event, named by
            // the event the run waits on: a wait after it waits for the run
            // to go on.
            if let Some(event) =
                decision::failure_event(&run.info, latest.as_ref().map(|(seq, _)| *seq))
                    .filter(|event| params.after.is_none_or(|after| event.event_id > after))
            {
                return Ok(Some((event, decision::shown(run.info))));
            }
            if let Some((seq, body)) = latest {
                let ended = matches!(
                    run.info.status,
                    TodoRunStatus::Done | TodoRunStatus::Blocked | TodoRunStatus::Aborted
                );
                // A review the server makes itself is not the coordinator's
                // until it escalates.
                let waits_on = run.info.status == TodoRunStatus::Waiting
                    && run.info.pending_event == Some(seq)
                    && !auto_review::is_due(&run, seq, &body)
                    && !auto_answer::is_due(&run, seq, &body);
                let fresh = params.after.is_none_or(|after| seq > after);
                if (ended && !Self::starting_next(&run)) || (waits_on && fresh) {
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
        let caller_coordinator = params
            .caller_pane_id
            .as_deref()
            .and_then(|pane| self.coordinator_of_pane(pane))
            .map(|tenure| tenure.coordinator_id);
        let takeover = self.run_takeover(
            &run,
            params.caller_pane_id.as_deref(),
            caller_coordinator.as_deref(),
        )?;
        let answered_body = latest
            .filter(|(seq, _)| *seq == params.event)
            .map(|(_, body)| body);
        let answered = answered_body
            .as_ref()
            .map(|body| event_of(params.event, body, &run.info));
        let kind = answered
            .as_ref()
            .map_or(TodoEventKind::Unknown, |event| event.kind);
        // The commit the event showed: what an approval binds.
        let reviewed = answered.and_then(|event| event.commits.last().cloned());
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
        if params.keep_open {
            if params.action != TodoAction::Approve {
                return Err(WorkerError::Invalid("only approve takes keep_open".into()));
            }
            if params.close.is_some() {
                return Err(WorkerError::Invalid(
                    "keep_open and a closing decision exclude each other".into(),
                ));
            }
        }
        self.check_after_close(&run, &params)?;
        // In queue mode an approval closes the item as the queue's own
        // review does, unless it keeps it open (a note, or keep_open): the
        // queue then never runs an approved item again.
        let queue_close = (params.action == TodoAction::Approve
            && params.note.is_none()
            && params.close.is_none()
            && !params.keep_open
            && (run.finish.queued || self.queue_is_on(&run.info.repo)))
        .then(|| {
            queue::queue_close(
                &run,
                reviewed.as_deref(),
                None,
                "the coordinator (`herdr todo resume --action approve`)",
            )
        });
        let mut note = None;
        let mut usage = None;
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
                // Without waking the run's driver: this resume moves the run
                // on itself below. A driver woken here would apply the
                // answered question first and make this event stale.
                match self.answer_for_run(&answer) {
                    Ok(_) => {}
                    // Answered elsewhere (the user's `?` list) or withdrawn:
                    // nothing left to answer, so the run goes on.
                    Err(error @ (WorkerError::QuestionGone(_) | WorkerError::NoQuestion(_))) => {
                        note = Some(error.to_string());
                    }
                    Err(error) => return Err(error),
                }
            }
            // The draft's question answered: the server drafts again with
            // the answer. No worker starts yet, so no usage gate.
            TodoAction::Retry if kind == TodoEventKind::Draft => {
                if params
                    .task
                    .as_deref()
                    .is_none_or(|task| task.trim().is_empty())
                {
                    return Err(WorkerError::Invalid(
                        "retry of a draft event needs the answer to its question as task text, \
                         which the server drafts again with"
                            .into(),
                    ));
                }
            }
            // The attempt whose commit did not cherry-pick starts from the
            // base instead; its review and usage gate were the retry's.
            TodoAction::Retry if kind == TodoEventKind::RetryConflict => {
                if params.task.is_some() {
                    return Err(WorkerError::Invalid(
                        "a retry after retry_conflict starts the attempt from the base with \
                         the review it has; it takes no task text"
                            .into(),
                    ));
                }
            }
            TodoAction::Retry => {
                let task = params.task.as_deref().unwrap_or_default();
                if task.trim().is_empty() {
                    return Err(WorkerError::Invalid(
                        "retry needs the review of the attempt, which the next attempt's task \
                         appends"
                            .into(),
                    ));
                }
                // A new attempt starts a new worker: the usage gate applies
                // (not to a retry after the last attempt, which only blocks
                // the run). The run itself is not touched by a refusal: it
                // keeps waiting on the same event.
                if run.info.attempt < MAX_ATTEMPTS {
                    usage = Some(self.usage_gate(&run.info.repo, params.ignore_usage)?);
                }
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
        // A headless item coordinator's resume (no pane) of its tenure's
        // run uses the environment of the pane that started it.
        let headless_env = match (&params.caller_pane_id, &run.owner_coordinator) {
            (None, Some(tenure)) => self
                .run_store()?
                .coordinator(tenure)
                .map_err(store_error)?
                .filter(|tenure| tenure.headless)
                .and_then(|_| super::item_coordinators::coordinator_env(&run.info.repo)),
            _ => None,
        };
        let env = headless_env.or_else(|| params.env.clone());
        match &env {
            Some(env) => lock(&RUN_ENV).insert(params.run_id.clone(), env.clone()),
            None => lock(&RUN_ENV).remove(&params.run_id),
        };
        let event = json!({
            "type": "run_resumed",
            "event": params.event,
            "action": params.action,
            "task": params.task,
            "request_id": params.request_id,
            "env_sent": env.is_some(),
            "note": note,
            "todo_note": params.note,
            "close": params.close.as_ref().or(queue_close.as_ref()),
            "queue_close": queue_close.is_some(),
            "keep_open": params.keep_open,
            "next": params.next,
            "stop_reason": params.stop_reason,
            "message": (params.action == TodoAction::Abort).then_some(&params.message),
            "ignore_usage": params.ignore_usage,
            "usage_gate": usage,
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
                // Another resume took the run over meanwhile.
                if current.owner_pane != run.owner_pane {
                    return Ok(Err(Self::owned_elsewhere(&current)));
                }
                if let Some(cause) = &takeover {
                    let taken = json!({
                        "type": "run_owner_taken",
                        "event": params.event,
                        "from_pane": current.owner_pane,
                        "from_session": current.owner_session,
                        "from_workspace": current.workspace,
                        "to_pane": params.caller_pane_id,
                        "to_session": params.caller_session_id,
                        "to_workspace": params.caller_workspace_id,
                        "cause": cause,
                    });
                    tx.run_note(&current.info.run_id, &taken, now_ms())?;
                    let owner = super::store::RunOwner {
                        pane_id: params.caller_pane_id.as_deref(),
                        session_id: params.caller_session_id.as_deref(),
                        workspace: params.caller_workspace_id.as_deref(),
                        coordinator_id: caller_coordinator.as_deref(),
                    };
                    tx.move_run_owner(&current.info.run_id, &owner, cause, now_ms())?;
                    current.owner_pane = params.caller_pane_id.clone();
                    current.owner_session = params.caller_session_id.clone();
                    current.workspace = params.caller_workspace_id.clone();
                    current.owner_coordinator = caller_coordinator.clone();
                }
                let was_blocked = current.info.status == TodoRunStatus::Blocked;
                current.info.status = TodoRunStatus::Running;
                match params.action {
                    TodoAction::Answer => current.info.step = TodoStep::Attention,
                    TodoAction::Approve => {
                        current.info.step = TodoStep::Stop;
                        current.finish.note = params.note.clone();
                        current.finish.close = params.close.clone().or_else(|| queue_close.clone());
                        current.finish.next = params.next.clone();
                        current.finish.stop_reason = params.stop_reason.clone();
                        current.info.next_item = params.next.as_ref().map(|next| next.item.clone());
                        current.info.stop_reason = params.stop_reason.clone();
                        current.current.review_event = Some(params.event);
                        current.current.review_decision = Some(APPROVE.to_owned());
                        current.current.review_text = None;
                        current.current.commit = reviewed.clone();
                        current.current.base = current.info.base.clone();
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
                    TodoAction::Retry if kind == TodoEventKind::Draft => {
                        current.info.step = TodoStep::Draft;
                        let answer = auto_draft::answer_note(
                            params.event,
                            answered_body.as_ref(),
                            params.task.as_deref().unwrap_or_default().trim(),
                            "coordinator",
                        );
                        tx.run_note(&current.info.run_id, &answer, now_ms())?;
                    }
                    TodoAction::Retry if kind == TodoEventKind::RetryConflict => {
                        current.info.step = TodoStep::Start;
                        current.current.from_commit = None;
                    }
                    TodoAction::Retry => {
                        current.info.step = TodoStep::Restart;
                        current.current.review_event = Some(params.event);
                        current.current.review_decision = Some(RETRY.to_owned());
                        current.current.review_text = params.task.clone();
                    }
                    // The driver's abort step stops the worker, then ends
                    // the run. An abort at that step (of its `still_alive`
                    // event) keeps the first reason.
                    TodoAction::Abort if current.info.step != TodoStep::Abort => {
                        current.current.review_event = Some(params.event);
                        current.current.review_decision = Some(ABORT.to_owned());
                        current.current.review_text = None;
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
        // The coordinator handled the worker's events up to the one it
        // answered: its owner owes nothing more for them.
        if let (Some(worker_id), Some(seq)) = (&run.info.worker_id, run.info.last_acked_seq) {
            self.ack_quietly(worker_id, seq);
        }
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
        // An answered event (an escalation among them) is one of the
        // queue's events; the run in progress keeps it from starting more.
        self.queue_event(&run.info.repo);
        Ok(run.info)
    }

    /// What follows a close: a closing decision names the next item to
    /// start (`next`) or why none (`stop_reason`), never neither, never
    /// both; neither comes without a close. The next run's parameters are
    /// checked here as far as they do not depend on the repository's state
    /// when it starts (its preflight checks the rest then).
    /// In queue mode a close names neither: the queue starts the next item.
    fn check_after_close(&self, run: &Run, params: &TodoResumeParams) -> Result<(), WorkerError> {
        let invalid = |why: String| Err(WorkerError::Invalid(why));
        if params.close.is_some() && self.queue_is_on(&run.info.repo) {
            if params.next.is_some() || params.stop_reason.is_some() {
                return invalid(format!(
                    "queue mode is on for {}: the queue starts the next item, so a close takes \
                     neither --next nor --stop-reason (`herdr todo queue pause` stops it)",
                    run.info.repo
                ));
            }
            return Ok(());
        }
        match (&params.close, &params.next, &params.stop_reason) {
            (None, None, None) => return Ok(()),
            (None, _, _) => {
                return invalid(
                    "only an approval with a closing decision (--close) takes --next or \
                     --stop-reason"
                        .into(),
                )
            }
            (Some(_), None, None) => {
                return invalid(
                    "a closing decision needs --next <item-id> (the item whose run starts once \
                     this run is done) or --stop-reason <text> (why no item starts next), unless \
                     queue mode is on"
                        .into(),
                )
            }
            (Some(_), Some(_), Some(_)) => {
                return invalid("--next and --stop-reason exclude each other".into())
            }
            (Some(_), None, Some(reason)) => {
                if reason.trim().is_empty() {
                    return invalid("the stop reason is empty".into());
                }
                return Ok(());
            }
            (Some(_), Some(_), None) => {}
        }
        let Some(next) = &params.next else {
            return Ok(());
        };
        super::check_item_id(&next.item)?;
        if next.item == run.info.item {
            return invalid(format!(
                "--next names the item this run closes ({})",
                next.item
            ));
        }
        if next.task.trim().is_empty() {
            return invalid("the next run's task text is empty".into());
        }
        check_message(&next.message).map_err(|why| WorkerError::Invalid(format!("next: {why}")))?;
        check_paths(&next.paths).map_err(|why| WorkerError::Invalid(format!("next: {why}")))?;
        Ok(())
    }

    /// Drives every run in progress again, as a server that starts does,
    /// from its step, after acknowledging its worker's events the
    /// coordinator handled (an ack lost to a crash), and settles what a
    /// done run's close named after it ([`Self::follow_up`]). A waiting
    /// run's events stay its owner's obligation.
    pub(crate) fn resume_runs(&self) {
        let runs = match self.run_store().map(|store| store.runs(None)) {
            Ok(Ok(runs)) => runs,
            Ok(Err(error)) => {
                warn!(%error, "cannot read the todo runs");
                return;
            }
            Err(_) => return,
        };
        self.restore_escalations(&runs);
        for run in runs {
            if run.info.status == TodoRunStatus::Running {
                if let (Some(worker_id), Some(seq)) = (&run.info.worker_id, run.info.last_acked_seq)
                {
                    self.ack_quietly(worker_id, seq);
                }
                self.spawn_driver(&run.info.run_id);
            } else if self.auto_review_due(&run).is_some() || self.auto_answer_due(&run).is_some() {
                // Its review or answer was in flight, or decided and not
                // applied.
                self.spawn_driver(&run.info.run_id);
            } else if run.follow_up_pending() {
                // Done before a server ended, its next start or stop notice
                // not settled yet.
                self.spawn_follow_up(&run.info.run_id);
            }
        }
        // The runs in progress are driven again first; then each queue
        // settles what ended while no server looked and starts its next
        // item when its repository has no run.
        self.resume_queues();
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
        driving.held = match self.take_run_lock(run_id) {
            Ok(held) => Some(held),
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
            escalations::settle(&run);
            if run.info.status != TodoRunStatus::Running {
                // A decision the server makes itself (a review, an answer):
                // decided (or the recorded decision taken) and applied, then
                // the run read again.
                let stepped = if let Some((seq, body)) = self.auto_review_due(&run) {
                    Some(self.step_auto_review(&mut run, seq, &body))
                } else {
                    self.auto_answer_due(&run)
                        .map(|(seq, body)| self.step_auto_answer(&mut run, seq, &body))
                };
                match stepped {
                    Some(Ok(())) => {
                        decision::recovered(run_id);
                        continue;
                    }
                    Some(Err(why)) => {
                        #[cfg(test)]
                        if why == CRASHED_HERE {
                            drop(driving);
                            lock(&CRASHED).push(run.info.repo.clone());
                            announce();
                            return;
                        }
                        // Shown blocked with why, never hidden, until the
                        // step goes through: taken again at the next write
                        // the store announces.
                        warn!(run_id, why, "a todo run's decision step failed");
                        let seen = decision::failed(run_id, &why);
                        decision::wait_for_write(seen, || self.handed_off());
                        continue;
                    }
                    None => decision::recovered(run_id),
                }
                let ended = matches!(
                    run.info.status,
                    TodoRunStatus::Done | TodoRunStatus::Blocked | TodoRunStatus::Aborted
                );
                if ended {
                    lock(&RUN_ENV).remove(run_id);
                    // Ended: no driver needs its lock again.
                    let _ = std::fs::remove_file(self.run_lock_path(run_id));
                }
                if driving.release() {
                    // The repository's queue looks again once the run let
                    // go of its claim.
                    if ended {
                        self.queue_after_run(&run.info.repo);
                    }
                    return;
                }
                continue;
            }
            // Running again (a resume answered the event a failed decision
            // step was on): nothing of it is left to show.
            decision::recovered(run_id);
            #[cfg(test)]
            if crashes_before(&run.info.repo, run.info.step) {
                drop(driving);
                lock(&CRASHED).push(run.info.repo.clone());
                announce();
                return;
            }
            let stepped = match run.info.step {
                TodoStep::Draft => self.step_draft(&mut run),
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
        self.step_prepare(run)?;
        self.release_slot(run)?;
        // A later attempt's branch starts from the previous attempt's
        // commit, cherry-picked onto the base; a conflict asks the
        // coordinator, who may start it from the base instead.
        let start = match run.current.from_commit.clone() {
            Some(from) => {
                let base = run.info.base.clone().ok_or("the run has no base")?;
                match carry_commit(Path::new(&run.info.repo), &base, &from)? {
                    Ok(start) => {
                        self.run_step(
                            run,
                            json!({
                                "type": "run_carried",
                                "from_attempt": run.current.from_attempt,
                                "from_commit": from,
                                "start": start,
                            }),
                        )?;
                        Some(start)
                    }
                    Err(conflict) => {
                        run.info.status = TodoRunStatus::Waiting;
                        let mut event = new_event(TodoEventKind::RetryConflict);
                        event.commits = vec![from];
                        event.error = Some(conflict);
                        self.record_run_event(run, &event)?;
                        return Ok(());
                    }
                }
            }
            None => None,
        };
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
            prompt: worker_task(run, carry_note(run, start.as_deref()).as_deref()),
            model: None,
            name: None,
            workspace_id: run.workspace.clone(),
            folder_slot: Some(SLOT.to_owned()),
            branch: Some(branch),
            base: start.or_else(|| run.info.base.clone()),
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
    /// and the worker's events up to there stay unacknowledged until the
    /// coordinator resumes the run ([`Self::todo_resume`]): until then its
    /// owner pane has an obligation for them, as for a worker it started
    /// by hand.
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
            // A report, not a turn's end: the user got its notice, the
            // worker's log and status hold it, and the run keeps waiting.
            WorkerAttentionReason::HeldOutput => {
                self.run_step(run, json!({"type": "run_acked", "seq": seq}))?;
                self.ack_quietly(&worker_id, seq);
                return Ok(());
            }
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
        // The attempt's commit, which the next attempt starts from: its
        // branch's tip when it has commits since the base.
        let repo = PathBuf::from(&run.info.repo);
        let branch = run.info.branch.clone().ok_or("the run has no branch")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        let tip = branch_evidence(&repo, &base, &format!("refs/heads/{branch}"))
            .1
            .last()
            .cloned();
        run.current.commit = tip.clone();
        self.run_step(
            run,
            json!({
                "type": "run_attempt_ended",
                "attempt": run.info.attempt,
                "commit": tip,
            }),
        )?;
        let previous = run.info.worker_id.take();
        let review = run.current.review_text.clone().unwrap_or_default();
        run.info.task = next_task(&run.info.task, run.info.attempt, &review);
        run.current = Attempt {
            base: Some(base),
            from_attempt: Some(run.info.attempt),
            from_commit: tip,
            ..Attempt::default()
        };
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
                "from_commit": run.current.from_commit,
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
    /// `verify_failed` event asking for the next attempt's task. An
    /// approval of the commit a verify already passed (the one the
    /// automatic review ran before it reviewed) takes that verdict instead
    /// of running the checks again; a commit that changed is verified anew.
    fn step_verify(&self, run: &mut Run) -> Result<(), String> {
        let worker_id = run
            .info
            .worker_id
            .clone()
            .ok_or("the run has no worker to verify")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        for (check, reason) in self.path_checks_due(run, &worker_id, &base)? {
            run.info.checks.push(check.name.clone());
            run.checks.push(check.clone());
            self.run_step(
                run,
                json!({
                    "type": "run_check_added",
                    "check": check,
                    "reason": reason,
                    "checks": run.info.checks,
                }),
            )?;
        }
        if let Some(verified) = self.verified_already(run, &worker_id) {
            run.info.step = TodoStep::CherryPick;
            self.run_step(
                run,
                json!({
                    "type": "run_verified",
                    "head": verified.head,
                    "commits": verified.commits,
                    "reused": true,
                    "verified_ms": verified.verified_ms,
                    "step": run.info.step,
                }),
            )?;
            return Ok(());
        }
        self.run_step(
            run,
            json!({"type": "run_verify_intent", "worker_id": worker_id, "checks": run.info.checks}),
        )?;
        let verification = self.verify_attempt(run, &worker_id, &base)?;
        run.current.verification = serde_json::to_string(&verification).ok();
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

    /// The attempt's verdict when it still holds for an approval: it is
    /// `verified`, of the commit the approval names, which the worker's
    /// branch is still at, with every check the run has now.
    fn verified_already(&self, run: &Run, worker_id: &str) -> Option<WorkerVerification> {
        if run.current.review_decision.as_deref() != Some(APPROVE) {
            return None;
        }
        let verification: WorkerVerification =
            serde_json::from_str(run.current.verification.as_deref()?).ok()?;
        let head = verification.head.as_deref()?;
        if verification.verdict != WorkerVerdict::Verified
            || run.current.commit.as_deref() != Some(head)
            || Some(verification.base.as_str()) != run.info.base.as_deref()
        {
            return None;
        }
        let checked = |name: &str| {
            verification
                .checks
                .iter()
                .any(|check| check.name.as_deref() == Some(name))
        };
        if !run.checks.iter().all(|check| checked(&check.name)) {
            return None;
        }
        let worker = self.status(worker_id).ok()?;
        let now = git(Path::new(&worker.cwd), &["rev-parse", "HEAD"]).ok()?;
        (now.trim() == head).then_some(verification)
    }

    /// Runs the verify of the attempt's worker with the run's checks, each
    /// with the caller's environment, or `unavailable` without it.
    fn verify_attempt(
        &self,
        run: &Run,
        worker_id: &str,
        base: &str,
    ) -> Result<WorkerVerification, String> {
        let env = lock(&RUN_ENV).get(&run.info.run_id).cloned();
        let no_env = "this server has not got the caller's environment for the check (a \
                      restart, or a `herdr todo resume` that sent none); `herdr todo resume \
                      --action verify` from the coordinator's shell sends it and verifies again";
        let params = WorkerVerifyParams {
            worker_id: worker_id.to_owned(),
            base: base.to_owned(),
            expected_message: run.info.message.clone(),
            allowed_paths: run.info.paths.clone(),
            command: None,
            generated: Vec::new(),
            env,
        };
        let commands = run
            .checks
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
        self.verify_with(&params, commands)
            .map_err(|error| format!("verifying worker {worker_id}: {error}"))
    }

    /// The run's [`PATH_CHECKS`], each with why, that the verify must add:
    /// the attempt's diff from the base touches the check's paths (or
    /// cannot be read) and the run does not have it yet. Once added (a
    /// `run_check_added` event), a check stays for the later attempts.
    fn path_checks_due(
        &self,
        run: &Run,
        worker_id: &str,
        base: &str,
    ) -> Result<Vec<(RunCheck, String)>, String> {
        let candidates: Vec<RunCheck> = run
            .finish
            .path_checks()
            .into_iter()
            .filter(|check| !run.checks.iter().any(|known| known.name == check.name))
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let changed = self
            .status(worker_id)
            .map_err(|error| error.to_string())
            .and_then(|worker| {
                git(
                    Path::new(&worker.cwd),
                    &["diff", "--name-only", base, "HEAD"],
                )
            });
        Ok(candidates
            .into_iter()
            .filter_map(|check| {
                let reason = match (&changed, PathCheck::of(&check.name)) {
                    (Ok(changed), Some(path_check)) if path_check.touched_by(changed) => {
                        format!("the diff touches {}", path_check.paths_text())
                    }
                    (Ok(_), _) => return None,
                    (Err(error), _) => format!("the diff could not be read: {error}"),
                };
                Some((check, reason))
            })
            .collect())
    }

    /// Picks the attempt's verified commit onto `master` in the
    /// repository's shared checkout, with the `Herdr-Item` and `Herdr-Run`
    /// trailers added to the landed commit's message, and records the
    /// landing. The branch's tip must be the commit and base the approval
    /// named: otherwise the run raises a new `review` instead of landing
    /// another commit. Refused (blocked) when that checkout is not on
    /// `master`, has uncommitted changes or a cherry-pick in progress, or
    /// its `master` no longer contains the base; a conflict is aborted and
    /// blocks too. A commit `master` already has (a pick a crash did not
    /// record) is not picked again, and its trailers are added only while
    /// it is `master`'s head.
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
        if run.current.review_decision.as_deref() == Some(APPROVE)
            && (run.current.commit.as_deref() != Some(commit.as_str())
                || run.current.base != run.info.base)
        {
            return self.review_again(run, &repo, &branch, &base, &commit);
        }
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
        let mut picked = git(&repo, &["log", "--format=%H %s", &format!("{base}..HEAD")])?
            .lines()
            .find_map(|line| {
                let (sha, subject) = line.split_once(' ')?;
                (subject == run.info.message).then(|| sha.to_owned())
            })
            .ok_or_else(|| format!("master has no commit {:?} after the pick", run.info.message))?;
        let trailer = run_trailer(&run.info);
        let mut trailers = landing_trailers(&repo, &picked)
            .is_some_and(|(_, run_id, attempt)| format!("{run_id}/{attempt}") == trailer);
        let head = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
        if !trailers && head == picked {
            // Only the message changes (`--only` without paths leaves the
            // index out); the worker's own commit stays subject-only.
            git(
                &repo,
                &[
                    "commit",
                    "--quiet",
                    "--amend",
                    "--only",
                    "--no-edit",
                    "--no-verify",
                    "--trailer",
                    &format!("Herdr-Item: {}", run.info.item),
                    "--trailer",
                    &format!("Herdr-Run: {trailer}"),
                ],
            )
            .map_err(|error| format!("adding the trailers to {picked}: {error}"))?;
            picked = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
            trailers = true;
        }
        run.info.picked = Some(picked.clone());
        run.info.step = TodoStep::Install;
        let at = now_ms();
        let landing = TodoLanding {
            landed_sha: picked.clone(),
            run_id: run.info.run_id.clone(),
            attempt: run.info.attempt,
            item: run.info.item.clone(),
            worker_commit: Some(commit.clone()),
            worker_id: run.info.worker_id.clone(),
            ts_ms: Some(at),
            source: TodoLandingSource::Store,
        };
        let event = json!({
            "type": "run_picked",
            "commit": picked,
            "worker_commit": commit,
            "trailers": trailers,
            "step": run.info.step,
        });
        let store = self.run_store().map_err(|error| error.to_string())?;
        store
            .transaction(|tx| {
                let seq = tx.run_event(run, &event, false, at)?;
                tx.landing(&landing)?;
                Ok(seq)
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        announce();
        Ok(())
    }

    /// The branch moved since the approval (or the approval named another
    /// base): a new `review` event with the branch's evidence, which the
    /// run waits on; nothing is picked.
    fn review_again(
        &self,
        run: &mut Run,
        repo: &Path,
        branch: &str,
        base: &str,
        tip: &str,
    ) -> Result<(), String> {
        let (diff_stat, commits) = branch_evidence(repo, base, &format!("refs/heads/{branch}"));
        let mut event = new_event(TodoEventKind::Review);
        event.diff_stat = diff_stat;
        event.commits = commits;
        event.error = Some(format!(
            "the approval named commit {} on base {}, but branch {branch} is at {tip} on base \
             {base}; nothing was picked: review it again",
            run.current.commit.as_deref().unwrap_or("none"),
            run.current.base.as_deref().unwrap_or("none"),
        ));
        run.info.step = TodoStep::Review;
        run.info.status = TodoRunStatus::Waiting;
        self.record_run_event(run, &event)?;
        Ok(())
    }

    /// The landing of `commit` (a sha or a prefix of one, the landed
    /// commit or the worker's): the store's record, else the commit's
    /// `Herdr-Run` trailer in `repo`'s history (shas change on the
    /// upstream rebase, trailers stay).
    pub(crate) fn landing_of(
        &self,
        repo: Option<&str>,
        commit: &str,
    ) -> Result<TodoLanding, WorkerError> {
        let commit = check_commit(commit)?;
        let store = self.run_store()?;
        let full = repo.and_then(|dir| {
            git(
                Path::new(dir),
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{commit}^{{commit}}"),
                ],
            )
            .ok()
            .map(|sha| sha.trim().to_owned())
        });
        if let Some(landing) = store
            .landing_of(full.as_deref().unwrap_or(&commit))
            .map_err(store_error)?
        {
            return Ok(landing);
        }
        if let (Some(dir), Some(full)) = (repo, &full) {
            if let Some((item, run_id, attempt)) = landing_trailers(Path::new(dir), full) {
                let worker_id = store
                    .attempts(&run_id)
                    .map_err(store_error)?
                    .into_iter()
                    .find(|row| row.number == attempt)
                    .and_then(|row| row.worker_id);
                return Ok(TodoLanding {
                    landed_sha: full.clone(),
                    run_id,
                    attempt,
                    item,
                    worker_commit: None,
                    worker_id,
                    ts_ms: None,
                    source: TodoLandingSource::Trailers,
                });
            }
        }
        Err(WorkerError::RunNotFound(format!(
            "no todo run landed commit {commit}: the worker store has no landing of it{}",
            match (repo, &full) {
                (None, _) => " (and no repository was given to read its trailers in)".to_owned(),
                (Some(dir), None) => format!(" and {dir} has no such commit"),
                (Some(_), Some(full)) => format!(" and {full} has no Herdr-Run trailer"),
            }
        )))
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

/// The usage poller published a new reading: the queues the usage gate
/// stopped look again ([`WorkerSupervisor::queues_after_usage_reading`]),
/// from a thread so the server's loop does not wait for the store, git or
/// the provider. Nothing without an open supervisor.
pub(crate) fn usage_reading_published() {
    let Some(supervisor) = super::coordinators::installed() else {
        return;
    };
    let spawned = crate::thread_spawn::spawn_named("herdr-todo-queue-usage", move || {
        supervisor.queues_after_usage_reading();
    });
    if let Err(error) = spawned {
        warn!(%error, "cannot re-evaluate the todo queues after a usage reading");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_path_check_is_due_only_for_its_own_paths() {
        let due = |changed: &str| -> Vec<&str> {
            PATH_CHECKS
                .iter()
                .filter(|check| check.touched_by(changed))
                .map(|check| check.name)
                .collect()
        };
        let rust = ["lint", "windows-lint", "tests"];
        for changed in ["src/workers/runs.rs", "build.rs", "tests/fixtures/x.json"] {
            assert_eq!(due(changed), rust, "{changed}");
        }
        assert_eq!(
            due("src/config/mod.rs"),
            ["lint", "windows-lint", "tests", "maintenance"]
        );
        assert_eq!(
            due("src/ui/sidebar.rs"),
            ["lint", "windows-lint", "tests", "ui-architecture"]
        );
        assert_eq!(
            due("src/integration/assets/claude/x.ts"),
            [
                "lint",
                "windows-lint",
                "tests",
                "maintenance",
                "integration-assets"
            ]
        );
        for changed in [
            "docs/next/website/src/content/docs/configuration.mdx",
            "plugins/job/bin/herdr-job",
            ".github/workflows/release.yml",
        ] {
            assert_eq!(due(changed), ["maintenance"], "{changed}");
        }
        assert_eq!(due("scripts/todo_edit.py"), ["tests", "maintenance"]);
        assert_eq!(
            due("scripts/docs/versions.mjs"),
            ["maintenance", "docs-contract"]
        );
        assert_eq!(
            due("scripts/windows_cross.py"),
            ["windows-lint", "maintenance"]
        );
        assert_eq!(due(".herdr/checks.toml"), ["tests"]);
        for changed in ["TODO.md", "DECISIONS.md", "docs/versions/x.md"] {
            assert!(due(changed).is_empty(), "{changed}");
        }
        let maintenance = PathCheck::of("maintenance").unwrap();
        // A prefix matches from the start of a path only, and any line of
        // the diff's name list is enough.
        for changed in [
            "src/workers/runs.rs",
            "docs/versions/x.md",
            "a/scripts/x.py",
        ] {
            assert!(!maintenance.touched_by(changed), "{changed}");
        }
        assert!(maintenance.touched_by("src/main.rs\nscripts/x.py\n"));
    }

    /// Every path the Rust build reads is a path of each check that builds
    /// it, so no Rust change skips one.
    #[test]
    fn the_rust_checks_cover_every_rust_path() {
        for name in ["lint", "windows-lint", "tests"] {
            let check = PathCheck::of(name).unwrap();
            for path in RUST_PATHS {
                assert!(check.paths.contains(&path), "{name} lacks {path}");
            }
        }
    }

    /// The registered checks the install's `just check` runs, from the
    /// justfile: a recipe a check runs as `just <recipe>`, or a command line
    /// whose words start with a check's argv (after `{{slot}}` is dropped
    /// and `{{python}}` read as `python3`), is that check; any other recipe
    /// expands to its dependencies and the `just <recipe>` lines of its
    /// body. A command line no check runs is returned as is, so it fails
    /// the test.
    fn just_check_leaves(justfile: &str, checks: &ChecksFile) -> Vec<String> {
        // The Unix recipe of each name: the first definition not marked
        // `[windows]`.
        let mut recipes: BTreeMap<&str, (Vec<&str>, Vec<&str>)> = BTreeMap::new();
        let mut windows = false;
        let mut current: Option<&str> = None;
        for line in justfile.lines() {
            if let Some(body) = line.strip_prefix("    ") {
                if let Some(recipe) = current.and_then(|name| recipes.get_mut(name)) {
                    recipe.1.push(body.trim());
                }
                continue;
            }
            current = None;
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                windows |= trimmed == "[windows]";
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.contains(":=") {
                windows = false;
                continue;
            }
            let Some((head, deps)) = line.split_once(':') else {
                continue;
            };
            let name = head.split_whitespace().next().unwrap_or_default();
            if !windows && !recipes.contains_key(name) {
                recipes.insert(name, (deps.split_whitespace().collect(), Vec::new()));
                current = Some(name);
            }
            windows = false;
        }
        let check_of_line = |line: &str| {
            let words: Vec<&str> = line
                .split_whitespace()
                .filter(|word| *word != "{{slot}}")
                .map(|word| {
                    if word == "{{python}}" {
                        "python3"
                    } else {
                        word
                    }
                })
                .collect();
            checks
                .checks
                .iter()
                .find(|(_, argv)| {
                    !argv.is_empty()
                        && argv.len() <= words.len()
                        && argv.iter().zip(&words).all(|(arg, word)| arg == word)
                })
                .map(|(name, _)| name.clone())
        };
        let mut leaves = Vec::new();
        let mut pending = vec!["check".to_owned()];
        let mut seen = Vec::new();
        while let Some(name) = pending.pop() {
            // `guard` checks the free disk, which preflight does too.
            if name == "guard" || seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            if let Some(check) = check_of_line(&format!("just {name}")) {
                leaves.push(check);
                continue;
            }
            let (deps, body) = recipes
                .get(name.as_str())
                .unwrap_or_else(|| panic!("no recipe {name}"));
            pending.extend(deps.iter().map(|dep| (*dep).to_owned()));
            for line in body {
                if line.starts_with('@') {
                    continue;
                } else if let Some(call) = line.strip_prefix("just ") {
                    pending.push(
                        call.split_whitespace()
                            .next()
                            .unwrap_or_default()
                            .to_owned(),
                    );
                } else {
                    leaves.push(check_of_line(line).unwrap_or_else(|| (*line).to_owned()));
                }
            }
        }
        leaves.sort();
        leaves.dedup();
        leaves
    }

    /// The verify adds by path a registered check for every recipe of the
    /// install's `just check`, and nothing else, so the two cannot diverge:
    /// a recipe added to `just check` fails here until it is registered in
    /// `.herdr/checks.toml` and given paths in [`PATH_CHECKS`].
    #[test]
    fn just_check_runs_only_recipes_the_verify_adds_by_path() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let checks = read_checks(root).unwrap();
        let justfile = std::fs::read_to_string(root.join("justfile")).unwrap();
        let mut covered: Vec<&str> = PATH_CHECKS.iter().map(|check| check.name).collect();
        covered.sort_unstable();
        assert_eq!(just_check_leaves(&justfile, &checks), covered);
    }

    #[test]
    fn just_check_leaves_expand_dependencies_and_just_lines() {
        let checks: ChecksFile = toml::from_str(
            "[checks]\nlint = [\"just\", \"lint\"]\nmaint = [\"just\", \"maint-test\"]\n\
             tests = [\"cargo\", \"nextest\", \"run\"]\n\
             win = [\"python3\", \"scripts/win.py\", \"lint\"]\n",
        )
        .unwrap();
        let justfile = "x := \"y\"\n\n# Check.\n[unix]\ncheck: guard ci win\n    \
                        just maint-test\n    @echo done\n\n[windows]\ncheck:\n    \
                        windows-only\n\nci: lint\n    \
                        {{slot}} cargo nextest run --locked\n    just other\n\n\
                        win:\n    {{slot}} {{python}} scripts/win.py lint\n\n\
                        other:\n    stray command\n";
        assert_eq!(
            just_check_leaves(justfile, &checks),
            ["lint", "maint", "stray command", "tests", "win"]
        );
    }

    #[test]
    fn path_checks_are_the_registered_ones_the_run_does_not_name() {
        let checks: ChecksFile = toml::from_str(
            "[checks]\nok = [\"true\"]\ntests = [\"cargo\", \"nextest\", \"run\"]\n\
             maintenance = [\"just\", \"maintenance-test\"]\n",
        )
        .unwrap();
        let named = |names: &[&str]| {
            registered_checks(
                &checks,
                &names
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let names = |found: Vec<RunCheck>| -> Vec<String> {
            found.into_iter().map(|check| check.name).collect()
        };
        assert_eq!(
            names(path_checks_of(&checks, &named(&["ok"]))),
            ["tests", "maintenance"]
        );
        assert_eq!(
            names(path_checks_of(&checks, &named(&["ok", "maintenance"]))),
            ["tests"]
        );
        let found = path_checks_of(&checks, &named(&["tests"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].argv, ["just", "maintenance-test"]);
        // An unregistered path check is not added.
        let bare: ChecksFile = toml::from_str("[checks]\nok = [\"true\"]\n").unwrap();
        assert!(path_checks_of(&bare, &named(&["ok"])).is_empty());
    }

    #[test]
    fn a_run_stored_by_an_older_build_keeps_its_tests_check() {
        let check = |name: &str| RunCheck {
            name: name.to_owned(),
            argv: vec!["true".to_owned()],
        };
        let old: RunFinish =
            serde_json::from_value(json!({"contract_check": {"name": "tests", "argv": ["true"]}}))
                .unwrap();
        assert_eq!(old.path_checks(), [check("tests")]);
        let new = RunFinish {
            path_checks: vec![check("tests"), check("maintenance")],
            ..RunFinish::default()
        };
        assert_eq!(new.path_checks(), [check("tests"), check("maintenance")]);
        let written = serde_json::to_value(&new).unwrap();
        assert!(written.get("contract_check").is_none(), "{written}");
    }

    /// The repository's own checks file registers every check the verify
    /// adds by path, so they are added to its runs.
    #[test]
    fn the_repository_registers_every_path_check() {
        let checks = read_checks(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        for path_check in &PATH_CHECKS {
            assert!(
                checks.checks.contains_key(path_check.name),
                "{} is not in {CHECKS_FILE}",
                path_check.name
            );
        }
        assert_eq!(checks.checks["maintenance"], ["just", "maintenance-test"]);
        assert_eq!(checks.checks["lint"], ["just", "lint"]);
    }

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
