//! What the todo runs in progress are doing now, for clients: the step each
//! run's driver is at, since when, and the command it runs there with its
//! log. The driver publishes these from its own step transitions (each run
//! write, and each step it enters); nothing reads logs or judges how long a
//! step takes. Also who waits on a run through `todo.wait` right now: a
//! wait edge exists exactly while that request blocks.
//!
//! Kept in memory: a driver after a restart publishes the step again when
//! it enters it, and a wait ends with its request.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::Value;

use super::{Run, CHANGES};
use crate::api::schema::{TodoRunInfo, TodoRunStatus, TodoStep};
use crate::workers::{lock, now_ms, WorkerSupervisor};

/// What a driver of this server publishes of a run's current step.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StepActivity {
    step: TodoStep,
    started_ms: u64,
    /// The command the step runs, while it runs.
    command: Option<String>,
    log: Option<String>,
}

static STEPS: Mutex<BTreeMap<String, StepActivity>> = Mutex::new(BTreeMap::new());

/// A run as clients list it under its coordinator's space: in progress, or
/// done with a stop reason the user has not acknowledged yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveRun {
    pub(crate) run_id: String,
    pub(crate) repo: String,
    pub(crate) item: String,
    pub(crate) step: TodoStep,
    pub(crate) status: TodoRunStatus,
    pub(crate) attempt: u32,
    /// The owner pane's workspace and the owner pane.
    pub(crate) workspace_id: Option<String>,
    pub(crate) owner_pane: Option<String>,
    pub(crate) step_started_ms: Option<u64>,
    pub(crate) step_command: Option<String>,
    pub(crate) step_log: Option<String>,
    /// Why the done run's close started no next item, until the user
    /// acknowledges it (`todo.ack_stop`).
    pub(crate) stop_reason: Option<String>,
}

/// The command a run event says the step starts, as one line, with its
/// log: the install's and the prepare's argv, the verify's checks.
fn command_of(event: &Value) -> Option<(String, Option<String>)> {
    let argv = |value: &Value| {
        value.as_array().map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
    };
    let log = event.get("log").and_then(Value::as_str).map(str::to_owned);
    match event.get("type").and_then(Value::as_str)? {
        "run_install_intent" => Some((argv(event.get("command")?)?, log)),
        "run_prepare_started" => Some((argv(event.get("grant")?.get("argv")?)?, log)),
        "run_verify_intent" => {
            let checks = argv(event.get("checks")?)?.replace(' ', ", ");
            Some((format!("worker verify: {checks}"), None))
        }
        _ => None,
    }
}

/// Records the step the run is at after a write (`event`, recorded at
/// `at_ms`): a new step starts its time and drops the previous step's
/// command and log; a step that stopped running (the run waits, or ended)
/// drops its command and keeps its log. An ended run leaves. Returns
/// whether anything a client shows changed.
pub(super) fn observe(info: &TodoRunInfo, event: Option<&Value>, at_ms: u64) -> bool {
    let mut steps = lock(&STEPS);
    if matches!(
        info.status,
        TodoRunStatus::Done | TodoRunStatus::Blocked | TodoRunStatus::Aborted
    ) {
        return steps.remove(&info.run_id).is_some();
    }
    let before = steps.get(&info.run_id).cloned();
    let entry = steps
        .entry(info.run_id.clone())
        .or_insert_with(|| StepActivity {
            step: info.step,
            started_ms: at_ms,
            command: None,
            log: None,
        });
    if entry.step != info.step {
        *entry = StepActivity {
            step: info.step,
            started_ms: at_ms,
            command: None,
            log: None,
        };
    }
    if let Some((command, log)) = event.and_then(command_of) {
        entry.command = Some(command);
        entry.log = log;
    }
    if info.status != TodoRunStatus::Running {
        entry.command = None;
    }
    before.as_ref() != Some(entry)
}

