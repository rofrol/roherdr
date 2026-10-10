//! `herdr todo next`: a fresh headless coordinator per TODO item. Instead
//! of one long-lived coordinator session, herdr starts a headless Claude
//! worker (the item coordinator) for the top item of `TODO.md`'s "Next, in
//! order", with a prompt herdr builds: the item, what to read, the driver
//! commands it may use and its contract. It keeps the coordinator's
//! judgement (the worker's task, each review, the close) and ends; its state
//! is only in files (the repository and the worker store).
//!
//! It runs under its own coordination tenure, one without a pane (the
//! tenure records `headless` and its worker), so a repository still has one
//! coordinator at a time, and the runs it claims outside a pane are that
//! tenure's ([`WorkerSupervisor::todo_run`]). The pane that asked for it
//! owns its worker, so obligations and escalation work as for any worker;
//! its questions go to the user at once ([`QUESTION_TO_USER`]). Herdr's
//! coordinator allowlist (the `pre-tool` branch of the Claude hook script,
//! in its headless mode) is its first pre-tool check, and its sandbox may
//! reach the server's socket for `herdr todo ...`.
//!
//! When its turn ends herdr stops it, and on its exit event records the
//! outcome (its last line: `COORDINATOR-DONE`, `-ESCALATED` or `-BLOCKED`)
//! in the item's history and ends its tenure. With a chain (`todo.next`'s
//! `chain`), an item that is done starts the next item's coordinator on that
//! exit event, never on a timer, until "Next, in order" is empty, an item
//! escalates, is blocked or fails, or `todo.stop`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::warn;

use super::coordinators::{info, is_unique_violation, new_tenure_id, store_error};
use super::store::{NewHistory, NewTenure, RunOwner, StoredChain};
use super::WorkerSupervisor;
use super::{lock, notify_clients, now_ms, repository_of, todo_titles, WorkerError};
use crate::api::schema::{
    CoordinatorInfo, HistoryEventKind, TodoChainInfo, TodoNextParams, WorkerCommandTarget,
    WorkerInfo, WorkerStartParams, WorkerWaitUntil,
};

/// Why an item coordinator's question is the user's from the start: it
/// decides everything the repository answers, so what is left is the user's.
pub(super) const QUESTION_TO_USER: &str = "an item coordinator asks the user directly";
/// The last lines an item coordinator ends with.
const DONE: &str = "COORDINATOR-DONE";
const ESCALATED: &str = "COORDINATOR-ESCALATED";
const BLOCKED: &str = "COORDINATOR-BLOCKED";
/// Where herdr keeps its copy of the allowlist, in the workers directory,
/// which the coordinator's sandbox and file tools cannot write.
const ALLOWLIST_FILE: &str = "coordinator-allowlist.sh";
/// The watcher's wait has no deadline: the worker's events end it, and a
/// handoff wakes it.
const NO_DEADLINE: Duration = Duration::MAX;
const TODO_FILE: &str = "TODO.md";
/// `todo.stop`'s reason.
pub(super) const STOPPED: &str = "stopped by todo.stop";

/// What a headless item coordinator runs as, beside a worker's start.
pub(super) struct CoordinatorRole {
    pub(super) coordinator_id: String,
    /// The server's API socket, which its `herdr` commands talk to.
    pub(super) socket: PathBuf,
    /// Herdr's copy of the hook script whose `pre-tool` branch is the
    /// allowlist.
    allowlist: PathBuf,
}

impl CoordinatorRole {
    /// The worker's settings for a coordinator: its sandbox may reach the
    /// server's socket, and `NotebookEdit`, which no pre-tool check sees,
    /// is denied.
    pub(super) fn restrict_settings(&self, settings: &mut Value) {
        settings["sandbox"]["network"]["allowUnixSockets"] =
            json!([self.socket.display().to_string()]);
        if let Some(deny) = settings["permissions"]["deny"].as_array_mut() {
            deny.push(json!("NotebookEdit"));
        }
    }

