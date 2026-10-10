//! Queue mode (`herdr todo queue on|pause|status`): herdr's server drives a
//! repository's TODO queue itself. While the mode is on, no run of the
//! repository is active (an escalation pending is one that waits), no
//! headless item coordinator runs and the usage gate admits, the server
//! starts the top item of "Next, in order" that is not blocked as
//! `todo.draft_run` would: the task drafted, reviewed and answered by typed
//! decision calls ([`super::auto_draft`], [`super::auto_review`],
//! [`super::auto_answer`]). A queue run whose own review approves it closes
//! the item, so the queue moves on.
//!
//! It looks again only on herdr's own events, never on a timer: a run of the
//! repository ended (done, blocked or aborted), an escalation was answered,
//! the server started, the mode was set. `TODO.md` is read from `master` at
//! each of them (git's own state, not a watched file), and the items by
//! their stable ids.
//!
//! Guarantees:
//!
//! - one active run per repository: the runs' unique index, and a fencing
//!   token every start of the queue claims and moves on in the transaction
//!   that creates the run ([`super::super::store::Tx::queue_claim`]), so an
//!   evaluation that read the queue before another start (another thread, a
//!   second server of a live handoff) starts nothing;
//! - a server that starts settles the queue runs that ended while no server
//!   looked (an interrupted run is driven again by [`WorkerSupervisor::resume_runs`]
//!   first) before it starts another;
//! - an item the queue ran [`ITEM_ATTEMPTS`] times with the same text is
//!   blocked, with the reason shown, until its text changes;
//! - [`BREAKER`] consecutive queue runs that ended blocked or aborted pause
//!   the queue with the reason and a notification;
//! - the cost is bounded by the usage gate, read at every start.
//!
//! The checks, install and push of the queue's runs use the environment the
//! last `queue on` sent, kept in memory only (as a run's): after a restart a
//! run's check is `unavailable` until a `todo resume` sends one again.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Mutex;

use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::warn;

use super::{git, lock, step_name, store_error, todo_titles, Run, TODO_FILE};
use crate::api::schema::{
    TodoQueueBlockedItem, TodoQueueInfo, TodoQueueMode, TodoQueueSetParams, TodoQueueStatus,
    TodoRunParams, TodoRunStatus,
};
use crate::workers::store::{QueueRun, RunOwner, StoredQueue};
use crate::workers::{now_ms, repository_of, WorkerError, WorkerSupervisor};

/// The runs the queue starts of one item with the same text; then the item
/// is blocked until its text changes.
pub(crate) const ITEM_ATTEMPTS: usize = 2;
/// Consecutive queue runs that ended blocked or aborted after which the
/// queue pauses itself.
pub(crate) const BREAKER: u32 = 3;
/// The prefix a queue's environment carries in a live handoff's payload,
/// beside the runs' (whose keys are run ids, `r-...`).
#[cfg(unix)]
const ENV_PREFIX: &str = "queue:";

/// The environment the last `queue on` of each repository sent, which the
/// queue's runs get. Never stored: it may hold credentials.
static QUEUE_ENV: Mutex<BTreeMap<String, HashMap<String, String>>> = Mutex::new(BTreeMap::new());

/// The queues' environments for a live handoff's payload, keyed
/// `queue:<repo>`.
#[cfg(unix)]
pub(super) fn envs_for_handoff() -> BTreeMap<String, HashMap<String, String>> {
    lock(&QUEUE_ENV)
        .iter()
        .map(|(repo, env)| (format!("{ENV_PREFIX}{repo}"), env.clone()))
        .collect()
}

/// Takes a queue's environment out of a live handoff's payload entry;
/// gives back an entry that is not a queue's.
#[cfg(unix)]
pub(super) fn restore_handed_off_env(
    key: String,
    env: HashMap<String, String>,
) -> Option<(String, HashMap<String, String>)> {
    match key.strip_prefix(ENV_PREFIX) {
        Some(repo) => {
            lock(&QUEUE_ENV).entry(repo.to_owned()).or_insert(env);
            None
        }
        None => Some((key, env)),
    }
}