/// The run as clients see it, with what its driver publishes of its step.
pub(super) fn shown(mut info: TodoRunInfo) -> TodoRunInfo {
    if let Some(step) = lock(&STEPS)
        .get(&info.run_id)
        .filter(|step| step.step == info.step)
    {
        info.step_started_ms = Some(step.started_ms);
        info.step_command = step.command.clone();
        info.step_log = step.log.clone();
    }
    info
}

/// The runs listed last, and the change generation they were
/// read at: a client snapshot reads the store again only after a run's
/// write announced a change.
static LISTED: Mutex<Option<(u64, Vec<ActiveRun>)>> = Mutex::new(None);

fn active_run(run: Run) -> ActiveRun {
    let stop_reason = run
        .stop_shown()
        .then(|| run.finish.stop_reason.clone())
        .flatten();
    let info = shown(super::decision::shown(run.info));
    ActiveRun {
        run_id: info.run_id,
        repo: info.repo,
        item: info.item,
        step: info.step,
        status: info.status,
        attempt: info.attempt,
        workspace_id: run.workspace,
        owner_pane: run.owner_pane,
        step_started_ms: info.step_started_ms,
        step_command: info.step_command,
        step_log: info.step_log,
        stop_reason,
    }
}

impl WorkerSupervisor {
    /// The runs in progress and the stopped runs not acknowledged yet,
    /// oldest first, read from the store once per announced change.
    pub(crate) fn active_runs(&self) -> Vec<ActiveRun> {
        let generation = *lock(&CHANGES.generation);
        if let Some((seen, runs)) = lock(&LISTED).as_ref() {
            if *seen == generation {
                return runs.clone();
            }
        }
        let runs = match self
            .run_store()
            .and_then(|store| store.runs_shown().map_err(super::store_error))
        {
            Ok(runs) => runs
                .into_iter()
                .map(active_run)
                // A run whose decision step fails shows blocked: not in
                // progress for a client.
                .filter(|run| run.status != TodoRunStatus::Blocked)
                .collect::<Vec<_>>(),
            Err(error) => {
                // Said once per change, not once per snapshot.
                tracing::warn!(%error, "cannot list the todo runs in progress");
                Vec::new()
            }
        };
        *lock(&LISTED) = Some((generation, runs.clone()));
        runs
    }
}

/// A `todo.wait` that blocks now: the pane it was sent from waits on the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunWait {
    pub(crate) waiter_pane: String,
    pub(crate) run_id: String,
    pub(crate) since_ms: u64,
}

static WAITS: Mutex<Vec<(u64, RunWait)>> = Mutex::new(Vec::new());
static NEXT_WAIT: Mutex<u64> = Mutex::new(0);

/// A wait edge while it lives: dropping it (the wait returned, failed or
/// its caller went away) removes it.
pub(crate) struct WaitEdge(u64);

impl WaitEdge {
    /// Records that `waiter_pane` waits on `run_id` from now on.
    pub(crate) fn begin(waiter_pane: String, run_id: String) -> Self {
        let id = {
            let mut next = lock(&NEXT_WAIT);
            *next += 1;
            *next
        };
        lock(&WAITS).push((
            id,
            RunWait {
                waiter_pane,
                run_id,
                since_ms: now_ms(),
            },
        ));
        super::announce();
        Self(id)
    }
}

impl Drop for WaitEdge {
    fn drop(&mut self) {
        lock(&WAITS).retain(|(id, _)| *id != self.0);
        super::announce();
    }
}

/// The `todo.wait`s that block now, oldest first.
pub(crate) fn run_waits() -> Vec<RunWait> {
    lock(&WAITS).iter().map(|(_, wait)| wait.clone()).collect()
}