    /// The allowlist as a pre-tool check: the hook script's `pre-tool`
    /// branch in its headless mode, with `temp_dir` as the coordinator's
    /// scratch directory. `env` sets what the check runner leaves out of
    /// a check's environment (every `HERDR_*`).
    pub(super) fn allowlist_check(&self, temp_dir: &Path) -> Vec<String> {
        vec![
            "env".into(),
            "HERDR_COORDINATOR_HEADLESS=1".into(),
            format!("HERDR_COORDINATOR_SCRATCH={}", temp_dir.display()),
            "sh".into(),
            self.allowlist.display().to_string(),
            "pre-tool".into(),
        ]
    }
}

/// Appended to an item coordinator's system prompt.
pub(super) fn contract(temp_dir: &Path) -> String {
    let temp = temp_dir.display();
    format!(
        "You are a headless item coordinator started by herdr for one TODO item. Nobody watches \
your output live and nothing wakes you after your turn ends: run every command, every \
`herdr todo wait` too, in the foreground; your turn's end ends you. Herdr's \
coordinator allowlist refuses every command a coordinator does not need (code goes to a \
worker through `herdr todo run`); a refused command is refused, never worked around. Write \
task, note and decision files only under your scratch directory {temp}, by that absolute \
path. A question you cannot decide from the repository goes to the user with \
AskUserQuestion: herdr shows it in the user's list and you wait for the answer; never guess."
    )
}

/// The prompt an item coordinator starts with.
fn prompt(repo: &str, item: &str, item_text: &str) -> String {
    let title = todo_titles::title_of_text(item_text).unwrap_or_else(|| item.to_owned());
    format!(
        "Item coordinator for {item}: {title}

You coordinate this one item of {repo} from its check to its close, then end. You remember \
nothing from earlier sessions: the state is in the repository and in herdr.

The item as TODO.md has it now:

{item_text}
Read first: TODO.md (this item, the items around it, \"Needs a decision\"), DECISIONS.md, \
AGENTS.md (and the instructions it points to), `git log --oneline -20`, `herdr history --item \
{item} --repo {repo}` (earlier runs and notes; none on a first claim), `herdr history \
reconcile --repo {repo}` and the checks in .herdr/checks.toml.

Then:
1. Check the item against the current code, the decisions and the other items. One that is \
done, outdated or contradictory, or that needs the user's choice or action, gets no run: move \
it to \"Needs a decision\" with its question and `Options:` (python3 scripts/todo_edit.py), \
commit TODO.md by path (`git commit -m <message> -- TODO.md`) and end with the ESCALATED line.
2. Write the worker's task to a file in your scratch directory: a title line, what to change, \
the files it may touch, how to test it. Herdr appends the commit subject, the paths, \"no body, \
no trailers\" and the WORKER-DONE line.
3. Start the run from {repo}: `herdr todo run {item} --task <file> --message \"<subject>\" \
--paths <globs> --check <names>`; pass windows-lint too for changes under src/.
4. Loop on `herdr todo wait <run-id> --after <event-id>` (no --after the first time). Review \
each event with `herdr todo review <run-id>` (--diff for the whole diff) and answer it with \
`herdr todo resume <run-id> --event <event-id> --action answer|approve|retry|verify|force-stop|\
retry-install|skip-install|retry-todo|skip-todo|retry-push|abort`. Read the worker's whole \
diff and last reply before approving; retry with a review (`--task <file>`) when it falls \
short. A question you cannot decide goes to the user (AskUserQuestion), never guessed; an \
open point the worker reports goes into the item or to \"Needs a decision\".
5. Approve with `--close <file>` (the decision record DECISIONS.md gets: what was chosen or \
rejected and why) when the item is finished, or `--note <file>` when it goes on. The run \
cherry-picks, installs, edits TODO.md and pushes.
6. When the run is done (or after step 1), end your turn. Your reply's last line is exactly one of:
{DONE} {item} | <one-line summary>
{ESCALATED} <what the user must decide>
{BLOCKED} <why the item cannot go on>

You may run: herdr todo run|wait|resume|review|status|runs, herdr history, herdr worker \
status|log|list|runs, herdr coordinator status, read-only git (status, log, diff, show), \
`git commit -m <message> -- TODO.md DECISIONS.md`, `python3 scripts/todo_edit.py` for this \
item's own notes and moves, and cat, head, tail, grep, rg, sed -n, jq, ls, wc. Do not edit \
code, start agents or other items' runs: herdr's allowlist refuses them."
    )
}

/// How an item coordinator ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    Escalated,
    Blocked,
    Failed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Escalated => "escalated",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

/// The outcome its last reply's last line reports, with what it says; a
/// reply without such a line, or no reply, failed.
fn classify(worker: &WorkerInfo) -> (Outcome, String) {
    let Some(result) = &worker.last_result else {
        return (
            Outcome::Failed,
            format!("it ended without a reply ({:?})", worker.state).to_lowercase(),
        );
    };
    let last = result
        .text
        .as_deref()
        .and_then(|text| text.lines().rev().find(|line| !line.trim().is_empty()))
        .map(str::trim)
        .unwrap_or_default();
    for (prefix, outcome) in [
        (DONE, Outcome::Done),
        (ESCALATED, Outcome::Escalated),
        (BLOCKED, Outcome::Blocked),
    ] {
        if let Some(rest) = last.strip_prefix(prefix) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return (outcome, rest.trim().to_owned());
            }
        }
    }
    let why = result
        .failure
        .clone()
        .or_else(|| {
            result
                .is_error
                .then(|| format!("its turn failed ({})", result.subtype))
        })
        .unwrap_or_else(|| format!("its last line is none of {DONE}, {ESCALATED} or {BLOCKED}"));
    (Outcome::Failed, why)
}