/// The repositories whose queue this process evaluates, each with whether
/// another event asked for an evaluation meanwhile: the evaluator then
/// looks once more before it lets go, so no event is lost.
static EVALUATING: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());

struct Evaluating {
    repo: String,
    released: bool,
}

impl Evaluating {
    fn claim(repo: &str) -> Option<Self> {
        let mut evaluating = lock(&EVALUATING);
        match evaluating.get_mut(repo) {
            Some(again) => {
                *again = true;
                None
            }
            None => {
                evaluating.insert(repo.to_owned(), false);
                Some(Self {
                    repo: repo.to_owned(),
                    released: false,
                })
            }
        }
    }

    /// Lets go, unless another event asked meanwhile: then `false`.
    fn release(&mut self) -> bool {
        let mut evaluating = lock(&EVALUATING);
        match evaluating.get_mut(&self.repo) {
            Some(again) if *again => {
                *again = false;
                false
            }
            _ => {
                evaluating.remove(&self.repo);
                self.released = true;
                true
            }
        }
    }
}

impl Drop for Evaluating {
    fn drop(&mut self) {
        if !self.released {
            lock(&EVALUATING).remove(&self.repo);
        }
    }
}

/// Test only: a run of this repository that ends does not tell its queue,
/// as a server that ended right after recording the end would not.
#[cfg(test)]
static SKIP_EVENT: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(all(test, unix))]
pub(crate) fn skip_next_event(repo: &str) {
    lock(&SKIP_EVENT).push(repo.to_owned());
}

#[cfg(test)]
fn skips_event(repo: &str) -> bool {
    let mut skips = lock(&SKIP_EVENT);
    match skips.iter().position(|at| at == repo) {
        Some(index) => {
            skips.remove(index);
            true
        }
        None => false,
    }
}

/// What a start of the queue claims: the fencing token it read and the
/// item with the digest of its text.
pub(super) struct QueueStart {
    pub(super) token: i64,
    pub(super) item_digest: String,
}

impl QueueStart {
    /// The claim's record of the run `run_id` of `item`.
    pub(super) fn run(&self, run_id: &str, item: &str) -> QueueRun {
        QueueRun {
            run_id: run_id.to_owned(),
            item: item.to_owned(),
            item_digest: self.item_digest.clone(),
            token: self.token + 1,
            started_ms: now_ms(),
            outcome: None,
            error: None,
        }
    }
}

/// What one evaluation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueueStep {
    /// It started this run.
    Started(String),
    /// It started nothing: the mode is paused, a run is active, another
    /// start claimed the token first, or it recorded why (empty, blocked,
    /// usage gate).
    Idle,
}