#[cfg(test)]
pub(super) fn forget_for_test(run_id: &str) {
    lock(&STEPS).remove(run_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(run_id: &str, step: TodoStep, status: TodoRunStatus) -> TodoRunInfo {
        TodoRunInfo {
            run_id: run_id.into(),
            repo: "/repo".into(),
            item: "t-aaaaaaaa".into(),
            step,
            status,
            attempt: 1,
            base: None,
            target_branch: None,
            landing_worktree: None,
            push: None,
            worker_id: None,
            branch: None,
            task: String::new(),
            message: String::new(),
            paths: Vec::new(),
            checks: Vec::new(),
            last_acked_seq: None,
            pending_event: None,
            error: None,
            picked: None,
            installed_build: None,
            todo_commit: None,
            pushed: None,
            kept_branches: Vec::new(),
            next_item: None,
            next_run_id: None,
            next_refusal: None,
            stop_reason: None,
            auto_review: false,
            auto_answer: false,
            new_api: false,
            drafted: false,
            queued: false,
            grants: Vec::new(),
            created_ms: 0,
            updated_ms: 0,
            step_started_ms: None,
            step_command: None,
            step_log: None,
            agent: None,
            model: None,
        }
    }

    #[test]
    fn a_step_publishes_its_start_its_command_and_its_log() {
        let run = "r-activity1";
        forget_for_test(run);
        let install = info(run, TodoStep::Install, TodoRunStatus::Running);
        assert!(observe(&install, None, 1_000));
        let intent = json!({
            "type": "run_install_intent",
            "command": ["just", "clean-install", "TODO.md"],
            "log": "/state/run-r-activity1-install.log",
        });
        assert!(observe(&install, Some(&intent), 2_000));
        let shown = shown(install.clone());
        assert_eq!(
            shown.step_started_ms,
            Some(1_000),
            "the step started when entered"
        );
        assert_eq!(
            shown.step_command.as_deref(),
            Some("just clean-install TODO.md")
        );
        assert_eq!(
            shown.step_log.as_deref(),
            Some("/state/run-r-activity1-install.log")
        );
        // Nothing new: nothing to announce.
        assert!(!observe(&install, Some(&json!({"type": "other"})), 3_000));

        // The install failed: the run waits, the command no longer runs,
        // its log stays named.
        let failed = info(run, TodoStep::Install, TodoRunStatus::Waiting);
        assert!(observe(&failed, None, 4_000));
        let shown_failed = super::shown(failed);
        assert_eq!(shown_failed.step_command, None);
        assert_eq!(shown_failed.step_started_ms, Some(1_000));
        assert!(shown_failed.step_log.is_some());

        // The next step starts its own time and drops the log.
        let todo = info(run, TodoStep::Todo, TodoRunStatus::Running);
        assert!(observe(&todo, None, 5_000));
        let shown_todo = super::shown(todo);
        assert_eq!(shown_todo.step_started_ms, Some(5_000));
        assert_eq!(shown_todo.step_log, None);

        // An ended run leaves.
        assert!(observe(
            &info(run, TodoStep::Done, TodoRunStatus::Done),
            None,
            6_000
        ));
        assert_eq!(
            super::shown(info(run, TodoStep::Done, TodoRunStatus::Done)).step_started_ms,
            None
        );
    }

    #[test]
    fn the_verify_names_its_checks_and_the_prepare_its_argv() {
        let verify =
            json!({"type": "run_verify_intent", "worker_id": "w1", "checks": ["workers", "lint"]});
        assert_eq!(
            command_of(&verify),
            Some(("worker verify: workers, lint".to_owned(), None))
        );
        let prepare = json!({
            "type": "run_prepare_started",
            "grant": {"argv": ["npm", "ci"]},
            "log": "/state/prepare/r-x.log",
        });
        assert_eq!(
            command_of(&prepare),
            Some((
                "npm ci".to_owned(),
                Some("/state/prepare/r-x.log".to_owned())
            ))
        );
    }

    #[test]
    fn a_wait_edge_lives_while_its_request_blocks() {
        let edge = WaitEdge::begin("w1-3".into(), "r-waited01".into());
        assert!(run_waits()
            .iter()
            .any(|wait| wait.waiter_pane == "w1-3" && wait.run_id == "r-waited01"));
        drop(edge);
        assert!(!run_waits().iter().any(|wait| wait.run_id == "r-waited01"));
    }
}