/// Who owns a chain and the coordinators it starts.
#[derive(Debug, Clone, Default)]
struct ChainOwner {
    pane: Option<String>,
    session: Option<String>,
    workspace: Option<String>,
}

impl ChainOwner {
    fn of(chain: &StoredChain) -> Self {
        Self {
            pane: chain.owner_pane.clone(),
            session: chain.owner_session.clone(),
            workspace: chain.workspace.clone(),
        }
    }

    fn as_run_owner(&self) -> RunOwner<'_> {
        RunOwner {
            pane_id: self.pane.as_deref(),
            session_id: self.session.as_deref(),
            workspace: self.workspace.as_deref(),
            coordinator_id: None,
        }
    }
}

fn chain_info(chain: StoredChain) -> TodoChainInfo {
    TodoChainInfo {
        active: chain.stopped_ms.is_none(),
        repo: chain.repo,
        owner_pane_id: chain.owner_pane,
        started_ms: chain.started_ms,
        last_item: chain.last_item,
        last_outcome: chain.last_outcome,
        stopped_ms: chain.stopped_ms,
        stop_reason: chain.stop_reason,
    }
}

/// What `todo.next` started.
#[derive(Debug)]
pub(crate) struct NextStarted {
    pub(crate) item: String,
    pub(crate) coordinator: CoordinatorInfo,
    pub(crate) worker: WorkerInfo,
    pub(crate) chain: Option<TodoChainInfo>,
}

/// The environment of the pane that started a repository's item
/// coordinators (`todo.next`'s `env`), which their runs' checks, install
/// and push use instead of a coordinator's sandboxed one. Never stored: it
/// may hold credentials. After a restart there is none, and a run's check
/// is then `unavailable` as after any restart.
static COORDINATOR_ENV: Mutex<BTreeMap<String, HashMap<String, String>>> =
    Mutex::new(BTreeMap::new());

/// The environment of `repo`'s item coordinators' runs, when the pane that
/// started them sent one.
pub(super) fn coordinator_env(repo: &str) -> Option<HashMap<String, String>> {
    lock(&COORDINATOR_ENV).get(repo).cloned()
}

/// The tenures this process watches, so one watcher runs per tenure.
static WATCHING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

struct Watching(String);