/// The digest of an item's text, which tells an edited item from the one
/// the queue blocked.
fn item_digest(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `master`'s `TODO.md`: git's own state, not the checkout's file.
fn todo_on_master(repo: &str) -> Result<String, String> {
    git(Path::new(repo), &["show", &format!("master:{TODO_FILE}")])
        .map_err(|error| format!("cannot read {TODO_FILE} on master: {error}"))
}

fn mode_of(queue: Option<&StoredQueue>) -> TodoQueueMode {
    match queue.map(|queue| queue.mode.as_str()) {
        Some("on") => TodoQueueMode::On,
        _ => TodoQueueMode::Paused,
    }
}

fn status_name(status: TodoQueueStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn parse_status(status: Option<&str>) -> Option<TodoQueueStatus> {
    serde_json::from_value(json!(status?)).ok()
}

/// The items of "Next, in order" the queue skips, each with why: the queue
/// ran it [`ITEM_ATTEMPTS`] times with its current text.
fn blocked_items(todo: &str, runs: &[QueueRun]) -> Vec<TodoQueueBlockedItem> {
    todo_titles::next_items(todo)
        .into_iter()
        .filter_map(|item| {
            let text = todo_titles::item_text(todo, &item).unwrap_or_default();
            blocked_reason(&item, &item_digest(&text), runs)
                .map(|reason| TodoQueueBlockedItem { item, reason })
        })
        .collect()
}

/// Why the queue no longer starts `item` with the text whose digest is
/// `digest`, when it does not.
fn blocked_reason(item: &str, digest: &str, runs: &[QueueRun]) -> Option<String> {
    let tried: Vec<&QueueRun> = runs
        .iter()
        .filter(|run| run.item == item && run.item_digest == digest && run.outcome.is_some())
        .collect();
    let last = tried.last()?;
    (tried.len() >= ITEM_ATTEMPTS).then(|| {
        format!(
            "the queue ran {item} {} times with its current text; the last run {} ended {}{}; \
             edit the item in TODO.md (or move it) to let the queue run it again",
            tried.len(),
            last.run_id,
            last.outcome.as_deref().unwrap_or("?"),
            last.error
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        )
    })
}

impl WorkerSupervisor {
    /// `todo.queue_set`: turns the repository's queue on (clearing a pause
    /// and the circuit breaker's count) and evaluates it at once, or pauses
    /// it; a pause lets the run in progress finish.
    pub(crate) fn todo_queue_set(
        &self,
        params: TodoQueueSetParams,
    ) -> Result<TodoQueueInfo, WorkerError> {
        let repo = self.queue_repo(&params.cwd)?;
        let store = self.run_store()?;
        let at = now_ms();
        match params.mode {
            TodoQueueMode::On => {
                let owner = RunOwner {
                    pane_id: params.owner_pane_id.as_deref(),
                    session_id: params.owner_session_id.as_deref(),
                    workspace: params.workspace_id.as_deref(),
                    coordinator_id: None,
                };
                store
                    .transaction(|tx| tx.queue_on(&repo, &owner, at))
                    .map_err(store_error)?;
                match params.env {
                    Some(env) => lock(&QUEUE_ENV).insert(repo.clone(), env),
                    None => lock(&QUEUE_ENV).remove(&repo),
                };
                super::announce();
                self.queue_event(&repo);
            }
            TodoQueueMode::Paused => {
                let reason = params
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("paused by the user (`herdr todo queue pause`)")
                    .to_owned();
                store
                    .transaction(|tx| tx.queue_paused(&repo, &reason, at))
                    .map_err(store_error)?;
                super::announce();
            }
            TodoQueueMode::Unknown => {
                return Err(WorkerError::Invalid(
                    "queue mode is `on` or `paused`".into(),
                ))
            }
        }
        self.todo_queue_status(&repo)
    }

    /// `todo.queue_status`: the queue's mode, and its status from the run
    /// in progress when there is one, else from the mode and the last
    /// evaluation.
    pub(crate) fn todo_queue_status(&self, cwd: &str) -> Result<TodoQueueInfo, WorkerError> {
        let repo = self.queue_repo(cwd)?;
        let store = self.run_store()?;
        let queue = store.queue(&repo).map_err(store_error)?;
        let mode = mode_of(queue.as_ref());
        let runs = store.queue_runs(&repo).map_err(store_error)?;
        let blocked = todo_on_master(&repo)
            .map(|todo| blocked_items(&todo, &runs))
            .unwrap_or_default();
        let active = store.active_run(&repo).map_err(store_error)?;
        let (status, reason) = match (&active, &queue) {
            (Some(run), _) => self.status_of_run(run),
            (None, None) => (
                TodoQueueStatus::WaitingOnUser,
                "queue mode was never turned on for this repository (`herdr todo queue on`)"
                    .to_owned(),
            ),
            (None, Some(queue)) if mode == TodoQueueMode::Paused => (
                TodoQueueStatus::WaitingOnUser,
                format!(
                    "the queue is paused: {}",
                    queue
                        .pause_reason
                        .as_deref()
                        .unwrap_or("no reason recorded")
                ),
            ),
            (None, Some(queue)) => (
                parse_status(queue.status.as_deref()).unwrap_or(TodoQueueStatus::Unknown),
                queue.reason.clone().unwrap_or_default(),
            ),
        };
        Ok(TodoQueueInfo {
            repo: repo.clone(),
            mode,
            status,
            reason,
            run_id: active.as_ref().map(|run| run.info.run_id.clone()),
            item: active.as_ref().map(|run| run.info.item.clone()),
            pause_reason: queue.as_ref().and_then(|queue| queue.pause_reason.clone()),
            failures: queue.as_ref().map_or(0, |queue| queue.failures),
            blocked_items: blocked,
            owner_pane_id: queue.as_ref().and_then(|queue| queue.owner_pane.clone()),
            updated_ms: queue.as_ref().map_or(0, |queue| queue.updated_ms),
        })
    }

    fn queue_repo(&self, cwd: &str) -> Result<String, WorkerError> {
        let dir = Path::new(cwd);
        if !dir.is_absolute() {
            return Err(WorkerError::Invalid(format!("cwd must be absolute: {cwd}")));
        }
        repository_of(dir)
            .ok_or_else(|| WorkerError::Preflight(format!("{cwd} is not in a git repository")))
    }

    /// The queue's status while `run` is the repository's run in progress.
    fn status_of_run(&self, run: &Run) -> (TodoQueueStatus, String) {
        let info = &run.info;
        let at = format!(
            "run {} of {} at its {} step",
            info.run_id,
            info.item,
            step_name(info.step)
        );
        if info.status != TodoRunStatus::Waiting
            || self.auto_review_due(run).is_some()
            || self.auto_answer_due(run).is_some()
        {
            return (TodoQueueStatus::Running, at);
        }
        let latest = self
            .run_store()
            .ok()
            .and_then(|store| store.latest_run_event(&info.run_id).ok().flatten())
            .filter(|(seq, _)| info.pending_event == Some(*seq));
        let kind = latest
            .as_ref()
            .and_then(|(_, body)| body["kind"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "pending".to_owned());
        match latest {
            Some((_, body)) if body[super::auto_review::ESCALATES].is_string() => (
                TodoQueueStatus::EscalationPending,
                format!(
                    "{at} waits on the user's answer to its escalated {kind} event (the `?` list)"
                ),
            ),
            _ => (
                TodoQueueStatus::WaitingOnUser,
                format!("{at} waits on its {kind} event (`herdr todo resume`)"),
            ),
        }
    }

    /// Whether the repository's queue is on: a close then names no next
    /// item, and a queue run's approval closes its item.
    pub(super) fn queue_is_on(&self, repo: &str) -> bool {
        self.run_store()
            .ok()
            .and_then(|store| store.queue(repo).ok().flatten())
            .is_some_and(|queue| queue.mode == "on")
    }

    /// One of herdr's own events for the repository's queue (a run ended,
    /// an escalation was answered, the mode was set, the server started):
    /// evaluates it, once more when another event came meanwhile.
    pub(crate) fn queue_event(&self, repo: &str) {
        if self.handed_off() {
            return;
        }
        let Some(mut evaluating) = Evaluating::claim(repo) else {
            return;
        };
        loop {
            if let Err(error) = self.queue_step(repo) {
                warn!(repo, %error, "the todo queue's evaluation failed");
            }
            if evaluating.release() {
                return;
            }
        }
    }

    /// A run of `repo` ended: its queue looks again.
    pub(super) fn queue_after_run(&self, repo: &str) {
        #[cfg(test)]
        if skips_event(repo) {
            return;
        }
        self.queue_event(repo);
    }

    /// One evaluation: settles the queue runs that ended, then, when the
    /// mode is on and nothing of the repository is active, starts the top
    /// item of "Next, in order" that is not blocked, or records why not.
    pub(crate) fn queue_step(&self, repo: &str) -> Result<QueueStep, WorkerError> {
        self.settle_queue(repo)?;
        let store = self.run_store()?;
        let Some(queue) = store.queue(repo).map_err(store_error)? else {
            return Ok(QueueStep::Idle);
        };
        if queue.mode != "on" {
            return Ok(QueueStep::Idle);
        }
        // A run at its abort step no longer claims the repository, but it
        // has not ended: its end, settled first, is the next event.
        let unended = store
            .runs(Some(repo))
            .map_err(store_error)?
            .into_iter()
            .any(|run| {
                matches!(
                    run.info.status,
                    TodoRunStatus::Running | TodoRunStatus::Waiting
                )
            });
        if unended {
            return Ok(QueueStep::Idle);
        }
        if let Some(tenure) = store
            .active_coordinator_of(repo)
            .map_err(store_error)?
            .filter(|tenure| tenure.headless)
        {
            let reason = format!(
                "headless item coordinator {} of {} is active",
                tenure.id,
                tenure.item.as_deref().unwrap_or("an item")
            );
            return self.queue_record(repo, TodoQueueStatus::Running, &reason);
        }
        let todo = match todo_on_master(repo) {
            Ok(todo) => todo,
            Err(why) => return self.queue_record(repo, TodoQueueStatus::Blocked, &why),
        };
        let items = todo_titles::next_items(&todo);
        if items.is_empty() {
            let reason = format!(
                "\"{}\" in {TODO_FILE} on master has no open item",
                todo_titles::NEXT_SECTION
            );
            return self.queue_record(repo, TodoQueueStatus::Empty, &reason);
        }
        let runs = store.queue_runs(repo).map_err(store_error)?;
        let mut blocked = Vec::new();
        let mut chosen = None;
        for item in items {
            let text = todo_titles::item_text(&todo, &item).unwrap_or_default();
            let digest = item_digest(&text);
            match blocked_reason(&item, &digest, &runs) {
                Some(reason) => blocked.push(reason),
                None => {
                    chosen = Some((item, digest));
                    break;
                }
            }
        }
        let Some((item, item_digest)) = chosen else {
            let reason = format!(
                "every item of \"{}\" is blocked: {}",
                todo_titles::NEXT_SECTION,
                blocked.join("; ")
            );
            return self.queue_record(repo, TodoQueueStatus::Blocked, &reason);
        };
        let start = QueueStart {
            token: queue.token,
            item_digest,
        };
        let params = TodoRunParams {
            cwd: repo.to_owned(),
            item: item.clone(),
            task: String::new(),
            message: String::new(),
            paths: Vec::new(),
            checks: Vec::new(),
            owner_pane_id: queue.owner_pane.clone(),
            owner_session_id: queue.owner_session.clone(),
            workspace_id: queue.workspace.clone(),
            env: lock(&QUEUE_ENV).get(repo).cloned(),
            ignore_usage: false,
            auto_review: true,
            auto_answer: true,
        };
        match self.create_run(params, true, Some(&start)) {
            Ok(run) => Ok(QueueStep::Started(run.run_id)),
            // Another start claimed the token, or a run started meanwhile:
            // its end is the next event.
            Err(WorkerError::RunActive(_)) => Ok(QueueStep::Idle),
            Err(WorkerError::UsageGate(why)) => {
                self.queue_record(repo, TodoQueueStatus::UsageGate, &why)
            }
            Err(refused) => {
                let reason = format!("the start of {item} was refused: {refused}");
                self.queue_record(repo, TodoQueueStatus::Blocked, &reason)
            }
        }
    }

    fn queue_record(
        &self,
        repo: &str,
        status: TodoQueueStatus,
        reason: &str,
    ) -> Result<QueueStep, WorkerError> {
        let name = status_name(status);
        self.run_store()?
            .transaction(|tx| tx.queue_evaluated(repo, &name, reason, now_ms()))
            .map_err(store_error)?;
        super::announce();
        Ok(QueueStep::Idle)
    }

    /// Settles each queue run of `repo` that ended since the last look,
    /// once: its outcome, and the circuit breaker's count (a done run resets
    /// it, a blocked or aborted one adds one); at [`BREAKER`] the queue
    /// pauses with the reason. The user is told about a pause and about an
    /// item the queue now blocks.
    fn settle_queue(&self, repo: &str) -> Result<(), WorkerError> {
        let store = self.run_store()?;
        let at = now_ms();
        let settled = store
            .transaction(|tx| {
                let Some(queue) = tx.queue(repo)? else {
                    return Ok(None);
                };
                let ended = tx.queue_unsettled(repo)?;
                if ended.is_empty() {
                    return Ok(None);
                }
                let mut failures = queue.failures;
                let mut last_failure = None;
                for (run, status, error) in &ended {
                    if status == "done" {
                        failures = 0;
                    } else {
                        failures += 1;
                        last_failure = Some((run.clone(), status.clone(), error.clone()));
                    }
                    tx.queue_settled(repo, &run.run_id, status, error.as_deref(), failures, at)?;
                }
                let paused = match last_failure {
                    Some((run, status, error)) if queue.mode == "on" && failures >= BREAKER => {
                        let reason = format!(
                            "circuit breaker: {failures} consecutive queue runs ended blocked or \
                             aborted (the last, {} of {}, {status}{}); `herdr todo queue on` \
                             resumes it",
                            run.run_id,
                            run.item,
                            error
                                .as_deref()
                                .map(|error| format!(": {error}"))
                                .unwrap_or_default()
                        );
                        tx.queue_paused(repo, &reason, at)?;
                        Some(reason)
                    }
                    _ => None,
                };
                Ok(Some((ended, paused)))
            })
            .map_err(store_error)?;
        let Some((ended, paused)) = settled else {
            return Ok(());
        };
        super::announce();
        if let Some(reason) = paused {
            crate::workers::notify_user(crate::workers::UserNotice {
                title: format!("TODO queue of {repo} paused"),
                body: reason,
            });
        }
        let runs = store.queue_runs(repo).map_err(store_error)?;
        for (run, status, _) in ended.iter().filter(|(_, status, _)| status != "done") {
            let tried = runs
                .iter()
                .filter(|other| {
                    other.item == run.item
                        && other.item_digest == run.item_digest
                        && other.outcome.is_some()
                })
                .count();
            if tried == ITEM_ATTEMPTS {
                if let Some(reason) = blocked_reason(&run.item, &run.item_digest, &runs) {
                    crate::workers::notify_user(crate::workers::UserNotice {
                        title: format!("TODO queue: {} blocked ({status})", run.item),
                        body: reason,
                    });
                }
            }
        }
        Ok(())
    }

    /// What a server that starts does for the queues, after it drove every
    /// interrupted run again: each queue that is on settles the runs that
    /// ended while no server looked and starts its next item when nothing
    /// of its repository is active.
    pub(super) fn resume_queues(&self) {
        let queues = match self.run_store().map(|store| store.queues()) {
            Ok(Ok(queues)) => queues,
            Ok(Err(error)) => {
                warn!(%error, "cannot read the todo queues");
                return;
            }
            Err(_) => return,
        };
        for queue in queues.into_iter().filter(|queue| queue.mode == "on") {
            self.queue_event(&queue.repo);
        }
    }
}

/// The closing decision a queue run's own approval records: the item leaves
/// "Next, in order" and `DECISIONS.md` gets what landed, titled by the item.
pub(super) fn queue_close(run: &Run, commit: &str, note: Option<&str>) -> String {
    let mut text = format!(
        "Landed by herdr's TODO queue: run {}, attempt {}, `{}` ({}), approved by the server's \
         review.",
        run.info.run_id,
        run.info.attempt,
        run.info.message,
        &commit[..commit.len().min(12)]
    );
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        text.push_str(&format!("\n\nReview note: {note}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(item: &str, digest: &str, outcome: Option<&str>) -> QueueRun {
        QueueRun {
            run_id: format!("r-{item}"),
            item: item.into(),
            item_digest: digest.into(),
            token: 1,
            started_ms: 1,
            outcome: outcome.map(str::to_owned),
            error: Some("why".into()),
        }
    }

    #[test]
    fn an_item_is_blocked_after_its_attempts_with_the_same_text() {
        let runs = vec![run("t-a", "d1", Some("aborted"))];
        assert_eq!(blocked_reason("t-a", "d1", &runs), None);
        let runs = vec![
            run("t-a", "d1", Some("aborted")),
            run("t-a", "d1", Some("blocked")),
        ];
        let reason = blocked_reason("t-a", "d1", &runs).unwrap();
        assert!(
            reason.contains("2 times") && reason.contains("blocked: why"),
            "{reason}"
        );
        // An edited item, or a run still going, is not counted.
        assert_eq!(blocked_reason("t-a", "d2", &runs), None);
        let runs = vec![run("t-a", "d1", Some("aborted")), run("t-a", "d1", None)];
        assert_eq!(blocked_reason("t-a", "d1", &runs), None);
    }
}