impl Watching {
    fn claim(id: &str) -> Option<Self> {
        lock(&WATCHING)
            .insert(id.to_owned())
            .then(|| Self(id.to_owned()))
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        lock(&WATCHING).remove(&self.0);
    }
}

impl WorkerSupervisor {
    /// The server's API socket, which an item coordinator's `herdr` talks
    /// to; a test's server has none, so a path beside its store.
    fn server_socket(&self) -> PathBuf {
        #[cfg(test)]
        {
            self.shared.dir.join("api.sock")
        }
        #[cfg(not(test))]
        {
            crate::api::socket_path()
        }
    }

    /// Writes herdr's copy of the allowlist script beside the store,
    /// published with a rename so a check never runs half a file.
    fn write_allowlist(&self) -> Result<PathBuf, WorkerError> {
        #[cfg(unix)]
        {
            let path = self.shared.dir.join(ALLOWLIST_FILE);
            static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let write = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let staged = self.shared.dir.join(format!(
                ".{ALLOWLIST_FILE}.{}.{write}.tmp",
                std::process::id()
            ));
            std::fs::create_dir_all(&self.shared.dir)?;
            std::fs::write(&staged, crate::integration::CLAUDE_HOOK_SCRIPT)?;
            std::fs::rename(&staged, &path)?;
            Ok(path)
        }
        #[cfg(not(unix))]
        {
            Err(WorkerError::Unsupported(format!(
                "a headless item coordinator's allowlist ({ALLOWLIST_FILE}) runs only on Unix"
            )))
        }
    }

    /// `todo.next`: starts an item coordinator for the top item of the
    /// repository's "Next, in order", and with `chain` the chain that starts
    /// the next one when it is done.
    pub(crate) fn todo_next(&self, params: &TodoNextParams) -> Result<NextStarted, WorkerError> {
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
        let owner = ChainOwner {
            pane: params.owner_pane_id.clone(),
            session: params.owner_session_id.clone(),
            workspace: params.workspace_id.clone(),
        };
        self.start_item_coordinator(&repo, &owner, params.chain, params.env.as_ref())
    }

    /// `todo.stop`: stops the repository's chain; the coordinator that runs
    /// finishes its item. A stopped chain is returned as it is.
    pub(crate) fn todo_stop(&self, cwd: &str) -> Result<TodoChainInfo, WorkerError> {
        let repo = repository_of(Path::new(cwd)).unwrap_or_else(|| cwd.to_owned());
        let stopped = self
            .run_store()?
            .transaction(|tx| tx.chain_stopped(&repo, STOPPED, now_ms()))
            .map_err(store_error)?
            .ok_or_else(|| {
                WorkerError::Invalid(format!(
                    "repository {repo} has no chain of item coordinators (`herdr todo next \
                     --continue` starts one)"
                ))
            })?;
        self.announce();
        Ok(chain_info(stopped))
    }

    /// Claims a headless tenure for the top item of "Next, in order" and
    /// starts its coordinator; `chain` starts (or keeps) the repository's
    /// chain in the claim's transaction, so a coordinator that ends at once
    /// finds it. Refused while a run or a coordinator of the repository is
    /// active, when "Next, in order" has no item and when the usage gate
    /// refuses; nothing is claimed then.
    fn start_item_coordinator(
        &self,
        repo: &str,
        owner: &ChainOwner,
        chain: bool,
        env: Option<&HashMap<String, String>>,
    ) -> Result<NextStarted, WorkerError> {
        let store = self.run_store()?;
        let todo_path = Path::new(repo).join(TODO_FILE);
        let todo = std::fs::read_to_string(&todo_path).map_err(|error| {
            WorkerError::Preflight(format!("cannot read {}: {error}", todo_path.display()))
        })?;
        let item = todo_titles::next_item(&todo).ok_or_else(|| {
            WorkerError::Preflight(format!(
                "\"{}\" in {} has no open item",
                todo_titles::NEXT_SECTION,
                todo_path.display()
            ))
        })?;
        let item_text = todo_titles::item_text(&todo, &item).unwrap_or_default();
        // Refused before the usage gate's reading, which costs a request.
        if let Some(active) = store.active_run(repo).map_err(store_error)? {
            return Err(Self::run_active(&active));
        }
        if let Some(active) = store.active_coordinator_of(repo).map_err(store_error)? {
            return Err(Self::refusal(repo, "none", active));
        }
        self.usage_gate(repo, false)?;
        let id = new_tenure_id(repo, &item);
        let at = now_ms();
        let claimed = store.transaction(|tx| {
            if let Some(active) = tx.active_run(repo)? {
                return Ok(Err(Self::run_active(&active)));
            }
            if let Some(active) = tx.active_coordinator(repo)? {
                return Ok(Err(Self::refusal(repo, "none", active)));
            }
            let tenure = NewTenure {
                id: &id,
                repo,
                pane_id: None,
                session_id: None,
            };
            tx.coordinator_started(&tenure, at)?;
            tx.coordinator_item(&id, Some(&item), at)?;
            tx.item_history(
                &NewHistory {
                    repo: repo.to_owned(),
                    item: item.clone(),
                    kind: HistoryEventKind::CoordinatorStarted,
                    run_id: None,
                    attempt: None,
                    text: None,
                    item_text: Some(item_text.clone()),
                    ids_at_claim: todo_titles::item_ids(&todo).into_iter().collect(),
                    follow_ups: Vec::new(),
                    coordinator_id: Some(id.clone()),
                },
                at,
            )?;
            let chain = if chain {
                tx.chain_started(repo, &owner.as_run_owner(), at)?;
                tx.chain_advanced(repo)?;
                tx.chain(repo)?
            } else {
                None
            };
            Ok(Ok(chain))
        });
        let chain = match claimed {
            Ok(Ok(chain)) => chain,
            Ok(Err(refused)) => return Err(refused),
            // Another server's claim committed between the check and ours.
            Err(error) if is_unique_violation(&error) => {
                return Err(WorkerError::CoordinatorActive(format!(
                    "repository {repo} already has an active coordinator"
                )))
            }
            Err(error) => return Err(store_error(error)),
        };
        // Claimed: a chain's next coordinator keeps the environment its
        // first one got.
        if let Some(env) = env {
            lock(&COORDINATOR_ENV).insert(repo.to_owned(), env.clone());
        }
        self.announce();
        let started = self.write_allowlist().and_then(|allowlist| {
            let role = CoordinatorRole {
                coordinator_id: id.clone(),
                socket: self.server_socket(),
                allowlist,
            };
            let params = WorkerStartParams {
                cwd: repo.to_owned(),
                prompt: prompt(repo, &item, &item_text),
                model: None,
                name: Some(format!("coordinator {item}")),
                workspace_id: owner.workspace.clone(),
                folder_slot: None,
                branch: None,
                base: None,
                fresh_build: false,
                owner_pane_id: owner.pane.clone(),
                owner_session_id: owner.session.clone(),
                item: Some(item.clone()),
                command_id: None,
            };
            self.start_once(&params, None, Some(&role))
        });
        let worker = match started {
            Ok(worker) => worker,
            Err(error) => {
                let why = format!("its worker did not start: {error}");
                self.end_item_coordinator(&id, repo, &item, None, Outcome::Failed, &why);
                return Err(error);
            }
        };
        store
            .transaction(|tx| tx.coordinator_worker(&id, &worker.worker_id, now_ms()))
            .map_err(store_error)?;
        self.spawn_item_watcher(&id);
        let coordinator = store
            .coordinator(&id)
            .map_err(store_error)?
            .map(info)
            .ok_or_else(|| {
                WorkerError::CoordinatorNotFound(format!("coordinator {id} not found"))
            })?;
        Ok(NextStarted {
            item,
            coordinator,
            worker,
            chain: chain.map(chain_info),
        })
    }

    fn spawn_item_watcher(&self, id: &str) {
        let supervisor = self.clone();
        let id = id.to_owned();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-item-coordinator", move || {
            supervisor.watch_item_coordinator(&id);
        }) {
            warn!(%error, "cannot watch the item coordinator");
        }
    }

    /// Waits for the item coordinator's turn to end, stops it, and on its
    /// exit event records its outcome; then the chain goes on or stops.
    /// Returns without a decision when this server handed off: the next
    /// server watches it again ([`Self::resume_item_coordinators`]).
    fn watch_item_coordinator(&self, id: &str) {
        let Some(_watching) = Watching::claim(id) else {
            return;
        };
        let store = match self.run_store() {
            Ok(store) => store,
            Err(error) => {
                warn!(%error, coordinator_id = id, "cannot watch the item coordinator");
                return;
            }
        };
        let tenure = match store.coordinator(id) {
            Ok(Some(tenure)) if tenure.ended_at.is_none() => tenure,
            Ok(_) => return,
            Err(error) => {
                warn!(%error, coordinator_id = id, "cannot read the item coordinator's tenure");
                return;
            }
        };
        let item = tenure.item.clone().unwrap_or_default();
        let Some(worker_id) = tenure.worker_id.clone() else {
            // A server that ended between the claim and the start.
            self.end_item_coordinator(
                id,
                &tenure.repo,
                &item,
                None,
                Outcome::Failed,
                "its server ended before its worker was recorded",
            );
            return;
        };
        let keep_waiting = || !self.handed_off_for_coordinators();
        let worker = match self.wait(
            &worker_id,
            WorkerWaitUntil::TurnEnd,
            NO_DEADLINE,
            keep_waiting,
        ) {
            Ok(Some(worker)) => worker,
            Ok(None) => return,
            Err(error) => {
                warn!(%error, coordinator_id = id, "cannot wait for the item coordinator");
                return;
            }
        };
        // The item is the worker's: a run of it that ended cleared the
        // tenure's.
        let item = worker.item.clone().unwrap_or(item);
        // A coordinator's turn is its work: it ends it. Its exit, which
        // its own exit event reports, ends the wait; one gone already
        // returns at once.
        {
            let target = WorkerCommandTarget {
                worker_id: worker_id.clone(),
                caller_pane_id: None,
                command_id: Some(format!("{id}:stop")),
            };
            let stopped = match self.stop_command(&target) {
                Err(WorkerError::CommandInterrupted(_)) => {
                    self.stop_command(&WorkerCommandTarget {
                        command_id: None,
                        ..target
                    })
                }
                other => other,
            };
            if let Err(error) = stopped {
                if !matches!(error, WorkerError::NotRunning(_)) {
                    warn!(%error, coordinator_id = id, "cannot stop the item coordinator");
                }
            }
            match self.wait(&worker_id, WorkerWaitUntil::Exit, NO_DEADLINE, keep_waiting) {
                Ok(Some(_)) => {}
                Ok(None) => return,
                Err(error) => {
                    warn!(%error, coordinator_id = id, "cannot wait for the item coordinator's exit");
                    return;
                }
            }
        }
        let worker = match self.status(&worker_id) {
            Ok(worker) => worker,
            Err(error) => {
                warn!(%error, coordinator_id = id, "cannot read the item coordinator");
                return;
            }
        };
        let (outcome, said) = classify(&worker);
        let advance =
            self.end_item_coordinator(id, &tenure.repo, &item, Some(&worker), outcome, &said);
        if advance {
            // The chain goes on: the owner owes nothing for this end.
            if let Some(seq) = worker.seq {
                if let Err(error) = self.ack(&worker_id, seq) {
                    warn!(%error, worker_id, "cannot acknowledge the item coordinator's end");
                }
            }
            self.advance_chain(&tenure.repo);
        }
    }

    /// Tells the clients, and whoever waits on the workers' changes, that a
    /// tenure or chain changed: notified under the registry lock, which a
    /// waiter checks under, so no wakeup is lost between them.
    fn announce(&self) {
        {
            let _registry = lock(&self.shared.registry);
            self.shared.changed.notify_all();
        }
        notify_clients();
    }

    fn handed_off_for_coordinators(&self) -> bool {
        self.shared
            .handed_off
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Ends the item coordinator's tenure with its outcome, recorded in the
    /// item's history, and settles the chain in the same transaction: a done
    /// item marks it advancing, anything else stops it. Herdr checks a done
    /// item itself: no run of the repository may be in progress, and the
    /// item may no longer be first in "Next, in order". Returns whether the
    /// chain is to start the next coordinator; nothing when the tenure had
    /// ended already.
    fn end_item_coordinator(
        &self,
        id: &str,
        repo: &str,
        item: &str,
        worker: Option<&WorkerInfo>,
        outcome: Outcome,
        said: &str,
    ) -> bool {
        let Ok(store) = self.run_store() else {
            return false;
        };
        let (outcome, said) = match outcome {
            Outcome::Done => match store.active_run(repo) {
                Ok(Some(run)) => (
                    Outcome::Blocked,
                    format!(
                        "it reported done while run {} is still in progress ({said})",
                        run.info.run_id
                    ),
                ),
                _ => {
                    let todo = std::fs::read_to_string(Path::new(repo).join(TODO_FILE))
                        .unwrap_or_default();
                    if todo_titles::next_item(&todo).as_deref() == Some(item) {
                        (
                            Outcome::Blocked,
                            format!(
                                "it reported done, but the item is still first in \"{}\" ({said})",
                                todo_titles::NEXT_SECTION
                            ),
                        )
                    } else {
                        (Outcome::Done, said.to_owned())
                    }
                }
            },
            other => (other, said.to_owned()),
        };
        let text = if said.is_empty() {
            outcome.as_str().to_owned()
        } else {
            format!("{}: {said}", outcome.as_str())
        };
        let at = now_ms();
        let ended = store.transaction(|tx| {
            if tx
                .coordinator(id)?
                .is_none_or(|tenure| tenure.ended_at.is_some())
            {
                return Ok(None);
            }
            tx.coordinator_ended(id, &format!("item_{}", outcome.as_str()), Some(&text), at)?;
            tx.item_history(
                &NewHistory {
                    repo: repo.to_owned(),
                    item: item.to_owned(),
                    kind: HistoryEventKind::CoordinatorEnded,
                    run_id: None,
                    attempt: None,
                    text: Some(text.clone()),
                    item_text: None,
                    ids_at_claim: Vec::new(),
                    follow_ups: Vec::new(),
                    coordinator_id: Some(id.to_owned()),
                },
                at,
            )?;
            if tx
                .chain(repo)?
                .is_none_or(|chain| chain.stopped_ms.is_some())
            {
                return Ok(Some(false));
            }
            let advance = outcome == Outcome::Done;
            tx.chain_outcome(repo, item, outcome.as_str(), advance)?;
            if !advance {
                tx.chain_stopped(repo, &format!("item {item} {text}"), at)?;
            }
            Ok(Some(advance))
        });
        self.announce();
        if !matches!(ended, Ok(Some(true))) {
            lock(&COORDINATOR_ENV).remove(repo);
        }
        match ended {
            Ok(Some(advance)) => advance,
            Ok(None) => false,
            Err(error) => {
                warn!(
                    %error,
                    coordinator_id = id,
                    worker_id = worker.map(|worker| worker.worker_id.as_str()),
                    "cannot record the item coordinator's end"
                );
                false
            }
        }
    }

    /// Starts the next coordinator of `repo`'s chain, when the chain is
    /// active and advancing; a refused start stops the chain with why (an
    /// empty "Next, in order" too).
    fn advance_chain(&self, repo: &str) {
        let Ok(store) = self.run_store() else {
            return;
        };
        let chain = match store.transaction(|tx| tx.chain(repo)) {
            Ok(Some(chain)) if chain.stopped_ms.is_none() && chain.advancing => chain,
            Ok(_) => return,
            Err(error) => {
                warn!(%error, repo, "cannot read the chain of item coordinators");
                return;
            }
        };
        if let Err(error) = self.start_item_coordinator(repo, &ChainOwner::of(&chain), true, None) {
            let reason = format!("the next item coordinator did not start: {error}");
            if let Err(error) = store.transaction(|tx| tx.chain_stopped(repo, &reason, now_ms())) {
                warn!(%error, repo, "cannot stop the chain of item coordinators");
            }
            self.announce();
        }
    }

    /// What a server that starts does for the item coordinators a previous
    /// one left: it watches each active headless tenure again (its worker
    /// re-attached, or lost, which ends it), and starts the next coordinator
    /// of a chain whose last one ended done just before that server did.
    pub(crate) fn resume_item_coordinators(&self) {
        let Ok(store) = self.run_store() else {
            return;
        };
        match store.active_coordinators(None) {
            Ok(tenures) => {
                for tenure in tenures.into_iter().filter(|tenure| tenure.headless) {
                    self.spawn_item_watcher(&tenure.id);
                }
            }
            Err(error) => warn!(%error, "cannot read the item coordinators"),
        }
        match store.chains(None) {
            Ok(chains) => {
                for chain in chains {
                    if chain.stopped_ms.is_none()
                        && chain.advancing
                        && store
                            .active_coordinator_of(&chain.repo)
                            .is_ok_and(|active| active.is_none())
                    {
                        self.advance_chain(&chain.repo);
                    }
                }
            }
            Err(error) => warn!(%error, "cannot read the chains of item coordinators"),
        }
    }

    #[cfg(all(test, unix))]
    pub(super) fn chain_for_test(&self, repo: &str) -> Option<TodoChainInfo> {
        self.run_store()
            .ok()?
            .transaction(|tx| tx.chain(repo))
            .ok()
            .flatten()
            .map(chain_info)
    }

    #[cfg(all(test, unix))]
    pub(super) fn tenure_for_test(&self, id: &str) -> Option<super::store::StoredTenure> {
        self.run_store().ok()?.coordinator(id).ok().flatten()
    }
}

/// Watches the item coordinators a previous server left, from a thread so
/// the server's start does not wait for the store.
pub(crate) fn resume_item_coordinators_at_start() {
    let spawned = crate::thread_spawn::spawn_named("herdr-item-coordinators", || {
        super::supervisor().resume_item_coordinators();
    });
    if let Err(error) = spawned {
        warn!(%error, "cannot resume the item coordinators");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{WorkerState, WorkerTurnResult};

    fn worker(text: Option<&str>) -> WorkerInfo {
        let mut worker: WorkerInfo = serde_json::from_value(json!({
            "worker_id": "w1", "state": "exited", "cwd": "/repo", "name": "c",
            "turns": 1, "journal_path": "/j",
        }))
        .unwrap();
        worker.last_result = text.map(|text| WorkerTurnResult {
            subtype: "success".into(),
            is_error: false,
            terminal_reason: None,
            api_error_status: None,
            text: Some(text.into()),
            failure: None,
            permission_denials: Vec::new(),
        });
        worker
    }

    #[test]
    fn the_outcome_is_the_last_lines_marker() {
        let cases = [
            (
                "done\nCOORDINATOR-DONE t-abcd2345 | landed\n\n",
                Outcome::Done,
                "t-abcd2345 | landed",
            ),
            (
                "COORDINATOR-ESCALATED which one?",
                Outcome::Escalated,
                "which one?",
            ),
            (
                "x\nCOORDINATOR-BLOCKED no disk",
                Outcome::Blocked,
                "no disk",
            ),
        ];
        for (text, outcome, said) in cases {
            assert_eq!(
                classify(&worker(Some(text))),
                (outcome, said.to_owned()),
                "{text}"
            );
        }
        // Only the last line counts, and only the exact marker.
        for text in [
            "COORDINATOR-DONE x\nthen more",
            "COORDINATOR-DONEx",
            "nothing",
        ] {
            assert_eq!(classify(&worker(Some(text))).0, Outcome::Failed, "{text}");
        }
        let mut gone = worker(None);
        gone.state = WorkerState::Lost;
        assert_eq!(classify(&gone).0, Outcome::Failed);
    }
}
