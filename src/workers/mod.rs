//! Headless Claude workers owned by the server.
//!
//! A worker is `claude -p` over stream-json pipes, without a terminal, in its
//! own process group. One reader thread per worker records every line in and
//! out, answers `can_use_tool` requests through [`policy`], and folds the
//! events into the worker's state. Every event goes through
//! [`WorkerSupervisor::commit_locked`]: folded, then stored with the
//! projections it changes in one transaction ([`store`],
//! `<state dir>/workers/workers.sqlite3`), then exported to the worker's JSONL
//! journal (`<id>.jsonl`), which `herdr worker log` reads. A request the
//! policy leaves to the user becomes a pending question, shown in the
//! client's `?` list and answered with `worker.answer`; the worker waits for
//! it without a time limit. After a server restart the workers are rebuilt
//! from the store's projections, and one that had not exited is recorded as
//! `lost`.
//!
//! The server that runs a worker holds an exclusive lock on the lock file
//! beside its journal (`<id>.lock`) until the worker's exit is stored, so
//! a server started by a live handoff marks `lost` only workers whose server
//! is gone. A worker whose broker owns its pipes ([`broker`]) survives a
//! handoff: the old server detaches, the new one re-attaches. Pipes a
//! server owns itself are not handed over: a handoff is refused while such a
//! worker's process is alive ([`prepare_for_handoff`]).
//!
//! Evidence for the message shapes and flags: `docs/headless-worker-trial-2026-10-07.md`.

#[cfg(unix)]
pub(crate) mod broker;
mod capabilities;
pub(crate) mod coordinators;
mod decisions;
mod error;
mod grant_questions;
mod history;
mod inbox;
#[cfg(test)]
mod install_script_tests;
mod item_coordinators;
mod journal;
mod live;
mod log;
mod policy;
mod pre_tool_checks;
mod registry;
pub(crate) mod reports;
mod review;
mod runs;
mod slot;
mod status;
mod store;
#[cfg(test)]
mod tests;
mod todo_titles;
pub(crate) mod transcript;
mod types;
mod verify;

pub(crate) use coordinators::coordinators;
#[cfg(test)]
pub(crate) use coordinators::set_test_coordinators;
pub(crate) use error::WorkerError;
pub(crate) use item_coordinators::resume_item_coordinators_at_start;
pub(crate) use log::{entry_lines, log_lines};
#[cfg(unix)]
pub(crate) use runs::{envs_for_handoff, restore_handed_off_envs};
pub(crate) use runs::{resume_runs_at_start, usage_reading_published};
pub(crate) use types::{
    Attention, ItemCounts, OwnerEvent, PendingWorkerQuestion, Takeover, TakeoverFound,
    TakeoverTabCandidate, UnfinishedTakeover, UserNotice, WorkerSummary, TAKEOVER_ID_ENV,
};

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::api::schema::{
    WorkerAnswerAsParams, WorkerAnswerParams, WorkerChoiceQuestion, WorkerCommandTarget,
    WorkerDecision, WorkerDenyAndStopParams, WorkerDrain, WorkerDrainAction, WorkerHeldOutput,
    WorkerInfo, WorkerInterruptParams, WorkerItemRuns, WorkerKillParams, WorkerKillReport,
    WorkerObligation, WorkerPipeHolder, WorkerPromptParams, WorkerQuestion, WorkerQuestionDetail,
    WorkerQuestionKind, WorkerQuestionState, WorkerRun, WorkerRunsParams, WorkerStartParams,
    WorkerState, WorkerVerification, WorkerVerifyParams, WorkerWaitDrainedParams, WorkerWaitUntil,
};
use crate::platform::Signal;

use journal::{
    import_journal, journal_ownership, note_journal_gap, own_journal, replay_journal, Journal,
    Ownership,
};
#[cfg(unix)]
use live::{continuity_gap_event, input_id, input_written, output_lost, Input};
use live::{Live, Reader, Source, Spooled};
use registry::{worker_number, Drain, Entry, Registry, StartAdmission};
#[cfg(unix)]
use status::BrokerRecord;
use status::{Before, Direction, Pending, Status, RESOLVED_QUESTIONS_KEPT};

/// Appended to the worker's system prompt; names the worker's temp dir and,
/// in a folder slot, its branch.
fn worker_contract(temp_dir: &Path, slot: Option<&slot::Slot>) -> String {
    let temp = temp_dir.display();
    let mut contract = format!(
        "You are a headless worker started by herdr. Nobody watches your output live. Work \
only inside your working directory. Bash runs in a sandbox without asking: it can write only \
to your working directory and your temp dir {temp}, has no network and cannot read \
credentials. A command the sandbox refuses fails; do not try to get around it, report it as \
blocked: no shims, wrappers or PATH tricks. Run only the tests that work in the sandbox \
and list in your final message the ones you could not run; the coordinator runs the full \
check outside it. Put drafts and scratch files under {temp} by that absolute path; never use \
$TMPDIR, it is shared with other sessions. File tools work only inside your working directory and \
{temp}. Your questions wait until the user answers, which can take long: ask only when you \
cannot go on without it, otherwise finish the task or stop and say what blocks you."
    );
    if let Some(slot) = slot {
        contract.push_str(&format!(
            " Your working directory is herdr's persistent worker folder, on the new branch \
{} from {}; commit your work on that branch. Its target/ and Zig cache stay warm between \
workers, so cargo builds and tests in the sandbox compile only what changed.",
            slot.branch, slot.base
        ));
    }
    contract
}

/// Credential files and directories under the home directory that sandboxed
/// Bash must not read and the file tools must not touch. The CLI itself runs
/// outside the sandbox, so its own login keeps working.
/// `true` marks a directory, whose deny rules need `/**`.
const HOME_CREDENTIALS: &[(&str, bool)] = &[
    ("~/.ssh", true),
    ("~/.aws", true),
    ("~/.gnupg", true),
    ("~/.config/gh", true),
    ("~/.claude/.credentials.json", false),
    ("~/.git-credentials", false),
    ("~/.netrc", false),
    ("~/.npmrc", false),
    ("~/.docker", true),
    ("~/.kube", true),
    ("~/.cargo/credentials", false),
    ("~/.cargo/credentials.toml", false),
];

/// Environment files in the worktree, at any depth. The sandbox's `denyRead`
/// takes globs (Observed with 2.1.293: `<worktree>/**/.env` blocked `cat
/// .env` and `cat sub/.env`).
const ENV_FILE_GLOBS: &[&str] = &["**/.env", "**/.env.*", "**/.envrc"];

/// The worker's `--settings`: the user's CLAUDE.md, settings and login stay,
/// the global hooks are off (trial 2, T2-1), the co-author trailer is off
/// (T3-5), and Bash runs in Claude Code's sandbox: writes only to the
/// worktree (the CLI's default) and `temp_dir`, credential paths unreadable,
/// no network, no unsandboxed escape, and no start at all without the
/// sandbox (T3-2, T3-3). The deny rules cover the file tools and the
/// in-process web tools, which the sandbox does not.
/// `extra_write` are further directories Bash may write, a folder slot's
/// build caches.
fn worker_settings(cwd_real: &Path, temp_dir: &Path, extra_write: &[PathBuf]) -> Value {
    let mut deny_read: Vec<String> = HOME_CREDENTIALS
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    deny_read.extend(
        ENV_FILE_GLOBS
            .iter()
            .map(|glob| cwd_real.join(glob).display().to_string()),
    );
    let mut deny_rules = Vec::new();
    for (path, is_dir) in HOME_CREDENTIALS {
        let pattern = if *is_dir {
            format!("{path}/**")
        } else {
            (*path).to_owned()
        };
        deny_rules.push(format!("Read({pattern})"));
        deny_rules.push(format!("Edit({pattern})"));
    }
    // `Read` matters: the CLI reads inside the worktree without asking
    // herdr. An `Edit(...)` rule also stopped the Write tool (Observed with
    // 2.1.293), while a `Write(...)` rule did not, so no rule is added per
    // write tool; herdr's policy denies these files by name as well.
    for glob in ENV_FILE_GLOBS {
        deny_rules.push(format!("Read({glob})"));
        deny_rules.push(format!("Edit({glob})"));
    }
    deny_rules.push("WebFetch".into());
    deny_rules.push("WebSearch".into());
    let mut allow_write = vec![temp_dir.display().to_string()];
    allow_write.extend(extra_write.iter().map(|dir| dir.display().to_string()));
    json!({
        "disableAllHooks": true,
        "attribution": {"commit": "", "pr": "", "sessionUrl": false},
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "filesystem": {
                "allowWrite": allow_write,
                "denyRead": deny_read,
            },
            "network": {"allowedDomains": [], "strictAllowlist": true},
        },
        "permissions": {"deny": deny_rules},
    })
}

/// Whether an environment variable carries a credential the worker's tools
/// could use or leak. What the CLI needs to log in stays: the `ANTHROPIC_*`
/// and `CLAUDE_CODE_*` variables, and the AWS or Google credentials when the
/// CLI uses Bedrock or Vertex (`bedrock`, `vertex`).
fn is_credential_env(name: &str, bedrock: bool, vertex: bool) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper.starts_with("ANTHROPIC_") || upper.starts_with("CLAUDE_CODE_") {
        return false;
    }
    if upper.starts_with("AWS_") {
        return !bedrock;
    }
    if upper == "GOOGLE_APPLICATION_CREDENTIALS" || upper.starts_with("CLOUDSDK_AUTH_") {
        return !vertex;
    }
    const NAMES: &[&str] = &[
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "NPM_TOKEN",
        "NODE_AUTH_TOKEN",
        "CARGO_REGISTRY_TOKEN",
        "DOCKER_AUTH_CONFIG",
    ];
    const PREFIXES: &[&str] = &["AZURE_", "CARGO_REGISTRIES_"];
    const SUFFIXES: &[&str] = &[
        "_TOKEN",
        "_API_KEY",
        "_APIKEY",
        "_SECRET",
        "_SECRET_KEY",
        "_ACCESS_KEY",
        "_PASSWORD",
        "_CREDENTIALS",
    ];
    NAMES.contains(&upper.as_str())
        || PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
        || SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

/// Whether `item` is a TODO item's stable id: `t-` and 8 lowercase
/// base32 characters (`scripts/todo_edit.py`).
fn is_item_id(item: &str) -> bool {
    item.strip_prefix("t-").is_some_and(|body| {
        body.len() == 8
            && body
                .chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
    })
}

fn check_item_id(item: &str) -> Result<(), WorkerError> {
    if is_item_id(item) {
        Ok(())
    } else {
        Err(WorkerError::Invalid(format!(
            "item {item:?} is not a TODO item id (t- and 8 characters a-z, 2-7)"
        )))
    }
}

/// The repository `dir` is in: the parent of its git common directory, the
/// same for every worktree of it.
fn repository_of(dir: &Path) -> Option<String> {
    let space = crate::workspace::git_space_metadata(dir)?;
    Path::new(&space.key)
        .parent()
        .map(|repo| repo.display().to_string())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// One line per holder of a held tool output, and the tool calls then
/// open: the user's notice and the worker's log show it.
pub(super) fn held_output_summary(report: &WorkerHeldOutput) -> String {
    let mut lines: Vec<String> = report
        .holders
        .iter()
        .map(|holder| format!("pid {} holds it: {}", holder.pid, holder.command))
        .collect();
    for call in &report.tool_calls {
        lines.push(match &call.command {
            Some(command) => format!("open {} call {}: {command}", call.tool, call.id),
            None => format!("open {} call {}", call.tool, call.id),
        });
    }
    lines.join("\n")
}

/// Workers' statuses after an `owner_moved` event written in a transaction
/// not committed yet, each with its number and the event's `seq`
/// ([`WorkerSupervisor::stage_owner_moves`]).
type StagedMoves = Vec<(u64, Status, i64)>;

/// What [`WorkerSupervisor::commit_locked`] did.
struct Committed {
    /// What the clients show of the worker changed.
    shown_changed: bool,
    /// The event's `seq` in memory; `None` for an unknown worker.
    seq: Option<i64>,
    /// The store committed the event.
    stored: bool,
}

/// A client command id while its command runs: reserved in the store with
/// the command's first event, settled with its outcome.
pub(crate) struct Receipt {
    command_id: String,
    method: &'static str,
    params: String,
    /// An event of the command has reserved it in the store.
    reserved: Cell<bool>,
}

impl Receipt {
    fn row(&self) -> store::NewReceipt<'_> {
        store::NewReceipt {
            command_id: &self.command_id,
            method: self.method,
            params: &self.params,
        }
    }
}

/// What [`WorkerSupervisor::claim_command`] found for a command id.
enum Claim<'a> {
    /// New: this call runs it; the guard lets the id go when dropped.
    Run(InFlight<'a>),
    /// Seen before: its stored outcome.
    Stored(store::StoredReceipt),
}

/// Holds a command id in [`Registry::commands`] while its command runs.
struct InFlight<'a> {
    supervisor: &'a WorkerSupervisor,
    command_id: String,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        lock(&self.supervisor.shared.registry)
            .commands
            .remove(&self.command_id);
        self.supervisor.shared.changed.notify_all();
    }
}

/// A command's parameters without its id, as a receipt compares them.
fn command_params(params: &impl Serialize) -> Value {
    let mut value = serde_json::to_value(params).unwrap_or(Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.remove("command_id");
    }
    value
}

struct Shared {
    dir: PathBuf,
    program: PathBuf,
    /// The worker store, or why it could not be opened; then every event's
    /// write fails and marks its worker degraded.
    store: Result<store::Store, String>,
    registry: Mutex<Registry>,
    changed: Condvar,
    /// Held while a folder slot is prepared and its worker registered, so
    /// two starts never prepare one slot at once.
    slot_lock: Mutex<()>,
    /// Set once a live handoff succeeded: this server's `todo run` drivers
    /// let go of their runs (and their run locks) for the new server.
    handed_off: std::sync::atomic::AtomicBool,
    /// Starts each new worker's broker; without one this server owns the
    /// worker's pipes, and the worker ends with the server. Windows has no
    /// broker yet: a documented gap.
    #[cfg(unix)]
    broker: Option<broker::Launcher>,
    /// The `[workers] pre_tool_checks` a test sets; the server reads its
    /// config file instead.
    #[cfg(test)]
    pre_tool_checks: Mutex<Vec<Vec<String>>>,
    /// The answer a test sets for `todo.run`'s usage gate in place of
    /// Claude's provider, which the server asks.
    #[cfg(test)]
    claude_usage: Mutex<Result<crate::api::schema::ProviderUsage, String>>,
    /// The program a test sets for a run's automatic review in place of
    /// `program`, which the server calls.
    #[cfg(test)]
    review_program: Mutex<Option<PathBuf>>,
}

/// Starts, tracks and stops headless workers.
#[derive(Clone)]
pub(crate) struct WorkerSupervisor {
    shared: Arc<Shared>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Marks a stop or kill `signal` event as sent by the worker's owner when
/// `caller_pane` is that owner, so its exit creates no obligation.
fn mark_by_owner(signal: &mut Value, status: &Status, caller_pane: Option<&str>) {
    if caller_pane.is_some() && caller_pane == status.owner_pane.as_deref() {
        signal["by_owner"] = json!(true);
    }
}

static SUPERVISOR: OnceLock<WorkerSupervisor> = OnceLock::new();

type ChangeNotifier = Arc<dyn Fn() + Send + Sync>;

/// Called whenever what the clients show of a worker changes (its state,
/// its pending questions, a takeover), so the server rebuilds the clients'
/// snapshots, which carry them to the sidebar and the `?` list.
static CHANGE_NOTIFIER: Mutex<Option<ChangeNotifier>> = Mutex::new(None);

pub(crate) fn set_change_notifier(notifier: ChangeNotifier) {
    *lock(&CHANGE_NOTIFIER) = Some(notifier);
}

fn notify_clients() {
    let notifier = lock(&CHANGE_NOTIFIER).clone();
    if let Some(notifier) = notifier {
        notifier();
    }
}

/// The notices no server took yet, and whether there are any, which the
/// server's loop reads without the lock.
static USER_NOTICES: Mutex<Vec<UserNotice>> = Mutex::new(Vec::new());
static USER_NOTICES_PENDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Queues a notice for the user and wakes the server, which sends it
/// ([`take_user_notices`]).
fn notify_user(notice: UserNotice) {
    lock(&USER_NOTICES).push(notice);
    USER_NOTICES_PENDING.store(true, std::sync::atomic::Ordering::Release);
    notify_clients();
}

/// The notices queued since the last call, oldest first.
pub(crate) fn take_user_notices() -> Vec<UserNotice> {
    if !USER_NOTICES_PENDING.swap(false, std::sync::atomic::Ordering::AcqRel) {
        return Vec::new();
    }
    std::mem::take(&mut *lock(&USER_NOTICES))
}

/// Every pending question of the server's supervisor; empty when no
/// supervisor was opened (it is never opened just to answer this).
pub(crate) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    SUPERVISOR
        .get()
        .map(WorkerSupervisor::pending_questions)
        .unwrap_or_default()
}

/// Reports an owner's event to the server's supervisor; nothing when no
/// supervisor was opened or no running worker is owned by `pane_id`.
pub(crate) fn owner_event(pane_id: &str, event: OwnerEvent) {
    if let Some(supervisor) = coordinators::installed() {
        supervisor.owner_event(pane_id, event, "");
    }
}

/// The owner panes [`owner_event`] can still act on, to check after panes
/// close; empty when no supervisor was opened.
pub(crate) fn owner_panes() -> Vec<String> {
    coordinators::installed()
        .map(WorkerSupervisor::owner_panes)
        .unwrap_or_default()
}

/// Re-evaluates every owner when herdr starts: the panes and agents of a
/// previous server may be gone, idle or blocked without any event this one
/// will see. `state` tells, for an owner pane, what it finds there now
/// (`None`: it works, nothing to do).
pub(crate) fn owners_at_start(state: impl Fn(&str) -> Option<OwnerEvent>) {
    if let Some(supervisor) = coordinators::installed() {
        supervisor.owners_at_start(state);
    }
}

/// Readies the server's workers for a live handoff without waiting
/// ([`WorkerSupervisor::prepare_for_handoff`]); the refusal says why. No
/// timer decides it: the caller ends the workers and waits for their exit
/// events before asking.
pub(crate) fn prepare_for_handoff(force: bool) -> Result<(), String> {
    match SUPERVISOR.get() {
        Some(supervisor) => supervisor.prepare_for_handoff(force).map(|_| ()),
        None => Ok(()),
    }
}

/// After a live handoff succeeded, hands the `todo run`s
/// ([`WorkerSupervisor::let_go_of_runs`]) and the workers with a broker to
/// the new server ([`WorkerSupervisor::detach_for_handoff`]).
pub(crate) fn detach_for_handoff() {
    if let Some(supervisor) = SUPERVISOR.get() {
        supervisor.let_go_of_runs();
    }
    #[cfg(unix)]
    if let Some(supervisor) = SUPERVISOR.get() {
        let detached = supervisor.detach_for_handoff();
        if !detached.is_empty() {
            info!(workers = ?detached, "headless workers handed to the new server");
        }
    }
}

/// [`ItemCounts`] of repository `repo` in the server's supervisor; zero
/// when no supervisor was opened.
pub(crate) fn item_counts(repo: &str) -> ItemCounts {
    SUPERVISOR
        .get()
        .map(|supervisor| supervisor.item_counts(repo))
        .unwrap_or_default()
}

/// Repositories found for directories, so a snapshot does not search the
/// file system again for a coordinator's directory. Only found ones are
/// kept: a directory may become a repository later.
static REPOSITORIES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// The repository `dir` is in, as workers' `repo` names it (the parent of
/// its git common directory); none outside git. Cached by directory.
pub(crate) fn repository_of_dir(dir: &str) -> Option<String> {
    if let Some(repo) = lock(&REPOSITORIES).get(dir) {
        return Some(repo.clone());
    }
    let repo = repository_of(Path::new(dir))?;
    let mut cache = lock(&REPOSITORIES);
    // A bound against a server that sees many directories over its life.
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.insert(dir.to_owned(), repo.clone());
    Some(repo)
}

/// Every worker of the server's supervisor, oldest first; empty when no
/// supervisor was opened.
pub(crate) fn summaries() -> Vec<WorkerSummary> {
    SUPERVISOR
        .get()
        .map(WorkerSupervisor::summaries)
        .unwrap_or_default()
}

/// The task a worker is named by: the first non-empty line of its prompt,
/// cut to a sidebar line.
fn task_name(prompt: &str) -> String {
    one_line(
        prompt
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(""),
        80,
    )
}

fn unfinished_takeover_of(status: &Status) -> Option<UnfinishedTakeover> {
    status.takeover_unfinished.then(|| UnfinishedTakeover {
        worker_id: status.worker_id.clone(),
        takeover_id: status.takeover_id.clone(),
        session_id: status.session_id.clone(),
    })
}

/// Why a takeover an earlier server left unfinished is not tried again
/// without `force`.
pub(crate) fn unfinished_takeover_refusal(worker_id: &str) -> String {
    format!(
        "worker {worker_id}: an earlier server claimed its takeover and ended before \
         recording a tab, and no tab carrying that takeover's id and no process \
         resuming its session was found; that server may still have opened a tab. \
         Retrying with --force opens another tab on the same session: if the \
         first one exists, two writers fork its transcript"
    )
}

/// The process facts recovery reads; [`LiveProcesses`] reads the system.
pub(crate) trait ProcessProbe {
    fn env_value(&self, pid: u32, key: &str) -> Option<String>;
    /// The live processes resuming the agent session `session_id`.
    fn resuming(&self, session_id: &str) -> Vec<u32>;
    fn parent(&self, pid: u32) -> Option<u32>;
}

/// The system's processes, through [`crate::platform`].
pub(crate) struct LiveProcesses;

impl ProcessProbe for LiveProcesses {
    fn env_value(&self, pid: u32, key: &str) -> Option<String> {
        crate::platform::process_env_value(pid, key)
    }

    fn resuming(&self, session_id: &str) -> Vec<u32> {
        crate::platform::processes_with_argv(&|argv| {
            crate::platform::argv_resumes_session(argv, session_id)
        })
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        crate::platform::process_parent(pid)
    }
}

/// Finds an unfinished takeover's tab among `tabs`: the one whose title or
/// pane shell's environment carries its id, else the one whose pane shell
/// is an ancestor of a process resuming its session. A resuming process
/// under no tab is reported as such.
pub(crate) fn find_takeover_tab(
    takeover: &UnfinishedTakeover,
    tabs: &[TakeoverTabCandidate],
    probe: &dyn ProcessProbe,
) -> TakeoverFound {
    if let Some(takeover_id) = takeover.takeover_id.as_deref() {
        let carries_id = |tab: &&TakeoverTabCandidate| {
            tab.title.contains(takeover_id)
                || tab.shell_pids.iter().any(|pid| {
                    probe.env_value(*pid, TAKEOVER_ID_ENV).as_deref() == Some(takeover_id)
                })
        };
        if let Some(tab) = tabs.iter().find(carries_id) {
            return TakeoverFound::Tab(tab.tab_id.clone());
        }
    }
    let Some(session_id) = takeover.session_id.as_deref() else {
        return TakeoverFound::Nothing;
    };
    // Bounds a walk through a parent table that changed under it.
    const MAX_DEPTH: usize = 64;
    let mut outside = None;
    for pid in probe.resuming(session_id) {
        let mut ancestor = Some(pid);
        for _ in 0..MAX_DEPTH {
            let Some(current) = ancestor.filter(|pid| *pid > 1) else {
                break;
            };
            if let Some(tab) = tabs.iter().find(|tab| tab.shell_pids.contains(&current)) {
                return TakeoverFound::Tab(tab.tab_id.clone());
            }
            ancestor = probe.parent(current).filter(|parent| *parent != current);
        }
        outside.get_or_insert(pid);
    }
    outside.map_or(TakeoverFound::Nothing, TakeoverFound::Process)
}

/// The takeover id of a new claim, unique per claim: this server's pid and
/// a counter tell its claims apart, `at_ms` those of servers before it.
fn new_takeover_id(worker_id: &str, at_ms: u64) -> String {
    static CLAIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let claim = CLAIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "takeover-{worker_id}-{at_ms:x}-{:x}-{claim}",
        std::process::id()
    )
}

/// The directory of the workers' journals: in herdr's state directory,
/// apart per named session.
fn workers_dir() -> PathBuf {
    let state_dir = crate::config::state_dir();
    match crate::session::active_name() {
        Some(name) => state_dir.join("sessions").join(name).join("workers"),
        None => state_dir.join("workers"),
    }
}

/// At a server's start: opens the supervisor, in a thread, when a worker's
/// broker may still serve it (a `<id>.sock` in the workers' directory, left
/// by the server a live handoff replaced or by one that died), so the
/// worker is read and its questions asked without waiting for a client's
/// first worker request. Otherwise the supervisor still opens on first use.
pub(crate) fn resume_brokered_at_start() {
    #[cfg(unix)]
    {
        let brokered = std::fs::read_dir(workers_dir())
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "sock"));
        if !brokered {
            return;
        }
        let spawned = crate::thread_spawn::spawn_named("herdr-worker-resume", || {
            supervisor();
            notify_clients();
        });
        if let Err(error) = spawned {
            warn!(%error, "cannot resume the headless workers at start");
        }
    }
}

#[cfg(all(test, unix))]
thread_local! {
    static TEST_SERVER_SUPERVISOR: std::cell::Cell<Option<&'static WorkerSupervisor>> =
        const { std::cell::Cell::new(None) };
}

/// Makes `supervisor` the server's [`supervisor`] on this test thread only,
/// so tests running in parallel in one process never share or replace it.
#[cfg(all(test, unix))]
pub(crate) fn set_test_supervisor(supervisor: WorkerSupervisor) {
    let leaked: &'static WorkerSupervisor = Box::leak(Box::new(supervisor));
    TEST_SERVER_SUPERVISOR.with(|cell| cell.set(Some(leaked)));
}

#[cfg(all(test, unix))]
pub(crate) use tests::FinishedWorker;

/// The server's supervisor, opened on first use ([`workers_dir`]).
pub(crate) fn supervisor() -> &'static WorkerSupervisor {
    #[cfg(all(test, unix))]
    if let Some(supervisor) = TEST_SERVER_SUPERVISOR.with(std::cell::Cell::get) {
        return supervisor;
    }
    SUPERVISOR.get_or_init(|| {
        let dir = workers_dir();
        #[cfg(unix)]
        let broker = match broker::Launcher::herdr() {
            Ok(launcher) => Some(launcher),
            Err(error) => {
                warn!(%error, "no worker broker: new workers end with this server");
                None
            }
        };
        #[cfg(unix)]
        return WorkerSupervisor::open_with(dir, PathBuf::from("claude"), broker);
        #[cfg(not(unix))]
        WorkerSupervisor::open(dir, PathBuf::from("claude"))
    })
}

impl WorkerSupervisor {
    /// Opens the worker directory and its store, and rebuilds the workers
    /// from the store's projections. A journal the store does not hold yet
    /// (written before the store) is imported into it once. A worker that
    /// has not ended and whose lock no server holds was left by a previous
    /// server: it is marked `lost`. One whose lock another server holds
    /// (the old server of a live handoff) is loaded as it is, and marked
    /// `lost` only if its exit is not recorded once that server lets go of
    /// it ([`Self::adopt_when_released`]).
    #[cfg(any(test, not(unix)))]
    pub(crate) fn open(dir: PathBuf, program: PathBuf) -> Self {
        Self::open_with(
            dir,
            program,
            #[cfg(unix)]
            None,
        )
    }

    /// [`Self::open`], starting new workers through `broker` when given.
    /// A worker whose broker still serves it is re-attached either way.
    fn open_with(
        dir: PathBuf,
        program: PathBuf,
        #[cfg(unix)] broker: Option<broker::Launcher>,
    ) -> Self {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            warn!(%error, dir = %dir.display(), "worker journal directory unavailable");
        }
        // A store whose projections cannot be read is not written either:
        // importing the journals again would duplicate its events.
        let opened = store::Store::open(&dir.join(store::STORE_FILE))
            .and_then(|store| Ok((store.load_all()?, store)))
            .map_err(|error| {
                warn!(%error, dir = %dir.display(), "worker store unavailable");
                format!("the worker store is unavailable: {error}")
            });
        let (mut stored, store) = match opened {
            Ok((stored, store)) => (stored, Ok(store)),
            Err(error) => (BTreeMap::new(), Err(error)),
        };
        // Every journal's number is reserved, readable or not, so a new
        // worker never appends to an old one's journal.
        let mut numbers: Vec<u64> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
                    .then(|| path.file_stem()?.to_str().and_then(worker_number))
                    .flatten()
            })
            .collect();
        numbers.extend(stored.keys().copied());
        numbers.sort_unstable();
        numbers.dedup();

        let mut registry = Registry {
            next_number: 1,
            ..Registry::default()
        };
        let mut owned_elsewhere = Vec::new();
        // Held until the workers left by a gone server are marked lost.
        let mut unowned = Vec::new();
        for number in numbers {
            registry.next_number = registry.next_number.max(number + 1);
            let worker_id = format!("w{number}");
            let path = dir.join(format!("{worker_id}.jsonl"));
            let ownership = journal_ownership(&path).unwrap_or_else(|error| {
                warn!(%error, path = %path.display(), "worker journal lock unavailable");
                Ownership::Unowned { _held: None }
            });
            let loaded = match (stored.remove(&number), &ownership, &store) {
                (Some(mut status), ..) => {
                    status.mark_unfinished_takeover();
                    if let Ok(store) = &store {
                        note_journal_gap(store, &mut status, &path);
                    }
                    Ok(status)
                }
                // A server from before the store runs it and writes only
                // its journal: imported once it lets go.
                (None, Ownership::Owned(_), _) | (None, _, Err(_)) => {
                    replay_journal(&worker_id, &path).map_err(|error| error.to_string())
                }
                (None, Ownership::Unowned { .. }, Ok(store)) => {
                    import_journal(store, &worker_id, &path)
                }
            };
            let status = match loaded {
                Ok(status) => status,
                Err(error) => {
                    warn!(%error, path = %path.display(), "worker journal unreadable");
                    continue;
                }
            };
            let mut entry = Entry::new(status, path);
            match ownership {
                Ownership::Owned(lock) => {
                    entry.foreign = true;
                    owned_elsewhere.push((number, lock));
                }
                Ownership::Unowned { _held } => unowned.push((number, _held)),
            }
            registry.workers.insert(number, entry);
        }
        let supervisor = Self {
            shared: Arc::new(Shared {
                dir,
                program,
                store,
                registry: Mutex::new(registry),
                changed: Condvar::new(),
                slot_lock: Mutex::new(()),
                handed_off: std::sync::atomic::AtomicBool::new(false),
                #[cfg(unix)]
                broker,
                #[cfg(test)]
                pre_tool_checks: Mutex::new(Vec::new()),
                #[cfg(test)]
                claude_usage: Mutex::new(Ok(runs::low_usage_for_test())),
                #[cfg(test)]
                review_program: Mutex::new(None),
            }),
        };
        for (number, held) in unowned {
            // Held until the worker is re-attached or marked lost.
            let _held = match held {
                Some(owner_lock) => match supervisor.reattach(number, owner_lock) {
                    None => continue,
                    Some(owner_lock) => Some(owner_lock),
                },
                None => None,
            };
            supervisor.settle_unowned(number);
        }
        for (number, owner_lock) in owned_elsewhere {
            supervisor.adopt_when_released(number, owner_lock);
        }
        supervisor
    }

    /// For a worker no server runs any more: records it `lost` unless it
    /// has ended, then removes its temp dir once its process is gone. A
    /// lost worker's process may still run (a server that died without its
    /// workers); its temp dir is then checked again at the next start.
    fn settle_unowned(&self, number: u64) {
        let process_gone = {
            let mut registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get_mut(&number) else {
                return;
            };
            entry.foreign = false;
            if !entry.status.is_gone() {
                let lost = json!({"type": "lost", "reason": "server restarted"});
                self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&lost),
                );
            }
            let status = &registry.workers[&number].status;
            let process_gone = status.process_gone();
            let answering: Vec<String> = status
                .questions
                .iter()
                .filter(|pending| pending.answering)
                .map(|pending| pending.question.request_id.clone())
                .collect();
            // An answer whose write a previous server did not confirm: its
            // worker can no longer take it, unless its process outlived
            // that server, which is reported, not guessed at.
            for request_id in &answering {
                if process_gone {
                    let expired = json!({
                        "type": "answer_expired",
                        "request_id": request_id,
                        "how": "expired: a server restart found its answer not confirmed sent",
                    });
                    self.commit_locked(
                        &mut registry,
                        number,
                        Direction::Herdr,
                        store::Recorded::Event(&expired),
                    );
                } else {
                    let worker_id = format!("w{number}");
                    warn!(
                        worker_id,
                        request_id,
                        "an answer in flight at a server restart; its worker still runs"
                    );
                    if let Some(entry) = registry.workers.get_mut(&number) {
                        entry.status.degraded = Some(format!(
                            "the answer to question {request_id} was not confirmed sent before \
                             a server restart, and the worker's process still runs outside this \
                             server: it may or may not have received it"
                        ));
                    }
                }
            }
            process_gone
        };
        if process_gone {
            remove_temp_dir(&temp_dir_path(&self.shared.dir, &format!("w{number}")));
        }
    }

    /// Waits, in a thread, until the server that holds a worker's journal
    /// lets go of it (the worker's exit is recorded, or that server ended),
    /// then loads the worker again (from the store, or by importing the
    /// journal a server from before the store wrote) and marks it `lost`
    /// if its exit is not there.
    fn adopt_when_released(&self, number: u64, owner_lock: File) {
        let supervisor = self.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-worker-owner", move || {
            let worker_id = format!("w{number}");
            let path = supervisor.journal_path(&worker_id);
            if let Err(error) = owner_lock.lock() {
                warn!(%error, path = %path.display(), "cannot wait for the worker journal's lock");
                return;
            }
            let loaded = match &supervisor.shared.store {
                Ok(store) => match store.load(&worker_id) {
                    Ok(Some(mut status)) => {
                        note_journal_gap(store, &mut status, &path);
                        Ok(status)
                    }
                    Ok(None) => import_journal(store, &worker_id, &path),
                    Err(error) => Err(error.to_string()),
                },
                Err(_) => replay_journal(&worker_id, &path).map_err(|error| error.to_string()),
            };
            let mut status = match loaded {
                Ok(status) => status,
                Err(error) => {
                    warn!(%error, path = %path.display(), "worker journal unreadable");
                    return;
                }
            };
            status.mark_unfinished_takeover();
            {
                let mut registry = lock(&supervisor.shared.registry);
                if let Some(entry) = registry.workers.get_mut(&number) {
                    if entry.live.is_none() {
                        entry.status = status;
                    }
                }
            }
            let Some(owner_lock) = supervisor.reattach(number, owner_lock) else {
                supervisor.shared.changed.notify_all();
                notify_clients();
                return;
            };
            supervisor.settle_unowned(number);
            drop(owner_lock);
            supervisor.shared.changed.notify_all();
            notify_clients();
        });
        if let Err(error) = spawned {
            warn!(%error, "worker journal owner wait unavailable");
        }
    }

    /// Readies this server's workers for a live handoff, which cannot carry
    /// their pipes. A worker with a broker survives it, whatever it does:
    /// once the handoff succeeded this server detaches from its broker
    /// ([`Self::detach_for_handoff`]), and the new server re-attaches from
    /// the last broker seq the store holds, replaying what it missed, and
    /// sends again the answers not confirmed sent, which the broker writes
    /// at most once. The others (started without a broker: by a build
    /// before it, or on Windows) end with this server's pipes: without
    /// `force` it refuses while any of their processes is alive, in a turn
    /// or idle between turns, naming each and how to end it. With `force`
    /// it sends each SIGTERM and goes on.
    ///
    /// It never waits for an exit. It runs on the server's main loop, and
    /// waiting there would freeze every pane and client; a deadline would
    /// let a timer decide whether the handoff happens. The caller stops the
    /// idle workers first and waits for each one's exit event
    /// (`herdr worker stop`, then `herdr worker wait --exit`, as
    /// `scripts/herdr_live.sh` does), then asks again. Returns the workers
    /// it signalled.
    pub(crate) fn prepare_for_handoff(&self, force: bool) -> Result<Vec<String>, String> {
        let running: Vec<(String, WorkerState, bool, bool)> = {
            let registry = lock(&self.shared.registry);
            registry
                .workers
                .values()
                .filter(|entry| entry.live.is_some() && !entry.status.is_gone())
                .filter(|entry| !entry.survives_handoff())
                .map(|entry| {
                    (
                        entry.status.worker_id.clone(),
                        entry.status.state,
                        entry.status.turn_ended(),
                        entry.status.stop_requested_ms.is_some(),
                    )
                })
                .collect()
        };
        if running.is_empty() {
            return Ok(Vec::new());
        }
        if !force {
            let named: Vec<String> = running
                .iter()
                .map(|(worker_id, state, turn_ended, stopping)| {
                    let doing = match (stopping, turn_ended, state) {
                        (true, ..) => "stopping",
                        (false, true, _) => "idle between turns",
                        (false, false, WorkerState::Starting) => "starting",
                        (false, false, WorkerState::WaitingApproval) => "waiting for an answer",
                        (false, false, _) => "working",
                    };
                    format!("{worker_id} ({doing})")
                })
                .collect();
            return Err(format!(
                "refusing the live handoff: headless workers are running: {}. Their pipes \
                 are not handed over, so the handoff would end them. End them first: \
                 `herdr worker stop <id>` (safe for one idle between turns; one in a turn \
                 loses that turn), then `herdr worker wait <id> --exit`. Or hand off anyway \
                 with `herdr server live-handoff --force`.",
                named.join(", ")
            ));
        }
        for (worker_id, ..) in &running {
            if let Err(error) = self.stop(worker_id) {
                warn!(%error, worker_id, "cannot stop worker before a forced handoff");
            }
        }
        Ok(running
            .into_iter()
            .map(|(worker_id, ..)| worker_id)
            .collect())
    }

    /// After a live handoff succeeded: lets go of every worker this server
    /// reads through a broker, without closing its input or recording an
    /// exit, so the journal's lock goes to the new server, which waits for
    /// it ([`Self::adopt_when_released`]) and re-attaches. Returns the
    /// workers it detached.
    #[cfg(unix)]
    pub(crate) fn detach_for_handoff(&self) -> Vec<String> {
        let lives: Vec<(u64, Arc<Live>)> = lock(&self.shared.registry)
            .workers
            .iter()
            .filter(|(_, entry)| !entry.status.is_gone())
            .filter_map(|(number, entry)| Some((*number, Arc::clone(entry.live.as_ref()?))))
            .collect();
        // Outside the registry lock: a sender holds the input's lock while
        // it takes the registry's.
        let numbers: Vec<u64> = lives
            .into_iter()
            .filter(|(_, live)| live.detach())
            .map(|(number, _)| number)
            .collect();
        let mut registry = lock(&self.shared.registry);
        numbers
            .into_iter()
            .filter_map(|number| {
                let entry = registry.workers.get_mut(&number)?;
                entry.live = None;
                entry.foreign = true;
                Some(entry.status.worker_id.clone())
            })
            .collect()
    }

    /// The drain before an install: `start` stops admitting new turns
    /// (`worker.prompt` and `worker.start` are refused with
    /// `workers_draining`, naming `reason`), `cancel` admits them again,
    /// `status` changes nothing. Turns already running go on, and answers
    /// and interrupts still reach them. Starting an active drain keeps it.
    /// The drain lives in this server's memory only, so it ends with a
    /// handoff: the new server admits turns.
    pub(crate) fn drain(&self, action: WorkerDrainAction, reason: Option<&str>) -> WorkerDrain {
        let (drain, changed) = {
            let mut registry = lock(&self.shared.registry);
            let changed = match action {
                WorkerDrainAction::Start if registry.drain.is_none() => {
                    let reason = reason
                        .map(str::trim)
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or("an install");
                    registry.drain = Some(Drain {
                        reason: one_line(reason, 200),
                        started_ms: now_ms(),
                    });
                    true
                }
                WorkerDrainAction::Cancel => registry.drain.take().is_some(),
                _ => false,
            };
            (Self::drain_locked(&registry, &[]), changed)
        };
        if changed {
            info!(?action, reason = drain.reason.as_deref(), "worker drain");
            self.shared.changed.notify_all();
        }
        drain
    }

    /// Blocks until the workers in a turn differ from `params.in_turn` (one
    /// of them ended its turn, or another began one), a drain starts or ends
    /// when `params.draining` says what the caller saw, or at once when none
    /// is in a turn and no admitted start is pending. Woken by the workers' events,
    /// never decided by a timer: the state is checked under the registry
    /// lock right before each wait, and every commit notifies after taking
    /// that lock. `keep_waiting` runs at least every `liveness_check`, only
    /// so that a caller whose client went away can give up (`None`).
    pub(crate) fn wait_drained(
        &self,
        params: &WorkerWaitDrainedParams,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Option<WorkerDrain> {
        let seen = &params.in_turn;
        let seen_ids: BTreeSet<&str> = seen.iter().map(String::as_str).collect();
        let reached = |registry: &Registry| {
            let drain = Self::drain_locked(registry, seen);
            let drain_changed = params
                .draining
                .is_some_and(|draining| draining != drain.draining);
            let in_turn: BTreeSet<&str> = drain
                .in_turn
                .iter()
                .map(|worker| worker.worker_id.as_str())
                .collect();
            let settled = in_turn.is_empty() && drain.starting == 0;
            (settled || drain_changed || in_turn != seen_ids).then_some(drain)
        };
        let mut registry = lock(&self.shared.registry);
        loop {
            if let Some(drain) = reached(&registry) {
                return Some(drain);
            }
            drop(registry);
            if !keep_waiting() {
                return None;
            }
            registry = lock(&self.shared.registry);
            if let Some(drain) = reached(&registry) {
                return Some(drain);
            }
            registry = self
                .shared
                .changed
                .wait_timeout(registry, liveness_check)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// The drain and the workers in a turn; `ended` lists those of `seen`
    /// that are not in one any more.
    fn drain_locked(registry: &Registry, seen: &[String]) -> WorkerDrain {
        let in_turn: Vec<WorkerInfo> = registry
            .workers
            .values()
            .filter(|entry| entry.live.is_some() && !entry.status.turn_ended())
            // A handoff keeps those: their turns go on through the new
            // server.
            .filter(|entry| !entry.survives_handoff())
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect();
        let ended = seen
            .iter()
            .filter(|id| !in_turn.iter().any(|worker| &worker.worker_id == *id))
            .filter_map(|id| registry.workers.get(&worker_number(id)?))
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect();
        WorkerDrain {
            draining: registry.drain.is_some(),
            reason: registry.drain.as_ref().map(|drain| drain.reason.clone()),
            started_ms: registry.drain.as_ref().map(|drain| drain.started_ms),
            in_turn,
            starting: registry.starts_admitted,
            ended,
        }
    }

    /// Refuses a new turn while the server drains.
    fn admits_turns(registry: &Registry) -> Result<(), WorkerError> {
        match &registry.drain {
            None => Ok(()),
            Some(drain) => Err(WorkerError::Draining(format!(
                "workers are draining for {}: no new turns until it hands off or \
                 `herdr worker drain cancel` admits them again",
                drain.reason
            ))),
        }
    }

    /// Admits a worker start unless the server drains, counting it until
    /// the returned guard drops.
    fn admit_start(&self) -> Result<StartAdmission<'_>, WorkerError> {
        let mut registry = lock(&self.shared.registry);
        Self::admits_turns(&registry)?;
        registry.starts_admitted += 1;
        Ok(StartAdmission { supervisor: self })
    }

    /// Starts a worker. With a command id, a repeat returns the worker that
    /// id started, as the first reply showed it; one a server restart cut
    /// off returns that worker's status now.
    pub(crate) fn start(&self, params: &WorkerStartParams) -> Result<WorkerInfo, WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.start",
            params,
            |stored| match &stored.worker_id {
                Some(worker_id) => self.status(worker_id),
                None => Err(Self::cut_off(stored)),
            },
            |receipt| self.start_once(params, receipt, None),
        )
    }

    /// Starts a worker; with `role`, as a headless item coordinator
    /// ([`item_coordinators`]): its own contract, herdr's coordinator
    /// allowlist as its first pre-tool check, and the server's socket
    /// reachable from its sandbox.
    fn start_once(
        &self,
        params: &WorkerStartParams,
        receipt: Option<&Receipt>,
        role: Option<&item_coordinators::CoordinatorRole>,
    ) -> Result<WorkerInfo, WorkerError> {
        let cwd = params.cwd.as_str();
        let prompt = params.prompt.as_str();
        let model = params.model.as_deref();
        let cwd_path = PathBuf::from(cwd);
        if !cwd_path.is_absolute() {
            return Err(WorkerError::Invalid(format!("cwd must be absolute: {cwd}")));
        }
        let cwd_real = cwd_path
            .canonicalize()
            .map_err(|error| WorkerError::Invalid(format!("cwd {cwd}: {error}")))?;
        if !cwd_real.is_dir() {
            return Err(WorkerError::Invalid(format!(
                "cwd is not a directory: {cwd}"
            )));
        }
        if prompt.trim().is_empty() {
            return Err(WorkerError::Invalid("prompt must not be empty".into()));
        }
        if let Some(item) = &params.item {
            check_item_id(item)?;
        }
        if !crate::platform::WORKER_SANDBOX_SUPPORTED {
            return Err(WorkerError::Unsupported(
                "headless workers run Bash in Claude Code's sandbox, which this platform does not \
                 have; refusing to start a worker without it"
                    .into(),
            ));
        }
        // Counted until the worker is registered, so a drain that starts
        // meanwhile waits for its first turn.
        let _admission = self.admit_start()?;
        // Held until the worker is registered, so the next start into the
        // same slot sees it running.
        let _slot_guard = params
            .folder_slot
            .as_ref()
            .map(|_| lock(&self.shared.slot_lock));
        let (slot, slot_killed) = match params.folder_slot.as_deref() {
            Some(name) => {
                let (slot, killed) = self.prepare_slot(&cwd_real, name, params)?;
                (Some(slot), killed)
            }
            None if params.branch.is_some() || params.base.is_some() || params.fresh_build => {
                return Err(WorkerError::Invalid(
                    "branch, base and fresh_build need folder_slot".into(),
                ))
            }
            None => (None, Vec::new()),
        };
        let (cwd_path, cwd_real) = match &slot {
            Some(slot) => (slot.path.clone(), slot.path.clone()),
            None => (cwd_path, cwd_real),
        };
        let slot_caches: Vec<PathBuf> = slot
            .iter()
            .flat_map(|slot| [slot.target_dir(), slot.zig_cache_dir()])
            .collect();

        let number = {
            let mut registry = lock(&self.shared.registry);
            let number = registry.next_number;
            registry.next_number += 1;
            number
        };
        let worker_id = format!("w{number}");
        std::fs::create_dir_all(&self.shared.dir)?;
        let journal_path = self.shared.dir.join(format!("{worker_id}.jsonl"));
        let owner_lock = own_journal(&journal_path, &worker_id)?;
        let temp_dir = self.create_temp_dir(&worker_id)?;
        let mut settings = worker_settings(&cwd_real, &temp_dir, &slot_caches);
        let (contract, checks, check_errors) = match role {
            Some(role) => {
                role.restrict_settings(&mut settings);
                let (mut checks, errors) = self.configured_pre_tool_checks();
                // Herdr's own check first: a user's check never sees a call
                // the allowlist refuses.
                checks.insert(0, role.allowlist_check(&temp_dir));
                (item_coordinators::contract(&temp_dir), checks, errors)
            }
            None => {
                let (checks, errors) = self.configured_pre_tool_checks();
                (worker_contract(&temp_dir, slot.as_ref()), checks, errors)
            }
        };
        let settings = settings.to_string();

        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--replay-user-messages",
            "--permission-mode",
            "manual",
            "--permission-prompt-tool",
            "stdio",
            "--settings",
            &settings,
            "--append-system-prompt",
            &contract,
        ]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
        if let Some(model) = model {
            args.push("--model".into());
            args.push(model.to_owned());
        }

        // The worker's hooks and herdr CLI calls must not act on the
        // server's own pane or session (trial 1).
        // Nor may it use the user's credentials for other services (the
        // sandbox has no network, but a token can still end up in a file).
        let enabled = |name: &str| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0")
        };
        let bedrock = enabled("CLAUDE_CODE_USE_BEDROCK");
        let vertex = enabled("CLAUDE_CODE_USE_VERTEX");
        let mut removed_env = Vec::new();
        let mut env_removed = Vec::new();
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("HERDR_") {
                env_removed.push(key);
            } else if is_credential_env(&name, bedrock, vertex) {
                removed_env.push(name.into_owned());
                env_removed.push(key);
            }
        }
        removed_env.sort();
        // The slot's caches, inside the slot, so a sandboxed build writes
        // only there instead of the user's `~/.cache/zig` or another target.
        let mut env_set = Vec::new();
        // An item coordinator runs `herdr todo ...` against this server.
        if let Some(role) = role {
            env_set.push((crate::api::SOCKET_PATH_ENV_VAR, role.socket.clone()));
        }
        if let Some(slot) = &slot {
            let zig_cache = slot.zig_cache_dir();
            env_set.push(("CARGO_TARGET_DIR", slot.target_dir()));
            env_set.push(("ZIG_GLOBAL_CACHE_DIR", zig_cache.clone()));
            env_set.push(("ZIG_LOCAL_CACHE_DIR", zig_cache));
        }
        let configure = |command: &mut Command| {
            for key in &env_removed {
                command.env_remove(key);
            }
            for (key, value) in &env_set {
                command.env(key, value);
            }
        };
        let spawned = self
            .spawn_worker(&worker_id, &args, &cwd_real, configure)
            .inspect_err(|_| remove_temp_dir(&temp_dir))?;
        let pid = spawned.pid;

        let policy = policy::Policy::new(&cwd_path, &cwd_real, &temp_dir);
        let name = params
            .name
            .as_deref()
            .map(|name| one_line(name, 80))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| task_name(prompt));
        let repo = repository_of(&cwd_real);
        // The title as it is now: the item leaves TODO.md once finished.
        let item_title = params
            .item
            .as_ref()
            .zip(repo.as_ref())
            .and_then(|(item, repo)| todo_titles::read_titles(Path::new(repo)).remove(item));
        let started = json!({
            "type": "started",
            "worker_id": worker_id,
            "name": name,
            "workspace_id": params.workspace_id,
            "cwd": cwd_real.display().to_string(),
            "model": model,
            "temp_dir": temp_dir.display().to_string(),
            "removed_env": removed_env,
            "pid": pid,
            "broker": spawned.broker.as_ref().map(|(broker_pid, socket)| json!({
                "pid": broker_pid,
                "socket": socket,
                "cwd": cwd_path,
                "pre_tool_checks": checks,
            })),
            "pre_tool_checks": checks,
            "program": self.shared.program.display().to_string(),
            "args": args,
            "owner": params.owner_pane_id.as_ref().map(|pane_id| json!({
                "pane_id": pane_id,
                "session_id": params.owner_session_id,
                "coordinator_id": self.coordinator_of_pane(pane_id).map(|tenure| tenure.coordinator_id),
            })),
            "item": params.item,
            "item_title": item_title,
            "repo": repo,
            "coordinator": role.map(|role| json!({"coordinator_id": role.coordinator_id})),
            "folder_slot": slot.as_ref().map(|slot| json!({
                "name": params.folder_slot,
                "branch": slot.branch,
                "base": slot.base,
                "created": slot.created,
                "fresh_build": params.fresh_build,
                "killed_leftover_pids": slot_killed,
            })),
        });
        let live = Arc::new(Live::new(number, spawned.input));
        {
            let mut registry = lock(&self.shared.registry);
            let mut entry = Entry::new(Status::new(worker_id.clone()), journal_path);
            entry.live = Some(Arc::clone(&live));
            registry.workers.insert(number, entry);
            self.commit_command_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&started),
                receipt,
            );
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(
                    &json!({"type": "policy", "file_tool_roots": policy.roots()}),
                ),
            );
            for error in &check_errors {
                self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(
                        &json!({"type": "pre_tool_check_failed", "error": error}),
                    ),
                );
            }
        }
        self.shared.changed.notify_all();
        notify_clients();

        #[cfg(unix)]
        let spool = spawned
            .broker
            .as_ref()
            .map(|(_, socket)| broker::spool_path(socket));
        #[cfg(not(unix))]
        let spool = None;
        let checks = pre_tool_checks::Runner::new(checks, &cwd_real);
        let reader = Reader {
            supervisor: self.clone(),
            number,
            pid,
            policy,
            checks: checks.clone(),
            temp_dir: temp_dir.clone(),
            spool,
            live: Arc::clone(&live),
            owner_lock,
        };
        let reading = match spawned.source {
            Source::Pipes {
                stdout,
                stderr,
                child,
            } => {
                let stderr_supervisor = self.clone();
                if let Err(error) =
                    crate::thread_spawn::spawn_named("herdr-worker-err", move || {
                        for line in BufReader::new(stderr).lines() {
                            let Ok(line) = line else { break };
                            stderr_supervisor.record_raw(number, Direction::Err, &line);
                        }
                    })
                {
                    warn!(%error, "worker stderr reader unavailable");
                }
                crate::thread_spawn::spawn_named("herdr-worker", move || reader.run(stdout, child))
                    .map(drop)
            }
            #[cfg(unix)]
            Source::Broker { messages, process } => {
                crate::thread_spawn::spawn_named("herdr-worker", move || {
                    reader.run_broker(messages, Some(process));
                })
                .map(drop)
            }
        };
        if let Err(error) = reading {
            // Without a reader nobody would reap or journal the process.
            let _ = crate::platform::signal_process_group(pid, Signal::Kill);
            if let Some((broker_pid, _)) = &spawned.broker {
                let _ = crate::platform::signal_process_group(*broker_pid, Signal::Kill);
            }
            remove_temp_dir(&temp_dir);
            self.record(
                number,
                Direction::Herdr,
                &json!({"type": "exited", "code": null}),
            );
            return Err(WorkerError::Io(error));
        }
        self.watch_held_output(number, pid);

        // The CLI buffers input written before `system/init` (trial 1). The
        // hook is registered before the prompt, so it covers the first call;
        // every worker has it, for the rule against background waits.
        live.send(self, &pre_tool_checks::initialize_request())?;
        let turn_seq = live.send(self, &user_message(prompt))?;
        let mut info = self.status(&worker_id)?;
        info.turn_seq = Some(turn_seq);
        Ok(info)
    }

    /// The `[workers] pre_tool_checks` for a worker that starts now, with
    /// `~` expanded, and why a check is left out. The config file is read
    /// at each start, so a change applies to the next worker. A config that
    /// cannot be read runs no check: the worker starts anyway, and the
    /// reason is its `pre_tool_check_failed` event.
    fn configured_pre_tool_checks(&self) -> (Vec<Vec<String>>, Vec<String>) {
        #[cfg(test)]
        let configured: Result<Vec<Vec<String>>, String> =
            Ok(lock(&self.shared.pre_tool_checks).clone());
        #[cfg(not(test))]
        let configured = match crate::config::load_live_config() {
            Ok(loaded) if loaded.invalid_sections.iter().any(|name| name == "workers") => {
                Err(loaded
                    .diagnostics
                    .into_iter()
                    .filter(|diagnostic| diagnostic.contains("workers"))
                    .collect::<Vec<_>>()
                    .join("; "))
            }
            Ok(loaded) => Ok(loaded.config.workers.pre_tool_checks),
            Err(diagnostics) => Err(diagnostics.join("; ")),
        };
        match configured {
            Ok(checks) => pre_tool_checks::expand(&checks),
            Err(error) => (
                Vec::new(),
                vec![format!(
                    "the config could not be read, so no pre-tool check runs: {error}"
                )],
            ),
        }
    }

    /// Re-attaches to the broker of a worker a gone server ran, when that
    /// broker still serves it: this server reads its output and writes its
    /// input from now on. Takes the worker's journal lock, and gives it back
    /// when there is nothing to re-attach to (no broker, or it is gone, and
    /// with it the worker).
    ///
    /// It attaches after the last broker seq the store holds, so the broker
    /// sends again the lines the gone server read and did not store, then
    /// those written since. A broker that is gone left its spool: the lines
    /// in it are recorded, and the worker's exit when the spool holds it
    /// (then the lock is let go and `None` returned, as after a re-attach).
    fn reattach(&self, number: u64, owner_lock: File) -> Option<File> {
        #[cfg(unix)]
        return self.reattach_broker(number, owner_lock);
        #[cfg(not(unix))]
        {
            let _ = number;
            Some(owner_lock)
        }
    }

    #[cfg(unix)]
    fn reattach_broker(&self, number: u64, owner_lock: File) -> Option<File> {
        let (worker_id, record, pid, cwd_real, after) = {
            let registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get(&number) else {
                return Some(owner_lock);
            };
            let status = &entry.status;
            match (&status.broker, status.pid) {
                (Some(record), Some(pid)) if !status.is_gone() && entry.live.is_none() => (
                    status.worker_id.clone(),
                    record.clone(),
                    pid,
                    PathBuf::from(&status.cwd),
                    status.broker_seq,
                ),
                (Some(record), _) if status.is_gone() => {
                    // Left by a server that stored the exit and ended
                    // before it removed the spool.
                    let _ = std::fs::remove_file(broker::spool_path(&record.socket));
                    return Some(owner_lock);
                }
                _ => return Some(owner_lock),
            }
        };
        let link = match broker::connect(&record.socket, after) {
            Ok(link) if link.pid == pid => link,
            Ok(link) => {
                warn!(
                    worker_id,
                    pid,
                    broker_pid = link.pid,
                    "worker broker serves another process"
                );
                return Some(owner_lock);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                // A broker of another build: its worker runs on, but this
                // server cannot read it, nor prove what it missed.
                warn!(worker_id, %error, "cannot re-attach to the worker broker");
                self.record_continuity_gap(number, &error.to_string());
                return Some(owner_lock);
            }
            Err(error) => {
                info!(worker_id, %error, "worker broker gone");
                return self.recover_spool(number, &worker_id, &record, owner_lock);
            }
        };
        let gap = broker::attach_gap(link.floor, after);
        let (input, messages) = match link.split() {
            Ok(split) => split,
            Err(error) => {
                warn!(worker_id, %error, "cannot re-attach to the worker broker");
                return Some(owner_lock);
            }
        };
        let temp_dir = temp_dir_path(&self.shared.dir, &worker_id);
        let live = Arc::new(Live::new(number, Input::Broker(input)));
        let reader = Reader {
            supervisor: self.clone(),
            number,
            pid,
            policy: policy::Policy::new(&record.cwd, &cwd_real, &temp_dir),
            checks: pre_tool_checks::Runner::new(record.pre_tool_checks.clone(), &cwd_real),
            temp_dir,
            spool: Some(broker::spool_path(&record.socket)),
            live: Arc::clone(&live),
            owner_lock,
        };
        {
            let mut registry = lock(&self.shared.registry);
            if let Some(entry) = registry.workers.get_mut(&number) {
                entry.live = Some(Arc::clone(&live));
                entry.foreign = false;
            }
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&json!({"type": "reattached", "broker_pid": record.pid})),
            );
            if let Some(gap) = &gap {
                warn!(worker_id, gap, "worker output continuity not proven");
                self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&continuity_gap_event(gap)),
                );
            }
        }
        self.shared.changed.notify_all();
        notify_clients();
        reader.settle_stored_requests();
        if let Err(((reader, _), error)) = crate::thread_spawn::spawn_named_with(
            "herdr-worker",
            (reader, messages),
            |(reader, messages)| reader.run_broker(messages, None),
        ) {
            warn!(worker_id, %error, "worker reader unavailable");
            if let Some(entry) = lock(&self.shared.registry).workers.get_mut(&number) {
                entry.live = None;
            }
            return Some(reader.owner_lock);
        }
        self.resend_answers(number, &live);
        self.watch_held_output(number, pid);
        None
    }

    /// Sends again the answers a gone server stored (`answer_intent`) and
    /// did not confirm sent: the broker writes each to the worker unless it
    /// already did ([`input_id`]), so the worker reads it once either way,
    /// and `answer_sent` settles the question; the `input_written` event
    /// says which happened. An answer whose response is not stored (an
    /// intent from before slice 4) cannot be sent again: the worker is
    /// marked degraded, as it may or may not have received it.
    #[cfg(unix)]
    fn resend_answers(&self, number: u64, live: &Live) {
        let worker_id = format!("w{number}");
        let answering: Vec<String> = lock(&self.shared.registry)
            .workers
            .get(&number)
            .map(|entry| {
                entry
                    .status
                    .questions
                    .iter()
                    .filter(|pending| pending.answering)
                    .map(|pending| pending.question.request_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut unknown = Vec::new();
        for request_id in answering {
            let response = match &self.shared.store {
                Ok(store) => store.answer_response(&worker_id, &request_id),
                Err(_) => Ok(None),
            };
            let response = match response {
                Ok(Some(response)) => response,
                Ok(None) => {
                    unknown.push(request_id);
                    continue;
                }
                Err(error) => {
                    warn!(worker_id, request_id, %error, "stored answer unreadable");
                    unknown.push(request_id);
                    continue;
                }
            };
            let answer = control_response(&request_id, response);
            let outcome = match live.resend(&answer, &input_id(&answer, None, 0)) {
                Ok(()) => json!({"type": "answer_sent", "request_id": request_id, "resent": true}),
                Err(error) => json!({
                    "type": "answer_failed",
                    "request_id": request_id,
                    "error": error.to_string(),
                }),
            };
            self.record(number, Direction::Herdr, &outcome);
        }
        if !unknown.is_empty() {
            if let Some(entry) = lock(&self.shared.registry).workers.get_mut(&number) {
                entry.status.degraded = Some(format!(
                    "the answer to {} was not confirmed sent before a server restart; the \
                     worker may or may not have received it",
                    unknown.join(", ")
                ));
            }
        }
    }

    /// Records what the spool of a worker whose broker is gone holds past
    /// the store's last broker seq; when that ends with the worker's exit,
    /// records it as the reader would have and lets go of `owner_lock`
    /// (`None`), else gives the lock back for the worker to be marked lost.
    #[cfg(unix)]
    fn recover_spool(
        &self,
        number: u64,
        worker_id: &str,
        record: &BrokerRecord,
        owner_lock: File,
    ) -> Option<File> {
        let spool = broker::spool_path(&record.socket);
        // This server holds the journal lock: its writes are the worker's
        // record now.
        if let Some(entry) = lock(&self.shared.registry).workers.get_mut(&number) {
            entry.foreign = false;
        }
        let Some((seq, exited)) = self.replay_spool(number, &spool) else {
            return Some(owner_lock);
        };
        remove_temp_dir(&temp_dir_path(&self.shared.dir, worker_id));
        let spooled = self.record_spooled(
            number,
            seq,
            Direction::Herdr,
            store::Recorded::Event(&exited),
        );
        if spooled.is_stored() {
            let _ = std::fs::remove_file(&spool);
        }
        drop(owner_lock);
        None
    }

    /// Records the lines in a worker's spool after the store's last broker
    /// seq, for a broker that is gone, and returns the worker's exit with its
    /// seq when the spool holds it. The worker is gone too, so a permission
    /// request among them is recorded, not answered.
    #[cfg(unix)]
    fn replay_spool(&self, number: u64, spool: &Path) -> Option<(u64, Value)> {
        let after = lock(&self.shared.registry)
            .workers
            .get(&number)?
            .status
            .broker_seq;
        let records = match broker::read_spool(spool, after) {
            Ok(read) => {
                if let Some(gap) = &read.gap {
                    self.record_continuity_gap(number, gap);
                }
                read.records
            }
            Err(error) => {
                let gap = if error.kind() == std::io::ErrorKind::NotFound {
                    "the worker's broker is gone and left no output spool".to_owned()
                } else {
                    format!(
                        "the worker's broker is gone and its output spool cannot be read: {error}"
                    )
                };
                self.record_continuity_gap(number, &gap);
                return None;
            }
        };
        for (seq, message) in records {
            match message {
                broker::Message::Out(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&line) {
                        Ok(event) => self.record_spooled(
                            number,
                            seq,
                            Direction::Out,
                            store::Recorded::Event(&event),
                        ),
                        Err(_) => self.record_spooled(
                            number,
                            seq,
                            Direction::Out,
                            store::Recorded::Raw(&line),
                        ),
                    };
                }
                broker::Message::Err(line) => {
                    self.record_spooled(number, seq, Direction::Err, store::Recorded::Raw(&line));
                }
                broker::Message::Lost(lines) => {
                    self.record_spooled(
                        number,
                        seq,
                        Direction::Herdr,
                        store::Recorded::Event(&output_lost(lines)),
                    );
                }
                broker::Message::Written { id, again } => {
                    self.record_spooled(
                        number,
                        seq,
                        Direction::Herdr,
                        store::Recorded::Event(&input_written(&id, again)),
                    );
                }
                broker::Message::Exit(exited) => return Some((seq, exited)),
            }
        }
        None
    }

    /// Records that the worker's stored record is not proven to hold every
    /// line it wrote, and why ([`Status::continuity_gap`]).
    #[cfg(unix)]
    fn record_continuity_gap(&self, number: u64, gap: &str) {
        warn!(
            worker_id = format!("w{number}"),
            gap, "worker output continuity not proven"
        );
        self.record(number, Direction::Herdr, &continuity_gap_event(gap));
    }

    /// Records one line of a worker's output that its broker numbered
    /// `broker_seq`, unless the store already holds it (a line the broker
    /// sends again after a re-attach): the seq goes into the store in the
    /// line's event's transaction, so the two never disagree.
    fn record_spooled(
        &self,
        number: u64,
        broker_seq: u64,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> Spooled {
        let committed = {
            let mut registry = lock(&self.shared.registry);
            match registry.workers.get_mut(&number) {
                None => return Spooled::NotStored,
                Some(entry) if entry.status.broker_seq >= broker_seq => return Spooled::Duplicate,
                Some(entry) => entry.status.broker_seq = broker_seq,
            }
            self.commit_command_locked(&mut registry, number, direction, record, None)
        };
        self.shared.changed.notify_all();
        if committed.shown_changed {
            notify_clients();
        }
        if committed.stored {
            Spooled::Stored
        } else {
            Spooled::NotStored
        }
    }

    /// Records one event of a worker and wakes those waiting on it.
    fn record(&self, number: u64, direction: Direction, event: &Value) {
        self.record_any(number, direction, store::Recorded::Event(event), None);
    }

    /// Records one event of a client command ([`Self::commit_command_locked`]).
    fn record_command(
        &self,
        number: u64,
        direction: Direction,
        event: &Value,
        receipt: Option<&Receipt>,
    ) {
        self.record_any(number, direction, store::Recorded::Event(event), receipt);
    }

    /// Records a line the CLI wrote that is not a JSON event.
    fn record_raw(&self, number: u64, direction: Direction, line: &str) {
        self.record_any(number, direction, store::Recorded::Raw(line), None);
    }

    fn record_any(
        &self,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
        receipt: Option<&Receipt>,
    ) {
        let committed = {
            let mut registry = lock(&self.shared.registry);
            self.commit_command_locked(&mut registry, number, direction, record, receipt)
        };
        self.shared.changed.notify_all();
        if committed.shown_changed {
            notify_clients();
        }
    }

    /// The one way an event enters a worker's record, under the registry
    /// lock, so the recorded order is the order it is folded in: the event
    /// is folded into the worker's status, then appended to the store with
    /// the projections it changes in one transaction (which gives it its
    /// `seq`), then exported to the JSONL journal. A failed write is not
    /// swallowed: the status keeps the event (it happened) and is marked
    /// degraded with the error. The caller wakes the waiters.
    ///
    /// The event's `seq` in memory is the store's, which only grows; after a
    /// failed write it counts on from the worker's last one instead, so a
    /// wait still sees the event as new, and a later stored event still
    /// counts higher than that.
    fn commit_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> Committed {
        self.commit_command_locked(registry, number, direction, record, None)
    }

    /// [`Self::commit_locked`] for an event of a client command: its
    /// `receipt`, if not reserved yet, is reserved in the same transaction
    /// as the event.
    fn commit_command_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
        receipt: Option<&Receipt>,
    ) -> Committed {
        let receipt = receipt.filter(|receipt| !receipt.reserved.get());
        let Some(entry) = registry.workers.get_mut(&number) else {
            return Committed {
                shown_changed: false,
                seq: None,
                stored: false,
            };
        };
        let shown_before = Self::shown(&entry.status);
        let before = entry.status.before();
        if let store::Recorded::Event(event) = record {
            entry.status.apply(direction, event);
        }
        let ts_ms = now_ms();
        let status = &mut entry.status;
        let foreign = entry.foreign;
        let written = match &self.shared.store {
            Ok(store) => store
                .transaction(|tx| {
                    let seq = tx.event(&store::EventRow {
                        worker_id: &status.worker_id,
                        direction,
                        record: &record,
                        ts_ms,
                    })?;
                    status.mark_seq(
                        seq.max(status.last_seq + 1),
                        ts_ms,
                        &before,
                        direction,
                        &record,
                    );
                    if !foreign {
                        tx.questions(seq, &before.pending, status)?;
                        tx.worker(status, seq)?;
                        inbox::write(tx, seq, &before, status, direction, &record)?;
                    }
                    if let Some(receipt) = receipt {
                        tx.reserve_receipt(&receipt.row(), &status.worker_id, ts_ms)?;
                    }
                    Ok(seq)
                })
                .map_err(|error| format!("a worker store write failed: {error}")),
            Err(error) => Err(error.clone()),
        };
        let seq = match written {
            Ok(seq) => {
                if let Some(receipt) = receipt {
                    receipt.reserved.set(true);
                }
                Some(seq)
            }
            Err(error) => {
                warn!(%error, worker_id = status.worker_id, "worker event not stored");
                status.degraded = Some(error);
                // Marked again past whatever a failed commit marked.
                status.mark_seq(status.last_seq + 1, ts_ms, &before, direction, &record);
                if !foreign {
                    // Its inbox rows are lost with it: their owners resync.
                    registry
                        .inbox_losses
                        .record(inbox::missed_by(&before, status, direction, &record));
                }
                None
            }
        };
        let in_memory = status.last_seq;
        let export = match &entry.export {
            Some(journal) => Ok(Arc::clone(journal)),
            None => Journal::open(&entry.journal_path).map(Arc::new),
        };
        let exported = export.and_then(|journal| {
            entry.export = Some(Arc::clone(&journal));
            journal.export(seq, ts_ms, direction, record)
        });
        if let Err(error) = exported {
            warn!(%error, worker_id = entry.status.worker_id, "worker journal write failed");
            entry.status.degraded = Some(format!("a worker journal write failed: {error}"));
        }
        let mut shown_changed = Self::shown(&entry.status) != shown_before;
        // A question asked while its owner cannot handle one goes to the
        // user at once, as one asked before would have when the owner got
        // stuck.
        let asked = matches!(record, store::Recorded::Event(event)
            if direction == Direction::Herdr && event["type"].as_str() == Some("question"));
        let stuck = entry
            .status
            .owner_pane
            .as_ref()
            .and_then(|pane| registry.stuck_owners.get(pane))
            .cloned();
        if let (true, Some(cause)) = (asked, stuck) {
            if let Some(escalated) = self.escalate_locked(registry, number, &cause) {
                shown_changed |= escalated.shown_changed;
            }
        }
        Committed {
            shown_changed,
            seq: Some(in_memory),
            stored: seq.is_some(),
        }
    }

    /// Writes `moved` (an `owner_moved` event) in `tx` for every worker
    /// tenure `from` owns that its owner still has to handle (running, or
    /// ended and not acknowledged), on copies of their statuses: the
    /// caller puts them in with [`Self::settle_moves`] once `tx` commits.
    fn stage_owner_moves(
        registry: &Registry,
        tx: &store::Tx<'_>,
        from: &str,
        moved: &Value,
        ts_ms: u64,
    ) -> store::StoreResult<StagedMoves> {
        let mut staged = Vec::new();
        for (number, entry) in &registry.workers {
            let status = &entry.status;
            if status.owner_coordinator.as_deref() != Some(from) || !status.listed() {
                continue;
            }
            let mut status = status.clone();
            let before = status.before();
            status.apply(Direction::Herdr, moved);
            let record = store::Recorded::Event(moved);
            let seq = tx.event(&store::EventRow {
                worker_id: &status.worker_id,
                direction: Direction::Herdr,
                record: &record,
                ts_ms,
            })?;
            status.mark_seq(
                seq.max(status.last_seq + 1),
                ts_ms,
                &before,
                Direction::Herdr,
                &record,
            );
            if !entry.foreign {
                tx.questions(seq, &before.pending, &status)?;
                tx.worker(&status, seq)?;
                inbox::write(tx, seq, &before, &status, Direction::Herdr, &record)?;
            }
            staged.push((*number, status, seq));
        }
        Ok(staged)
    }

    /// Puts in the statuses [`Self::stage_owner_moves`] wrote, once their
    /// transaction committed, and exports their events.
    fn settle_moves(registry: &mut Registry, staged: StagedMoves, moved: &Value, ts_ms: u64) {
        for (number, status, seq) in staged {
            let Some(entry) = registry.workers.get_mut(&number) else {
                continue;
            };
            entry.status = status;
            let export = match &entry.export {
                Some(journal) => Ok(Arc::clone(journal)),
                None => Journal::open(&entry.journal_path).map(Arc::new),
            };
            let exported = export.and_then(|journal| {
                entry.export = Some(Arc::clone(&journal));
                journal.export(
                    Some(seq),
                    ts_ms,
                    Direction::Herdr,
                    store::Recorded::Event(moved),
                )
            });
            if let Err(error) = exported {
                warn!(%error, worker_id = entry.status.worker_id, "worker journal write failed");
                entry.status.degraded = Some(format!("a worker journal write failed: {error}"));
            }
        }
    }

    /// Hands every question of worker `number` that waits quietly for its
    /// owner to the user, as one `escalated` event naming them and `cause`.
    /// Records nothing when none waits quietly.
    fn escalate_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        cause: &str,
    ) -> Option<Committed> {
        let quiet = registry.workers.get(&number)?.status.quiet_questions();
        if quiet.is_empty() {
            return None;
        }
        let escalated = json!({"type": "escalated", "request_ids": quiet, "cause": cause});
        Some(self.commit_locked(
            registry,
            number,
            Direction::Herdr,
            store::Recorded::Event(&escalated),
        ))
    }

    /// The parts of a status the clients show. Questions are only added or
    /// removed, never replaced in place, and only ever go from quiet to
    /// escalated, so the two counts tell a change.
    #[allow(clippy::type_complexity)] // Only compared with itself, never taken apart.
    fn shown(status: &Status) -> (WorkerState, bool, bool, usize, usize, bool, bool, bool) {
        (
            status.state,
            status.is_gone(),
            status.in_sidebar(),
            status.questions.len(),
            status
                .questions
                .iter()
                .filter(|pending| status.is_quiet(pending))
                .count(),
            status.session_id.is_some(),
            status.takeover_ms.is_some(),
            status.degraded.is_some(),
        )
    }

    fn summaries(&self) -> Vec<WorkerSummary> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .map(|entry| WorkerSummary {
                worker_id: entry.status.worker_id.clone(),
                workspace_id: entry.status.workspace_id.clone(),
                name: entry.status.name.clone(),
                cwd: entry.status.cwd.clone(),
                state: entry.status.state,
                session_id: entry.status.session_id.clone(),
                takeover: entry.status.takeover_ms.is_some() && !entry.status.takeover_unfinished,
                item: entry.status.item.clone(),
                item_title: entry.status.item_title.clone(),
                slot: entry.status.slot.clone(),
                branch: entry.status.branch.clone(),
                turns: entry.status.turns,
                started_ms: entry.status.started_ms,
                turn_started_ms: entry
                    .status
                    .turn_started_ms
                    .filter(|_| !entry.status.turn_ended()),
                last_event_ms: entry.status.last_event_ms,
                listed: entry.status.in_sidebar(),
                gone: entry.status.is_gone(),
                end_note: entry.status.end_note.clone(),
                quiet: entry.status.settled(),
                owner_pane: entry.status.owner_pane.clone(),
            })
            .collect()
    }

    /// [`ItemCounts`] of the items of repository `repo`.
    fn item_counts(&self, repo: &str) -> ItemCounts {
        let registry = lock(&self.shared.registry);
        let mut in_progress = std::collections::BTreeSet::new();
        let mut attention = std::collections::BTreeSet::new();
        for entry in registry.workers.values() {
            let status = &entry.status;
            let Some(item) = &status.item else {
                continue;
            };
            if status.repo.as_deref() != Some(repo) {
                continue;
            }
            let key = item.as_str();
            if !status.is_gone() {
                in_progress.insert(key);
            } else if status.end_unacked() {
                attention.insert(key);
            }
        }
        ItemCounts {
            in_progress: in_progress.len().try_into().unwrap_or(u32::MAX),
            attention: attention.len().try_into().unwrap_or(u32::MAX),
        }
    }

    fn pending_questions(&self) -> Vec<PendingWorkerQuestion> {
        let registry = lock(&self.shared.registry);
        let mut pending: Vec<PendingWorkerQuestion> = registry
            .workers
            .values()
            .flat_map(|entry| {
                entry
                    .status
                    .questions
                    .iter()
                    .filter(|pending| !pending.answering)
                    .map(|pending| PendingWorkerQuestion {
                        worker_id: entry.status.worker_id.clone(),
                        cwd: entry.status.cwd.clone(),
                        question: pending.shown(),
                        quiet: entry.status.is_quiet(pending),
                        owner_seen_ms: entry
                            .status
                            .owner_pane
                            .as_ref()
                            .and_then(|pane| registry.owner_seen_ms.get(pane))
                            .copied(),
                    })
            })
            .collect();
        // A todo run's review escalations, listed as its worker's.
        pending.extend(runs::escalations::pending_questions());
        // The definitions waiting for the user's grant.
        pending.extend(grant_questions::pending_questions());
        pending
    }

    pub(crate) fn status(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        self.with_entry(worker_id, |entry| entry.status.info(&entry.journal_path))
    }

    /// Every worker's run grouped by its item and repository, the group
    /// whose first run started first first, and the runs without an item.
    pub(crate) fn runs(
        &self,
        params: &WorkerRunsParams,
    ) -> Result<(Vec<WorkerItemRuns>, Vec<WorkerRun>), WorkerError> {
        if let Some(item) = &params.item {
            check_item_id(item)?;
        }
        // Any directory in the repository names it.
        let repo = params
            .repo
            .as_ref()
            .map(|dir| repository_of(Path::new(dir)).unwrap_or_else(|| dir.clone()));
        let registry = lock(&self.shared.registry);
        let mut items: Vec<WorkerItemRuns> = Vec::new();
        let mut unassigned = Vec::new();
        // The registry is ordered by worker number, which is the start order.
        for entry in registry.workers.values() {
            let status = &entry.status;
            if repo.is_some() && status.repo != repo {
                continue;
            }
            if params.item.is_some() && status.item != params.item {
                continue;
            }
            let run = status.run(&entry.journal_path);
            let Some(item) = &status.item else {
                unassigned.push(run);
                continue;
            };
            match items
                .iter_mut()
                .find(|group| group.item == *item && group.repo == status.repo)
            {
                Some(group) => {
                    group.runs.push(run);
                    // The latest run's title: the item may have been renamed.
                    if status.item_title.is_some() {
                        group.title = status.item_title.clone();
                    }
                }
                None => items.push(WorkerItemRuns {
                    item: item.clone(),
                    repo: status.repo.clone(),
                    runs: vec![run],
                    title: status.item_title.clone(),
                }),
            }
        }
        drop(registry);
        // Only items none of whose runs stored a title (workers started
        // before titles were stored) are looked up in TODO.md, after the
        // registry is released: a slow disk must not hold the workers'
        // events.
        let mut titles: HashMap<String, HashMap<String, String>> = HashMap::new();
        for group in &mut items {
            if group.title.is_some() {
                continue;
            }
            let Some(repo) = &group.repo else {
                continue;
            };
            let repo_titles = titles
                .entry(repo.clone())
                .or_insert_with(|| todo_titles::read_titles(Path::new(repo)));
            group.title = repo_titles.get(&group.item).cloned();
        }
        Ok((items, unassigned))
    }

    /// Checks the worker's commit in its directory ([`verify::verify`]) and
    /// records the verdict as its `verification` event, which its run then
    /// shows. Blocks while a command runs, as long as it takes. A second
    /// verification of the same directory while one runs is refused: they
    /// would restore the worktree under each other.
    pub(crate) fn verify(
        &self,
        params: &WorkerVerifyParams,
    ) -> Result<WorkerVerification, WorkerError> {
        self.verify_with(
            params,
            params
                .command
                .as_deref()
                .map(|command| (None, verify::CheckCommand::Shell(command)))
                .into_iter()
                .collect(),
        )
    }

    /// [`Self::verify`] with the command checks given apart, each with its
    /// registered name: `params.command` is not read.
    fn verify_with(
        &self,
        params: &WorkerVerifyParams,
        commands: Vec<(Option<&str>, verify::CheckCommand<'_>)>,
    ) -> Result<WorkerVerification, WorkerError> {
        let (number, status) = {
            let registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, &params.worker_id)?;
            (number, registry.workers[&number].status.clone())
        };
        if status.cwd.is_empty() {
            return Err(WorkerError::Invalid(format!(
                "worker {} has no recorded directory",
                params.worker_id
            )));
        }
        let dir = PathBuf::from(&status.cwd);
        let mut processes = Vec::new();
        if !status.process_gone() {
            processes.push(format!(
                "the worker's process{}; stop it first (herdr worker stop {})",
                status.pid.map(|pid| format!(" {pid}")).unwrap_or_default(),
                params.worker_id
            ));
        }
        let real = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        for session in status.tool_sessions.keys() {
            for pid in crate::platform::session_members(*session) {
                if slot::runs_in_slot(pid, &real) {
                    processes.push(format!("tool process {pid} (session {session})"));
                }
            }
        }
        let _claim = VerifyClaim::take(&real)?;
        let verification = verify::verify(
            &verify::Request {
                dir: &dir,
                base: &params.base,
                expected_message: &params.expected_message,
                allowed_paths: &params.allowed_paths,
                commands,
                generated: &params.generated,
                env: params.env.as_ref(),
                processes,
            },
            now_ms(),
        );
        self.record(
            number,
            Direction::Herdr,
            &json!({"type": "verification", "verification": verification}),
        );
        Ok(verification)
    }

    pub(crate) fn list(&self) -> Vec<WorkerInfo> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect()
    }

    /// Blocks until the worker reaches `until` (`turn_end` or `exit`),
    /// woken by its events ([`Self::wait_on`]). Returns `None` when the
    /// caller gave up.
    pub(crate) fn wait(
        &self,
        worker_id: &str,
        until: WorkerWaitUntil,
        liveness_check: Duration,
        keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<WorkerInfo>, WorkerError> {
        if until == WorkerWaitUntil::Attention {
            return Err(WorkerError::Invalid(
                "until: attention answers with worker_attention".into(),
            ));
        }
        self.wait_on(worker_id, liveness_check, keep_waiting, |entry| {
            let reached = match until {
                WorkerWaitUntil::Exit => entry.status.is_gone(),
                _ => entry.status.turn_ended(),
            };
            reached.then(|| entry.status.info(&entry.journal_path))
        })
    }

    /// Blocks until the worker needs its coordinator: a pending question
    /// asked after `after`, a turn ended after it, or its end (gone counts
    /// however old: nothing follows it). Level-triggered: what is already
    /// there returns at once. The reply's `seq` is the worker's latest
    /// event's; passed back as `after`, the same state does not wake the
    /// caller again. Returns `None` when the caller gave up.
    ///
    /// A shorthand over the inbox (`worker.events` filtered to one worker):
    /// with `after`, what woke it are the worker's inbox rows after it
    /// ([`inbox::attention`]), checked against what is still pending. A
    /// cursor the inbox cannot serve, a worker whose store write failed and
    /// a wait without `after` are answered from the snapshot, the worker's
    /// status, as `worker.events` answers them.
    pub(crate) fn wait_attention(
        &self,
        worker_id: &str,
        after: Option<i64>,
        liveness_check: Duration,
        keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<Attention>, WorkerError> {
        self.wait_on(worker_id, liveness_check, keep_waiting, |entry| {
            let status = &entry.status;
            let from_inbox = match (after, &self.shared.store) {
                (Some(after), Ok(store)) => store
                    .read(|conn| inbox::attention(conn, status, after))
                    .unwrap_or_else(|error| {
                        warn!(%error, worker_id, "the worker inbox could not be read");
                        None
                    }),
                _ => None,
            };
            let reason = match from_inbox {
                Some(reason) => reason,
                None => status.attention(after),
            };
            reason.map(|reason| Attention {
                reason,
                questions: status.questions.iter().map(Pending::shown).collect(),
                seq: status.last_seq,
                worker: status.info(&entry.journal_path),
            })
        })
    }

    /// Where `worker.wait --attention` without `after` starts for a caller
    /// in `caller_pane`: after the `seq` the worker's owner acknowledged,
    /// when that pane owns it (as in [`Self::obligations`]: through its
    /// tenure, else as the starting pane) and has acknowledged anything.
    /// The owner's loop is then wait, handle, ack, the same wait again,
    /// with no cursor of its own; an ack means handled, so a crash before
    /// it wakes the next wait again. Anyone else starts from the snapshot.
    pub(crate) fn owner_cursor(&self, worker_id: &str, caller_pane: &str) -> Option<i64> {
        let tenure = self
            .coordinator_of_pane(caller_pane)
            .map(|tenure| tenure.coordinator_id);
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id).ok()?;
        let status = &registry.workers[&number].status;
        let owns = match &status.owner_coordinator {
            Some(coordinator) => tenure.as_ref() == Some(coordinator),
            None => status.owner_pane.as_deref() == Some(caller_pane),
        };
        (owns && status.acked_seq > 0).then_some(status.acked_seq)
    }

    /// Records that the worker's owner handled its events up to `seq`
    /// (an `acked` event). Idempotent and monotonic: an ack at or below the
    /// acknowledged `seq` records nothing. A `seq` past the worker's latest
    /// event is refused, since it would hide events that have not happened.
    ///
    /// An ack that leaves a live owned worker between turns with nothing
    /// to handle ([`Status::handled_between_turns`]) stops it as its owner:
    /// an ack means handled, so its process and its folder slot go, and
    /// its exit counts as acknowledged. An owner that wants another turn
    /// prompts before it acknowledges the turn's end.
    pub(crate) fn ack(&self, worker_id: &str, seq: i64) -> Result<WorkerInfo, WorkerError> {
        self.acknowledge(worker_id, seq, true)
    }

    /// [`Self::ack`] for herdr's own drivers (a todo run, an item
    /// coordinator's chain), which stop the worker themselves at their own
    /// step: with `stop_handled` false the ack never stops it.
    pub(crate) fn acknowledge(
        &self,
        worker_id: &str,
        seq: i64,
        stop_handled: bool,
    ) -> Result<WorkerInfo, WorkerError> {
        let (committed, stop_as) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let status = &registry.workers[&number].status;
            if seq > status.last_seq {
                return Err(WorkerError::Invalid(format!(
                    "seq {seq} is past worker {worker_id}'s latest event ({})",
                    status.last_seq
                )));
            }
            if let Some(pane) = status.owner_pane.clone() {
                registry.owner_seen_ms.insert(pane, now_ms());
            }
            let status = &registry.workers[&number].status;
            let committed = if seq <= status.acked_seq {
                None
            } else {
                Some(self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&json!({"type": "acked", "seq": seq})),
                ))
            };
            let status = &registry.workers[&number].status;
            let stop_as = (stop_handled && committed.is_some())
                .then_some(status)
                .filter(|status| status.handled_between_turns())
                .and_then(|status| status.owner_pane.clone());
            (committed, stop_as)
        };
        if let Some(committed) = committed {
            self.shared.changed.notify_all();
            if committed.shown_changed {
                notify_clients();
            }
        }
        if let Some(owner) = stop_as {
            match self.stop_once(worker_id, Some(&owner), None) {
                Ok(_) | Err(WorkerError::NotRunning(_)) => {}
                Err(error) => warn!(%error, worker_id, "cannot stop the acknowledged worker"),
            }
        }
        self.status(worker_id)
    }

    /// The owned workers with an event their owner has not acknowledged
    /// ([`Status::obligation`]), only those `owner_pane` owns when given.
    /// A worker started by a coordinator is owed by its tenure, not by a
    /// pane: it belongs to the pane its tenure is bound to now, so a
    /// coordinator resumed elsewhere keeps it and another agent in its old
    /// pane does not get it. Derived from the workers' state on every call:
    /// nothing to deliver, nothing to lose.
    pub(crate) fn obligations(&self, owner_pane: Option<&str>) -> Vec<WorkerObligation> {
        let tenure = owner_pane
            .and_then(|pane| self.coordinator_of_pane(pane))
            .map(|tenure| tenure.coordinator_id);
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .filter_map(|entry| {
                let status = &entry.status;
                let owner = status.owner_pane.as_deref()?;
                let owns = |pane: &str| match &status.owner_coordinator {
                    Some(coordinator) => tenure.as_ref() == Some(coordinator),
                    None => pane == owner,
                };
                if owner_pane.is_some_and(|pane| !owns(pane)) {
                    return None;
                }
                let (reason, questions) = status.obligation()?;
                Some(WorkerObligation {
                    worker_id: status.worker_id.clone(),
                    name: status.name.clone(),
                    reason,
                    seq: status.last_seq,
                    questions,
                    owner_pane_id: owner.to_owned(),
                    owner_session_id: status.owner_session.clone(),
                })
            })
            .collect()
    }

    /// The owner hands one pending question to the user (`worker.escalate`).
    /// Idempotent: a question already escalated, or of a worker without an
    /// owner (which asks the user anyway), records nothing.
    pub(crate) fn escalate(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerInfo, WorkerError> {
        if runs::escalations::is_escalation(request_id) {
            // A review escalation is the user's already.
            return self.escalation_reply(worker_id);
        }
        if grant_questions::is_grant_question(request_id) {
            // So is a grant question.
            return self.grant_reply_of(worker_id);
        }
        self.escalate_because(worker_id, request_id, "its coordinator escalated it")
    }

    /// [`Self::escalate`] with the cause the user's `?` list shows: a todo
    /// run's automatic answer names its run, its item and the model's
    /// question with its options.
    pub(crate) fn escalate_because(
        &self,
        worker_id: &str,
        request_id: &str,
        cause: &str,
    ) -> Result<WorkerInfo, WorkerError> {
        let committed = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let status = &registry.workers[&number].status;
            if !status
                .questions
                .iter()
                .any(|pending| pending.question.request_id == request_id)
            {
                return Err(match status.resolution(request_id) {
                    Some(how) => WorkerError::QuestionGone(format!(
                        "question {request_id} of worker {worker_id} is no longer pending: {how}"
                    )),
                    None => WorkerError::NoQuestion(format!(
                        "worker {worker_id} has no question {request_id}"
                    )),
                });
            }
            if let Some(pane) = status.owner_pane.clone() {
                registry.owner_seen_ms.insert(pane, now_ms());
            }
            let status = &registry.workers[&number].status;
            let pending = status
                .questions
                .iter()
                .find(|pending| pending.question.request_id == request_id);
            if pending.is_some_and(|pending| status.is_quiet(pending)) {
                let escalated = json!({
                    "type": "escalated",
                    "request_ids": [request_id],
                    "cause": cause,
                });
                Some(self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&escalated),
                ))
            } else {
                None
            }
        };
        if let Some(committed) = committed {
            self.shared.changed.notify_all();
            if committed.shown_changed {
                notify_clients();
            }
        }
        self.status(worker_id)
    }

    /// One question the worker waits on, with the tool's whole input, for an
    /// answer dialog. One whose answer is being sent or that has ended is
    /// refused with `worker_question_gone`, saying how it ended.
    pub(crate) fn question_detail(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerQuestionDetail, WorkerError> {
        if runs::escalations::is_escalation(request_id) {
            return self.escalation_detail(worker_id, request_id);
        }
        if grant_questions::is_grant_question(request_id) {
            return self.grant_question_detail(worker_id, request_id);
        }
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        let status = &registry.workers[&number].status;
        let Some(pending) = status
            .questions
            .iter()
            .find(|pending| pending.question.request_id == request_id)
        else {
            return Err(match status.resolution(request_id) {
                Some(how) => WorkerError::QuestionGone(format!(
                    "question {request_id} of worker {worker_id} is no longer pending: {how}"
                )),
                None => WorkerError::NoQuestion(format!(
                    "worker {worker_id} has no question {request_id}"
                )),
            });
        };
        if pending.answering {
            return Err(WorkerError::QuestionGone(format!(
                "question {request_id} of worker {worker_id} is no longer pending: its answer \
                 is being sent"
            )));
        }
        Ok(WorkerQuestionDetail {
            worker_id: status.worker_id.clone(),
            name: status.name.clone(),
            cwd: status.cwd.clone(),
            state: status.state,
            question: pending.shown(),
            input_text: full_input_text(&pending.input),
            owner_pane_id: status.owner_pane.clone(),
            owner_coordinator_id: status.owner_coordinator.clone(),
            quiet: status.is_quiet(pending),
        })
    }

    /// Denies the question `request_id` names, then stops the worker. A
    /// question that is no longer pending (answered or ended meanwhile)
    /// does not keep the worker running: the user asked to end it.
    pub(crate) fn deny_and_stop(
        &self,
        params: &WorkerDenyAndStopParams,
    ) -> Result<WorkerInfo, WorkerError> {
        // A grant question is answered only from the user's client
        // ([`Self::deny_and_stop_from_client`]).
        if grant_questions::is_grant_question(&params.request_id) {
            return Err(grant_questions::needs_user(&params.request_id));
        }
        // A review escalation's worker has already exited: the denial
        // leaves the review to the coordinator.
        if runs::escalations::is_escalation(&params.request_id) {
            self.answer_escalation(
                &params.worker_id,
                &params.request_id,
                Some(WorkerDecision::Deny),
                &[],
                params.message.as_deref(),
            )?;
            return self.escalation_reply(&params.worker_id);
        }
        let deny = WorkerAnswerParams {
            worker_id: params.worker_id.clone(),
            request_id: Some(params.request_id.clone()),
            decision: Some(WorkerDecision::Deny),
            answers: Vec::new(),
            message: Some(
                params
                    .message
                    .clone()
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or_else(|| "The user stopped this worker.".to_owned()),
            ),
            command_id: None,
        };
        match self.answer(&deny) {
            Ok(_) | Err(WorkerError::QuestionGone(_) | WorkerError::NoQuestion(_)) => {}
            Err(error) => return Err(error),
        }
        self.stop(&params.worker_id)
    }

    /// The owner panes of the workers that have not ended and whose owner is
    /// not gone, the panes of the active coordination tenures and the
    /// owner panes of the `todo.run`s not ended whose owner is not gone:
    /// those [`Self::owner_event`] can still act on.
    fn owner_panes(&self) -> Vec<String> {
        let tenure_panes = self.coordinator_panes().unwrap_or_else(|error| {
            warn!(%error, "cannot read the coordination tenures' panes");
            Vec::new()
        });
        let run_panes = self.run_owner_panes();
        let registry = lock(&self.shared.registry);
        let run_panes = run_panes
            .into_iter()
            .filter(|pane| !registry.gone_owners.contains_key(pane));
        let mut panes: Vec<String> = registry
            .workers
            .values()
            .filter(|entry| !entry.status.is_gone() && entry.status.owner_gone.is_none())
            .filter_map(|entry| entry.status.owner_pane.clone())
            .chain(tenure_panes)
            .chain(run_panes)
            .collect();
        panes.sort();
        panes.dedup();
        panes
    }

    /// Why owner pane `pane_id` is gone (its pane closed or its agent
    /// exited), as herdr's events reported it; `None` while it is there.
    pub(super) fn gone_owner(&self, pane_id: &str) -> Option<String> {
        lock(&self.shared.registry)
            .gone_owners
            .get(pane_id)
            .cloned()
    }

    /// [`owners_at_start`] for this supervisor.
    fn owners_at_start(&self, state: impl Fn(&str) -> Option<OwnerEvent>) {
        for pane in self.owner_panes() {
            if let Some(event) = state(&pane) {
                self.owner_event(&pane, event, " (found when herdr started)");
            }
        }
    }

    /// What the agent in owner pane `pane_id` just did, as herdr's events
    /// report it, and what that means for its workers' questions that wait
    /// quietly for it ([`OwnerEvent`]). `cause_suffix` is added to each
    /// escalation's cause (" (found when herdr started)").
    ///
    /// Escalation follows events only, never a timer (the user's decision,
    /// 2026-10-08). Known limitation: an owner that is alive but hung, its
    /// agent reported `working` for ever with no event, keeps its questions
    /// quiet; the `?` list shows them as awaiting the coordinator with the
    /// age of the owner's last event, so the user can see it and act.
    pub(crate) fn owner_event(&self, pane_id: &str, event: OwnerEvent, cause_suffix: &str) {
        // The pane's coordination tenure ends with its pane or agent.
        if let (OwnerEvent::PaneClosed | OwnerEvent::AgentExited, Some(cause)) =
            (event, event.cause())
        {
            self.orphan_coordinator(pane_id, &format!("{cause}{cause_suffix}"));
        }
        let mut any = false;
        {
            let mut registry = lock(&self.shared.registry);
            // Also for an owner of no worker now: a run it owns may start
            // its next attempt's worker later, or be resumed elsewhere.
            match (event, event.cause()) {
                (OwnerEvent::PaneClosed | OwnerEvent::AgentExited, Some(cause)) => {
                    registry
                        .gone_owners
                        .insert(pane_id.to_owned(), format!("{cause}{cause_suffix}"));
                }
                // An agent works in the pane again: it is there.
                (OwnerEvent::Working, _) => {
                    registry.gone_owners.remove(pane_id);
                }
                _ => {}
            }
            let owned: Vec<u64> = registry
                .workers
                .iter()
                .filter(|(_, entry)| {
                    !entry.status.is_gone()
                        && entry.status.owner_pane.as_deref() == Some(pane_id)
                        && entry.status.owner_gone.is_none()
                })
                .map(|(number, _)| *number)
                .collect();
            if owned.is_empty() {
                return;
            }
            registry.owner_seen_ms.insert(pane_id.to_owned(), now_ms());
            let cause = event.cause().map(|cause| format!("{cause}{cause_suffix}"));
            match event {
                OwnerEvent::PaneClosed | OwnerEvent::AgentExited => {
                    registry.stuck_owners.remove(pane_id);
                }
                OwnerEvent::Limited | OwnerEvent::Blocked => {
                    if let Some(cause) = &cause {
                        registry
                            .stuck_owners
                            .insert(pane_id.to_owned(), cause.clone());
                    }
                }
                OwnerEvent::TurnEnded => {}
                OwnerEvent::Working => {
                    registry.stuck_owners.remove(pane_id);
                }
            }
            for number in owned {
                let committed = match (&event, &cause) {
                    (OwnerEvent::PaneClosed | OwnerEvent::AgentExited, Some(cause)) => {
                        let gone = json!({"type": "owner_gone", "cause": cause});
                        Some(self.commit_locked(
                            &mut registry,
                            number,
                            Direction::Herdr,
                            store::Recorded::Event(&gone),
                        ))
                    }
                    (_, Some(cause)) => self.escalate_locked(&mut registry, number, cause),
                    (_, None) => None,
                };
                any |= committed.is_some();
            }
        }
        if any {
            self.shared.changed.notify_all();
        }
        // Also when nothing escalated: a quiet entry shows the owner's last
        // event's age.
        notify_clients();
    }

    /// Blocks until `reached` returns something for the worker, woken by
    /// its events: every commit notifies `changed`. Subscribe before check:
    /// the check and the `seq` it saw are read in one hold of the registry
    /// lock; the lock is let go only to run `keep_waiting`, and an event
    /// committed meanwhile has a higher `seq`, so it is checked before the
    /// wait blocks rather than missed. `keep_waiting` runs at least every
    /// `liveness_check`, only so that a caller whose client went away or
    /// whose server stops can give up; that interval never decides the
    /// outcome. Returns `None` when the caller gave up.
    fn wait_on<T>(
        &self,
        worker_id: &str,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
        mut reached: impl FnMut(&Entry) -> Option<T>,
    ) -> Result<Option<T>, WorkerError> {
        let missing = || WorkerError::NotFound(worker_id.to_owned());
        let mut registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        loop {
            let entry = registry.workers.get(&number).ok_or_else(missing)?;
            if let Some(value) = reached(entry) {
                return Ok(Some(value));
            }
            let seen = entry.status.last_seq;
            let handed_off = self
                .shared
                .handed_off
                .load(std::sync::atomic::Ordering::SeqCst);
            drop(registry);
            if !keep_waiting() {
                return Ok(None);
            }
            registry = lock(&self.shared.registry);
            let entry = registry.workers.get(&number).ok_or_else(missing)?;
            // A handoff between `keep_waiting` and here (it is set under
            // this lock) asks `keep_waiting` again instead of being lost.
            if entry.status.last_seq != seen
                || self
                    .shared
                    .handed_off
                    .load(std::sync::atomic::Ordering::SeqCst)
                    != handed_off
            {
                continue;
            }
            registry = self
                .shared
                .changed
                .wait_timeout(registry, liveness_check)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    fn live(&self, worker_id: &str) -> Result<(u64, Arc<Live>, Status), WorkerError> {
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        let entry = &registry.workers[&number];
        match (&entry.live, entry.status.is_gone()) {
            (Some(live), false) => Ok((number, Arc::clone(live), entry.status.clone())),
            _ => Err(WorkerError::NotRunning(format!(
                "worker {worker_id} is not running"
            ))),
        }
    }

    /// [`Self::prompt_command`] without a command id.
    #[cfg(all(test, unix))]
    pub(crate) fn prompt(&self, worker_id: &str, text: &str) -> Result<WorkerInfo, WorkerError> {
        self.prompt_command(&WorkerPromptParams {
            worker_id: worker_id.to_owned(),
            text: text.to_owned(),
            command_id: None,
        })
    }

    /// Sends the next user message. Accepted only between turns, checked in
    /// the same hold as its record, so a `worker.wait` after it waits for
    /// this turn's end and two prompts never both start one. The reply's
    /// `turn_seq` is the `seq` of the message it appended.
    pub(crate) fn prompt_command(
        &self,
        params: &WorkerPromptParams,
    ) -> Result<WorkerInfo, WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.prompt",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.prompt_once(&params.worker_id, &params.text, receipt),
        )
    }

    fn prompt_once(
        &self,
        worker_id: &str,
        text: &str,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        if text.trim().is_empty() {
            return Err(WorkerError::Invalid("text must not be empty".into()));
        }
        let (_, live, _) = self.live(worker_id)?;
        let turn_seq =
            live.send_checked(self, &user_message(text), receipt, |status, registry| {
                if status.takeover_ms.is_some() {
                    return Err(Self::taken_over(worker_id));
                }
                if let Some(gap) = &status.continuity_gap {
                    return Err(Self::continuity_gap(worker_id, gap));
                }
                if status.is_gone() {
                    return Err(WorkerError::NotRunning(format!(
                        "worker {worker_id} is not running"
                    )));
                }
                if !status.turn_ended() {
                    return Err(WorkerError::Busy(format!(
                        "worker {worker_id} is in a turn; wait for it or interrupt it first"
                    )));
                }
                Self::admits_turns(registry)
            })?;
        let mut info = self.status(worker_id)?;
        info.turn_seq = Some(turn_seq);
        Ok(info)
    }

    /// Asks the CLI to interrupt its turn (a control request). The turn
    /// then ends with a `result` whose `terminal_reason` is `aborted_*`.
    /// With `turn`, only that turn: once it has ended the interrupt is
    /// refused with `worker_turn_ended` and nothing is sent, so a repeated
    /// interrupt never aborts the next turn. The reply's `turn_seq` names
    /// the turn it interrupted (none when no turn ran).
    pub(crate) fn interrupt(
        &self,
        params: &WorkerInterruptParams,
    ) -> Result<WorkerInfo, WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.interrupt",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.interrupt_once(params, receipt),
        )
    }

    fn interrupt_once(
        &self,
        params: &WorkerInterruptParams,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        let worker_id = params.worker_id.as_str();
        let (number, live, _) = self.live(worker_id)?;
        let mut interrupted = None;
        let request = json!({
            "type": "control_request",
            "request_id": format!("herdr-interrupt-{number}-{}", now_ms()),
            "request": {"subtype": "interrupt"},
        });
        live.send_checked(self, &request, receipt, |status, _| {
            let running = (!status.turn_ended()).then_some(status.turn_seq).flatten();
            if let Some(turn) = params.turn {
                match status.turn_seq {
                    _ if running == Some(turn) => {}
                    Some(last) if turn <= last => {
                        return Err(WorkerError::TurnEnded(format!(
                            "turn {turn} of worker {worker_id} has already ended; nothing was sent"
                        )))
                    }
                    _ => {
                        return Err(WorkerError::Invalid(format!(
                            "worker {worker_id} has no turn {turn}"
                        )))
                    }
                }
            }
            interrupted = running;
            Ok(())
        })?;
        let mut info = self.status(worker_id)?;
        info.turn_seq = interrupted;
        Ok(info)
    }

    /// Closes the worker's input, sends SIGTERM to its process group and
    /// returns. The outcome comes from the process's exit event, which turns
    /// the state to `exited`; until then the status carries
    /// `stop_requested_ms`, so asking again shows a worker that is still
    /// alive. A repeated stop sends nothing. It never escalates; `worker.kill`
    /// does.
    pub(crate) fn stop(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        self.stop_once(worker_id, None, None)
    }

    /// [`Self::stop`] with the client's command id.
    pub(crate) fn stop_command(
        &self,
        target: &WorkerCommandTarget,
    ) -> Result<WorkerInfo, WorkerError> {
        self.command(
            target.command_id.as_deref(),
            "worker.stop",
            target,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.stop_once(&target.worker_id, target.caller_pane_id.as_deref(), receipt),
        )
    }

    /// `caller_pane`: the pane that asks for the stop or kill, if known.
    fn stop_once(
        &self,
        worker_id: &str,
        caller_pane: Option<&str>,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        let (number, live, status) = self.live(worker_id)?;
        if status.stop_requested_ms.is_some() {
            return self.status(worker_id);
        }
        let pid = status
            .pid
            .ok_or_else(|| WorkerError::NotRunning(format!("worker {worker_id} has no process")))?;
        self.record_tool_sessions(number, pid);
        let mut signal = json!({"type": "signal", "signal": "SIGTERM", "at_ms": now_ms()});
        mark_by_owner(&mut signal, &status, caller_pane);
        self.record_command(number, Direction::Herdr, &signal, receipt);
        live.close_input();
        crate::platform::signal_process_group(pid, Signal::Terminate)?;
        self.status(worker_id)
    }

    /// Answers the worker's pending question that `request_id` names, with
    /// the user's decision. Without `request_id` it answers the only pending
    /// question and refuses when several are pending. Only a `pending`
    /// question is answered.
    ///
    /// The answer goes out through an outbox: its decision is stored as
    /// `answer_intent` (the question is then `answering`), then the
    /// control_response is written to the worker, then `answer_sent` settles
    /// the question as answered. A failed write records `answer_failed` and
    /// makes the question pending again, so it can be answered again.
    pub(crate) fn answer(&self, params: &WorkerAnswerParams) -> Result<WorkerInfo, WorkerError> {
        let answered = self.answer_for_run(params)?;
        // A todo run that answers its worker's questions itself goes on
        // once the user answered one it escalated (a run's own escalation
        // wakes its driver as it is answered).
        if !params
            .request_id
            .as_deref()
            .is_some_and(runs::escalations::is_escalation)
        {
            self.question_settled(&params.worker_id);
        }
        Ok(answered)
    }

    /// [`Self::answer`] without waking the todo run of the worker: for the
    /// run's own `todo.resume`, which records the run going on itself.
    pub(crate) fn answer_for_run(
        &self,
        params: &WorkerAnswerParams,
    ) -> Result<WorkerInfo, WorkerError> {
        // A grant question is answered only from the user's client
        // ([`Self::answer_from_client`]), never through the API.
        if let Some(request_id) = params
            .request_id
            .as_deref()
            .filter(|request_id| grant_questions::is_grant_question(request_id))
        {
            return Err(grant_questions::needs_user(request_id));
        }
        if let Some(request_id) = params
            .request_id
            .as_deref()
            .filter(|request_id| runs::escalations::is_escalation(request_id))
        {
            self.answer_escalation(
                &params.worker_id,
                request_id,
                params.decision,
                &params.answers,
                params.message.as_deref(),
            )?;
            return self.escalation_reply(&params.worker_id);
        }
        self.command(
            params.command_id.as_deref(),
            "worker.answer",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.answer_once(params, None, receipt),
        )
    }

    /// [`Self::answer`] sent by tenure `params.coordinator_id`
    /// (`worker.answer_as`): applied only while that tenure owns the worker.
    /// A worker another tenure owns now refuses it with
    /// `ownership_transferred`, naming that tenure, and keeps it as a
    /// `late_answer` event, which reaches the owner's inbox; it is never
    /// applied. The ownership check and the record (of the answer or of the
    /// late answer) share one hold of the registry lock, as a handoff's move
    /// does, so an answer lands either before the move or after it. A review
    /// escalation is the user's question, not the worker's owner's: it is
    /// answered as `worker.answer` answers it.
    pub(crate) fn answer_as(
        &self,
        params: &WorkerAnswerAsParams,
    ) -> Result<WorkerInfo, WorkerError> {
        let answer = &params.answer;
        if answer.request_id.as_deref().is_some_and(|request_id| {
            runs::escalations::is_escalation(request_id)
                || grant_questions::is_grant_question(request_id)
        }) {
            return self.answer(answer);
        }
        let answered = self.command(
            answer.command_id.as_deref(),
            "worker.answer_as",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.answer_once(answer, Some(&params.coordinator_id), receipt),
        )?;
        self.question_settled(&answer.worker_id);
        Ok(answered)
    }

    /// `answering`: the tenure that sends the answer, which must own the
    /// worker ([`Self::answer_as`]).
    fn answer_once(
        &self,
        params: &WorkerAnswerParams,
        answering: Option<&str>,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        let worker_id = params.worker_id.as_str();
        let (number, live, request_id, response) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let owner = entry.status.owner_coordinator.clone();
            if let (Some(from), Some(owner)) = (answering, owner) {
                if owner != from {
                    return Err(self.refuse_late_answer(
                        &mut registry,
                        number,
                        params,
                        from,
                        &owner,
                        receipt,
                    ));
                }
            }
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if let Some(request_id) = params.request_id.as_deref() {
                let open = entry
                    .status
                    .questions
                    .iter()
                    .find(|pending| pending.question.request_id == request_id);
                match (open, entry.status.resolution(request_id)) {
                    (Some(pending), _) if pending.answering => {
                        return Err(WorkerError::QuestionGone(format!(
                            "question {request_id} of worker {worker_id} is no longer pending: \
                             its answer is being sent"
                        )))
                    }
                    (None, Some(how)) => {
                        return Err(WorkerError::QuestionGone(format!(
                        "question {request_id} of worker {worker_id} is no longer pending: {how}"
                    )))
                    }
                    _ => {}
                }
            }
            let live = match (&entry.live, entry.status.is_gone()) {
                (Some(live), false) => Arc::clone(live),
                _ => {
                    return Err(WorkerError::NotRunning(format!(
                        "worker {worker_id} is not running"
                    )))
                }
            };
            if entry.status.takeover_ms.is_some() {
                return Err(Self::taken_over(worker_id));
            }
            let questions: Vec<&Pending> = entry
                .status
                .questions
                .iter()
                .filter(|pending| !pending.answering)
                .collect();
            let pending = match params.request_id.as_deref() {
                Some(request_id) => questions
                    .iter()
                    .find(|pending| pending.question.request_id == request_id),
                None if questions.len() > 1 => {
                    let ids: Vec<&str> = questions
                        .iter()
                        .map(|pending| pending.question.request_id.as_str())
                        .collect();
                    return Err(WorkerError::Invalid(format!(
                        "worker {worker_id} has {} pending questions; name one with request_id: {}",
                        ids.len(),
                        ids.join(", ")
                    )));
                }
                None => questions.first(),
            }
            .ok_or_else(|| {
                WorkerError::NoQuestion(format!("worker {worker_id} has no such pending question"))
            })?;
            let (response, answers) = answer_response(pending, params)?;
            let request_id = pending.question.request_id.clone();
            let intent = json!({
                "type": "answer_intent",
                "request_id": request_id,
                "tool_name": pending.question.tool_name,
                "decision": response["behavior"],
                "answers": answers,
                "by": "user",
                // What a server re-attaching after a crash sends again.
                "response": response,
            });
            // Stored under the lock, so a second answer finds the question
            // answering and is refused.
            self.commit_command_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&intent),
                receipt,
            );
            (number, live, request_id, response)
        };
        self.shared.changed.notify_all();
        notify_clients();
        match live.send(self, &control_response(&request_id, response)) {
            Ok(_) => self.record(
                number,
                Direction::Herdr,
                &json!({"type": "answer_sent", "request_id": request_id}),
            ),
            Err(error) => {
                self.record(
                    number,
                    Direction::Herdr,
                    &json!({
                        "type": "answer_failed",
                        "request_id": request_id,
                        "error": error.to_string(),
                    }),
                );
                return Err(error);
            }
        }
        self.status(worker_id)
    }

    /// Keeps the answer tenure `from` sent to worker `number`, which tenure
    /// `owner` owns now, as a `late_answer` event (with the command's
    /// receipt, so a repeated command id replays the refusal and keeps it
    /// once), and returns the refusal. Under the registry lock.
    fn refuse_late_answer(
        &self,
        registry: &mut Registry,
        number: u64,
        params: &WorkerAnswerParams,
        from: &str,
        owner: &str,
        receipt: Option<&Receipt>,
    ) -> WorkerError {
        let late = json!({
            "type": "late_answer",
            "from_coordinator_id": from,
            "to_coordinator_id": owner,
            "request_id": params.request_id,
            "decision": params.decision,
            "answers": params.answers,
            "message": params.message,
        });
        self.commit_command_locked(
            registry,
            number,
            Direction::Herdr,
            store::Recorded::Event(&late),
            receipt,
        );
        self.shared.changed.notify_all();
        WorkerError::OwnershipTransferred(format!(
            "worker {} belongs to coordinator {owner} now, not to {from}: the answer was not \
             applied; it is passed to {owner} as information",
            params.worker_id
        ))
    }

    /// Runs a client command once per `command_id` (T3 Code's command
    /// receipts). Without an id, `run` just runs. With one, the id is
    /// claimed under the registry lock: new, `run` gets its receipt, which
    /// the command's first event reserves in the store in the same
    /// transaction, and the outcome (the reply or the refusal) is stored
    /// with it. Seen before, the stored outcome is returned and nothing
    /// runs; one that a server restart cut off goes to `cut_off`. A repeat
    /// while it runs in this server waits for its outcome. The same id with
    /// another method or other parameters is refused.
    fn command<T: Serialize + DeserializeOwned>(
        &self,
        command_id: Option<&str>,
        method: &'static str,
        params: &impl Serialize,
        cut_off: impl FnOnce(&store::StoredReceipt) -> Result<T, WorkerError>,
        run: impl FnOnce(Option<&Receipt>) -> Result<T, WorkerError>,
    ) -> Result<T, WorkerError> {
        let Some(command_id) = command_id else {
            return run(None);
        };
        if command_id.trim().is_empty() {
            return Err(WorkerError::Invalid("command_id must not be empty".into()));
        }
        let store = self.shared.store.as_ref().map_err(|error| {
            WorkerError::Io(std::io::Error::other(format!(
                "{error}; a command_id needs it (send the command without one)"
            )))
        })?;
        let params = command_params(params);
        let worker_id = params["worker_id"].as_str().map(str::to_owned);
        let receipt = Receipt {
            command_id: command_id.to_owned(),
            method,
            params: params.to_string(),
            reserved: Cell::new(false),
        };
        let in_flight = match self.claim_command(store, &receipt)? {
            Claim::Stored(stored) => {
                return match stored.state {
                    store::ReceiptState::Accepted => stored
                        .result
                        .as_deref()
                        .and_then(|result| serde_json::from_str(result).ok())
                        .ok_or_else(|| {
                            WorkerError::Io(std::io::Error::other(format!(
                                "the stored reply of command {command_id} is unreadable"
                            )))
                        }),
                    store::ReceiptState::Rejected => {
                        let refusal: Value = stored
                            .result
                            .as_deref()
                            .and_then(|result| serde_json::from_str(result).ok())
                            .unwrap_or(Value::Null);
                        Err(WorkerError::replayed(
                            refusal["code"].as_str().unwrap_or(""),
                            refusal["message"].as_str().unwrap_or("refused").to_owned(),
                        ))
                    }
                    store::ReceiptState::Pending => cut_off(&stored),
                };
            }
            Claim::Run(in_flight) => in_flight,
        };
        let outcome = run(Some(&receipt));
        // A drain ends: refused before any event, the id stays unused, so
        // the same command can be sent again once the drain is over.
        if matches!(outcome, Err(WorkerError::Draining(_))) && !receipt.reserved.get() {
            drop(in_flight);
            return outcome;
        }
        let (state, result) = match &outcome {
            Ok(reply) => (
                store::ReceiptState::Accepted,
                serde_json::to_string(reply).unwrap_or_else(|_| "null".into()),
            ),
            Err(error) => (
                store::ReceiptState::Rejected,
                json!({"code": error.code(), "message": error.to_string()}).to_string(),
            ),
        };
        let settled = store.transaction(|tx| {
            tx.settle_receipt(
                &receipt.row(),
                worker_id.as_deref(),
                state,
                &result,
                now_ms(),
            )
        });
        if let Err(error) = settled {
            warn!(%error, command_id, "worker command outcome not stored");
        }
        drop(in_flight);
        outcome
    }

    /// Claims a command id for this call, or finds its stored outcome; see
    /// [`Self::command`]. Waits, woken by the outcome, while this server
    /// runs the same command.
    fn claim_command<'a>(
        &'a self,
        store: &store::Store,
        receipt: &Receipt,
    ) -> Result<Claim<'a>, WorkerError> {
        let conflict = |method: &str| {
            WorkerError::CommandConflict(format!(
                "command id {} was used for {method} with other parameters; use a new id",
                receipt.command_id
            ))
        };
        let mut registry = lock(&self.shared.registry);
        loop {
            if let Some((method, params)) = registry.commands.get(&receipt.command_id) {
                if (*method, params) != (receipt.method, &receipt.params) {
                    return Err(conflict(method));
                }
                registry = self
                    .shared
                    .changed
                    .wait(registry)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }
            let stored = store
                .receipt(&receipt.command_id)
                .map_err(|error| WorkerError::Io(std::io::Error::other(error.to_string())))?;
            return match stored {
                Some(stored)
                    if (stored.method.as_str(), &stored.params)
                        != (receipt.method, &receipt.params) =>
                {
                    Err(conflict(&stored.method))
                }
                Some(stored) => Ok(Claim::Stored(stored)),
                None => {
                    registry.commands.insert(
                        receipt.command_id.clone(),
                        (receipt.method, receipt.params.clone()),
                    );
                    Ok(Claim::Run(InFlight {
                        supervisor: self,
                        command_id: receipt.command_id.clone(),
                    }))
                }
            };
        }
    }

    /// Why a repeated command id has no outcome: a server restart cut its
    /// command off.
    fn cut_off(stored: &store::StoredReceipt) -> WorkerError {
        WorkerError::CommandInterrupted(format!(
            "this command id's {} was cut off by a server restart before its outcome was \
             stored; {} shows what it did",
            stored.method,
            match &stored.worker_id {
                Some(worker_id) => format!("`herdr worker status {worker_id}`"),
                None => "the worker's status".to_owned(),
            }
        ))
    }

    /// [`Self::kill_command`] without a command id.
    #[cfg(all(test, unix))]
    pub(crate) fn kill(
        &self,
        worker_id: &str,
        force: bool,
    ) -> Result<(WorkerInfo, WorkerKillReport), WorkerError> {
        self.kill_once(worker_id, force, None, None)
    }

    /// Force-stops a worker: SIGKILL to its process group, then to the
    /// processes of its recorded tool sessions that are still the ones
    /// recorded ([`plan_session_kill`]). A worker that exited or was lost is
    /// signalled only with `force`; without it the kill is refused with what
    /// it would signal, since its sessions may have ended long ago.
    pub(crate) fn kill_command(
        &self,
        params: &WorkerKillParams,
    ) -> Result<(WorkerInfo, WorkerKillReport), WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.kill",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| {
                self.kill_once(
                    &params.worker_id,
                    params.force,
                    params.caller_pane_id.as_deref(),
                    receipt,
                )
            },
        )
    }

    fn kill_once(
        &self,
        worker_id: &str,
        force: bool,
        caller_pane: Option<&str>,
        receipt: Option<&Receipt>,
    ) -> Result<(WorkerInfo, WorkerKillReport), WorkerError> {
        let (number, status) = {
            let registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            (number, registry.workers[&number].status.clone())
        };
        if status.is_gone() && !force {
            let plan = plan_session_kill(
                &status.tool_sessions,
                crate::platform::process_start_token,
                crate::platform::session_members,
            );
            let state = if status.exited { "exited" } else { "lost" };
            return Err(WorkerError::NeedsForce(format!(
                "worker {worker_id} is {state}, so nothing was signalled; with force, kill would signal {}",
                describe_kill(&plan)
            )));
        }
        if let (Some(pid), false) = (status.pid, status.is_gone()) {
            self.record_tool_sessions(number, pid);
            let mut signal = json!({"type": "signal", "signal": "SIGKILL"});
            mark_by_owner(&mut signal, &status, caller_pane);
            self.record_command(number, Direction::Herdr, &signal, receipt);
            crate::platform::signal_process_group(pid, Signal::Kill)?;
        }
        let sessions = self.with_entry(worker_id, |entry| entry.status.tool_sessions.clone())?;
        let plan = plan_session_kill(
            &sessions,
            crate::platform::process_start_token,
            crate::platform::session_members,
        );
        crate::platform::signal_processes(&plan.pids, Signal::Kill);
        self.record_command(
            number,
            Direction::Herdr,
            &json!({
                "type": "killed_tool_processes",
                "pids": plan.pids,
                "unverified_sessions": plan.unverified_sessions,
                "stale_sessions": plan.stale_sessions,
            }),
            receipt,
        );
        Ok((self.status(worker_id)?, plan))
    }

    /// Why a running worker refuses a prompt or an answer: a takeover is
    /// ending it.
    fn taken_over(worker_id: &str) -> WorkerError {
        WorkerError::Busy(format!(
            "worker {worker_id} is being taken over; its session resumes in a tab"
        ))
    }

    /// Claims a worker for a takeover and journals it. Refused while a
    /// question waits on the user (answer it first, or the interrupt would
    /// throw the answer away), for a worker without a session yet, for one
    /// that was lost while its process still runs (it is not ours to end),
    /// and while another
    /// takeover holds the claim or has opened its tab. The claim is kept in
    /// every state, so an exited worker is taken over once too; a failed
    /// takeover releases it ([`Self::fail_takeover`]). One an earlier server
    /// left unfinished, whose tab recovery did not find
    /// ([`Self::adopt_takeover_tab`]), is tried again only with `force`:
    /// that server may have opened a tab nobody saw, and a second tab would
    /// be a second writer of the session. The new claim replaces it under
    /// the lock, so concurrent retries are serialized.
    /// The refusal of a prompt or takeover of a worker whose output after a
    /// server restart is not proven complete.
    fn continuity_gap(worker_id: &str, gap: &str) -> WorkerError {
        WorkerError::ContinuityGap(format!(
            "worker {worker_id}'s output since a server restart is not proven complete ({gap}), \
             so its state and session may be stale: it takes no new prompt and no takeover; \
             stop it and start a new worker"
        ))
    }

    pub(crate) fn begin_takeover(
        &self,
        worker_id: &str,
        force: bool,
    ) -> Result<Takeover, WorkerError> {
        let takeover = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let status = &entry.status;
            if let Some(gap) = &status.continuity_gap {
                return Err(Self::continuity_gap(worker_id, gap));
            }
            if status.lost && !status.process_gone() {
                return Err(WorkerError::NotRunning(format!(
                    "worker {worker_id} was lost: its process is not this server's to end"
                )));
            }
            if !status.questions.is_empty() {
                return Err(WorkerError::Busy(format!(
                    "worker {worker_id} waits on your answer; answer it before taking over"
                )));
            }
            if status.takeover_ms.is_some() && !status.takeover_unfinished {
                return Err(WorkerError::Busy(match &status.takeover_tab {
                    Some(tab_id) => format!(
                        "worker {worker_id} was already taken over: its session resumes in tab {tab_id}"
                    ),
                    None => format!("worker {worker_id} is already being taken over"),
                }));
            }
            if status.takeover_unfinished && !force {
                return Err(WorkerError::Busy(unfinished_takeover_refusal(worker_id)));
            }
            let session_id = status.session_id.clone().ok_or_else(|| {
                WorkerError::NotRunning(format!("worker {worker_id} has no session yet"))
            })?;
            let at_ms = now_ms();
            let takeover = Takeover {
                worker_id: worker_id.to_owned(),
                workspace_id: status.workspace_id.clone(),
                name: status.name.clone(),
                cwd: status.cwd.clone(),
                session_id,
                takeover_id: new_takeover_id(worker_id, at_ms),
            };
            let event = json!({
                "type": "takeover",
                "at_ms": at_ms,
                "takeover_id": takeover.takeover_id,
                "forced": status.takeover_unfinished,
            });
            // Claimed under the lock, so a second takeover is refused.
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&event),
            );
            takeover
        };
        self.shared.changed.notify_all();
        notify_clients();
        Ok(takeover)
    }

    /// Releases a takeover's claim with a journaled `takeover_failed`, so the
    /// user can try again: its thread did not start, ending the worker
    /// failed, or its tab could not be opened.
    pub(crate) fn fail_takeover(&self, worker_id: &str, error: &str) -> Result<(), WorkerError> {
        self.record_takeover_step(
            worker_id,
            json!({"type": "takeover_failed", "error": error, "at_ms": now_ms()}),
        )
    }

    /// Journals the tab that resumes a taken-over worker's session; until
    /// then a restart reports the takeover as unfinished.
    pub(crate) fn takeover_tab_opened(
        &self,
        worker_id: &str,
        tab_id: &str,
    ) -> Result<(), WorkerError> {
        self.record_takeover_step(
            worker_id,
            json!({"type": "takeover_tab_opened", "tab_id": tab_id, "at_ms": now_ms()}),
        )
    }

    /// The takeovers an earlier server left unfinished, for recovery to look
    /// for their tabs.
    pub(crate) fn unfinished_takeovers(&self) -> Vec<UnfinishedTakeover> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .filter_map(|entry| unfinished_takeover_of(&entry.status))
            .collect()
    }

    /// The worker's takeover an earlier server left unfinished, if any.
    pub(crate) fn unfinished_takeover(
        &self,
        worker_id: &str,
    ) -> Result<Option<UnfinishedTakeover>, WorkerError> {
        self.with_entry(worker_id, |entry| unfinished_takeover_of(&entry.status))
    }

    /// Adopts the tab recovery found for an unfinished takeover: journals
    /// `takeover_tab_opened` with it, as the server that opened it would
    /// have. Only while that same claim is still unfinished, so a claim
    /// taken meanwhile is not given another claim's tab.
    pub(crate) fn adopt_takeover_tab(
        &self,
        takeover: &UnfinishedTakeover,
        tab_id: &str,
    ) -> Result<(), WorkerError> {
        let worker_id = takeover.worker_id.as_str();
        {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if unfinished_takeover_of(&entry.status).as_ref() != Some(takeover) {
                return Err(WorkerError::Invalid(format!(
                    "worker {worker_id} has no such unfinished takeover"
                )));
            }
            let event = json!({
                "type": "takeover_tab_opened",
                "tab_id": tab_id,
                "adopted": true,
                "at_ms": now_ms(),
            });
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&event),
            );
        }
        self.shared.changed.notify_all();
        notify_clients();
        Ok(())
    }

    /// Records a step of a claimed takeover under the lock.
    fn record_takeover_step(&self, worker_id: &str, event: Value) -> Result<(), WorkerError> {
        {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if entry.status.takeover_ms.is_none() {
                return Err(WorkerError::Invalid(format!(
                    "worker {worker_id} has no takeover claimed"
                )));
            }
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&event),
            );
        }
        self.shared.changed.notify_all();
        notify_clients();
        Ok(())
    }

    /// Ends a worker being taken over, so its session has one writer: the
    /// running turn is interrupted and its `result` (`aborted_*`) awaited,
    /// then the worker is stopped and its exit awaited. Blocks on those
    /// events; no timer decides anything, so a worker that ignores SIGTERM
    /// keeps this waiting until the user force-stops it (`worker.kill`).
    pub(crate) fn end_for_takeover(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        // Only to look at nothing: the waits below end on the worker's
        // events, never on this interval.
        const RECHECK: Duration = Duration::from_secs(60);
        let (gone, status) = self.with_entry(worker_id, |entry| {
            (
                entry.status.is_gone(),
                entry.status.info(&entry.journal_path),
            )
        })?;
        if gone {
            return Ok(status);
        }
        let in_turn = !matches!(
            status.state,
            WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
        );
        if in_turn {
            let interrupt = WorkerInterruptParams {
                worker_id: worker_id.to_owned(),
                turn: status.turn_seq,
                command_id: None,
            };
            match self.interrupt(&interrupt) {
                // The turn ended on its own meanwhile.
                Ok(_) | Err(WorkerError::TurnEnded(_)) => {}
                Err(error) => return Err(error),
            }
            self.wait(worker_id, WorkerWaitUntil::TurnEnd, RECHECK, || true)?;
        }
        match self.stop(worker_id) {
            // It exited between the wait and the stop.
            Ok(_) | Err(WorkerError::NotRunning(_)) => {}
            Err(error) => return Err(error),
        }
        self.wait(worker_id, WorkerWaitUntil::Exit, RECHECK, || true)?
            .ok_or_else(|| WorkerError::NotRunning(format!("worker {worker_id} wait ended")))
    }

    /// Readies the folder slot `name` of `caller_cwd`'s repository for a new
    /// worker: refuses while a worker still runs there, ends what the
    /// earlier workers there left running, then cleans and switches it
    /// ([`slot::prepare`]). Returns the slot and the pids it ended.
    fn prepare_slot(
        &self,
        caller_cwd: &Path,
        name: &str,
        params: &WorkerStartParams,
    ) -> Result<(slot::Slot, Vec<u32>), WorkerError> {
        let branch = params
            .branch
            .as_deref()
            .ok_or_else(|| WorkerError::Invalid("folder_slot needs branch".into()))?;
        let base = params.base.as_deref().unwrap_or("master");
        let (path, created) = slot::locate_or_create(caller_cwd, name, base)?;
        let killed = self.end_slot_leftovers(&path)?;
        // Before the switch, so a refused start leaves no branch behind.
        slot::bound_target(&path)?;
        let slot = slot::prepare(&path, branch, base, params.fresh_build, created)?;
        Ok((slot, killed))
    }

    /// Refuses while one of this server's workers still runs in the slot
    /// (also between turns: stop it first). Then SIGKILLs what earlier
    /// workers there left: a lost worker's process group and the recorded
    /// tool sessions, each process only when its working directory is
    /// inside the slot (a reused pid or session is someone else's).
    fn end_slot_leftovers(&self, path: &Path) -> Result<Vec<u32>, WorkerError> {
        let shown = path.display().to_string();
        let previous: Vec<(Status, bool)> = {
            let registry = lock(&self.shared.registry);
            registry
                .workers
                .values()
                .filter(|entry| entry.status.cwd == shown)
                .map(|entry| (entry.status.clone(), entry.live.is_some()))
                .collect()
        };
        if let Some((status, _)) = previous
            .iter()
            .find(|(status, live)| *live && !status.is_gone())
        {
            return Err(WorkerError::Busy(format!(
                "worker {} still runs in folder slot {shown}; wait for it and stop it first",
                status.worker_id
            )));
        }
        let mut killed = Vec::new();
        for (status, _) in previous {
            let mut ended = Vec::new();
            if let (Some(pid), false) = (status.pid, status.exited) {
                if crate::platform::process_group_alive(pid) && slot::runs_in_slot(pid, path) {
                    crate::platform::signal_process_group(pid, Signal::Kill)?;
                    ended.push(pid);
                }
            }
            for session in status.tool_sessions.keys() {
                let members: Vec<u32> = crate::platform::session_members(*session)
                    .into_iter()
                    .filter(|pid| slot::runs_in_slot(*pid, path))
                    .collect();
                crate::platform::signal_processes(&members, Signal::Kill);
                ended.extend(members);
            }
            if ended.is_empty() {
                continue;
            }
            if let Some(number) = worker_number(&status.worker_id) {
                self.record(
                    number,
                    Direction::Herdr,
                    &json!({"type": "killed_tool_processes", "pids": ended, "reason": "folder slot reused"}),
                );
            }
            killed.extend(ended);
        }
        Ok(killed)
    }

    fn journal_path(&self, worker_id: &str) -> PathBuf {
        self.shared.dir.join(format!("{worker_id}.jsonl"))
    }

    /// Creates the worker's own temp dir and returns its real path: the
    /// sandbox and the policy compare real paths. The sandbox's `$TMPDIR` is
    /// `/tmp/claude-<uid>`, shared by every sandboxed session of the user,
    /// so workers get this one by its absolute path (T3-2). It is created
    /// new (anything already at its path, a symlink included, fails the
    /// start) and private, inside a private parent that is a real directory.
    fn create_temp_dir(&self, worker_id: &str) -> std::io::Result<PathBuf> {
        let path = temp_dir_path(&self.shared.dir, worker_id);
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("worker temp dir has no parent"))?;
        std::fs::create_dir_all(&self.shared.dir)?;
        match crate::platform::create_private_dir(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        require_real_dir(parent)?;
        crate::platform::create_private_dir(&path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("worker temp dir {}: {error}", path.display()),
            )
        })?;
        require_real_dir(&path)?;
        path.canonicalize()
    }

    /// Watches the CLI's children's exits and reports a tool call whose
    /// output its descendants hold open: when a child of the CLI exits
    /// (the positive event, from the platform's process watch) and
    /// processes outside the CLI's tree still hold the write end of a pipe
    /// the CLI reads, a `tool_output_held` event names those processes.
    /// Each holder is reported once. Herdr kills nothing and sets no timer:
    /// the report is a signal for the worker's coordinator, whose wait
    /// returns with `held_output`. Only macOS has the fork event this
    /// needs; elsewhere nothing is watched.
    fn watch_held_output(&self, number: u64, pid: u32) {
        let supervisor = self.clone();
        let mut reported: BTreeSet<(u32, Option<u64>)> = BTreeSet::new();
        let watched = crate::platform::watch_child_exits(
            pid,
            Box::new(move |exited| {
                let holders: Vec<WorkerPipeHolder> = crate::platform::outside_pipe_writers(pid)
                    .into_iter()
                    .filter(|writer| {
                        reported
                            .insert((writer.pid, crate::platform::process_start_token(writer.pid)))
                    })
                    .map(|writer| WorkerPipeHolder {
                        pid: writer.pid,
                        command: writer.command,
                    })
                    .collect();
                if !holders.is_empty() {
                    supervisor.report_held_output(number, exited, holders);
                }
            }),
        );
        if let Err(error) = watched {
            if error.kind() != std::io::ErrorKind::Unsupported {
                warn!(%error, pid, "cannot watch the worker's tool processes");
            }
        }
    }

    /// Reports held output, naming the tool calls open now. The CLI writes
    /// a call's `tool_use` line before it runs the tool, but the reader
    /// may not have read it yet when the tool's child exits: a report found
    /// mid-turn with no open call waits in the entry for that line
    /// ([`Self::flush_held_output`]) instead of naming no call.
    fn report_held_output(&self, number: u64, exited: u32, holders: Vec<WorkerPipeHolder>) {
        let report = {
            let mut registry = lock(&self.shared.registry);
            // Only the server attached to the worker reports: one that let
            // go of it at a live handoff leaves that to the new one.
            let Some(entry) = registry
                .workers
                .get_mut(&number)
                .filter(|entry| entry.live.is_some())
            else {
                return;
            };
            if entry.status.open_tool_calls.is_empty() && entry.status.mid_turn() {
                match &mut entry.held_pending {
                    Some((_, pending)) => pending.extend(holders),
                    None => entry.held_pending = Some((exited, holders)),
                }
                return;
            }
            (
                entry.status.name.clone(),
                WorkerHeldOutput {
                    exited_pid: exited,
                    holders,
                    tool_calls: entry.status.open_tool_calls.clone(),
                    at_ms: now_ms(),
                },
            )
        };
        self.record_held_output(number, report);
    }

    /// Records the held output waiting for its tool call
    /// ([`Self::report_held_output`]), naming the calls open now. The
    /// reader calls it after each `assistant` line, where it records only
    /// once a call is open (a text block can come before the `tool_use`),
    /// and with `at_turn_end` before the turn's `result` and the worker's
    /// exit, so a report whose call never came is still recorded in its
    /// turn.
    fn flush_held_output(&self, number: u64, at_turn_end: bool) {
        let report = {
            let mut registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get_mut(&number) else {
                return;
            };
            if entry.held_pending.is_none()
                || (!at_turn_end && entry.status.open_tool_calls.is_empty())
            {
                return;
            }
            let Some((exited, holders)) = entry.held_pending.take() else {
                return;
            };
            (
                entry.status.name.clone(),
                WorkerHeldOutput {
                    exited_pid: exited,
                    holders,
                    tool_calls: entry.status.open_tool_calls.clone(),
                    at_ms: now_ms(),
                },
            )
        };
        self.record_held_output(number, report);
    }

    fn record_held_output(&self, number: u64, (name, report): (String, WorkerHeldOutput)) {
        let mut event = json!({"type": "tool_output_held"});
        if let (Some(event), Ok(Value::Object(fields))) =
            (event.as_object_mut(), serde_json::to_value(&report))
        {
            event.extend(fields);
        }
        self.record(number, Direction::Herdr, &event);
        notify_user(UserNotice {
            title: format!("Worker {name}: a tool call's output is held open"),
            body: held_output_summary(&report),
        });
    }

    /// Records the sessions of the CLI's descendants that are new. Claude
    /// Code runs each Bash tool with `setsid`, so after a crash only these
    /// recorded sessions find its tools (trial 2, T2-3).
    fn record_tool_sessions(&self, number: u64, pid: u32) {
        let seen = crate::platform::descendant_sessions(pid);
        if seen.is_empty() {
            return;
        }
        let new: Vec<u32> = {
            let registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get(&number) else {
                return;
            };
            seen.into_iter()
                .filter(|session| !entry.status.tool_sessions.contains_key(session))
                .collect()
        };
        if new.is_empty() {
            return;
        }
        // A session id is its leader's pid; the leader's start token tells
        // it apart from a later process that gets the same pid, so `kill`
        // can check it is still the same session.
        let sessions: Vec<Value> = new
            .into_iter()
            .map(
                |session| match crate::platform::process_start_token(session) {
                    Some(start) => json!({"session": session, "leader_start": start}),
                    None => json!({"session": session}),
                },
            )
            .collect();
        self.record(
            number,
            Direction::Herdr,
            &json!({"type": "tool_sessions", "sessions": sessions}),
        );
    }
}

/// Which processes of the recorded tool `sessions` (each with its leader's
/// start token) `kill` may signal. A session id is its leader's pid, which a
/// new session leader, such as a pane's shell, can get once the session has
/// ended, so a session counts only while its leader still has the recorded
/// start token, and of its members only those that did not start before
/// that leader: every process of a session is its leader or was forked
/// after the leader started. A session without a recorded token is not
/// verified and not signalled. On Windows nothing records sessions, so the
/// plan is empty there.
fn plan_session_kill(
    sessions: &BTreeMap<u32, Option<u64>>,
    start_token: impl Fn(u32) -> Option<u64>,
    members: impl Fn(u32) -> Vec<u32>,
) -> WorkerKillReport {
    let mut report = WorkerKillReport {
        pids: Vec::new(),
        unverified_sessions: Vec::new(),
        stale_sessions: Vec::new(),
    };
    for (&session, &leader_start) in sessions {
        let Some(leader_start) = leader_start else {
            report.unverified_sessions.push(session);
            continue;
        };
        if start_token(session) != Some(leader_start) {
            report.stale_sessions.push(session);
            continue;
        }
        report.pids.extend(
            members(session)
                .into_iter()
                .filter(|pid| start_token(*pid).is_some_and(|start| start >= leader_start)),
        );
    }
    report
}

/// `kill`'s plan in words, for the refusal that asks for force.
fn describe_kill(plan: &WorkerKillReport) -> String {
    let list = |ids: &[u32]| {
        ids.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = if plan.pids.is_empty() {
        "no process".to_owned()
    } else {
        format!("pids {}", list(&plan.pids))
    };
    if !plan.unverified_sessions.is_empty() {
        text.push_str(&format!(
            "; sessions not verified, not killed: {}",
            list(&plan.unverified_sessions)
        ));
    }
    if !plan.stale_sessions.is_empty() {
        text.push_str(&format!(
            "; sessions whose leader is gone or another process, skipped: {}",
            list(&plan.stale_sessions)
        ));
    }
    text
}

/// The directories a `worker.verify` runs in now.
static VERIFYING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A directory claimed by one verification until it is dropped.
struct VerifyClaim(PathBuf);

impl VerifyClaim {
    fn take(dir: &Path) -> Result<Self, WorkerError> {
        let mut verifying = lock(&VERIFYING);
        if verifying.iter().any(|claimed| claimed == dir) {
            return Err(WorkerError::Busy(format!(
                "a verification already runs in {}",
                dir.display()
            )));
        }
        verifying.push(dir.to_owned());
        Ok(Self(dir.to_owned()))
    }
}

impl Drop for VerifyClaim {
    fn drop(&mut self) {
        lock(&VERIFYING).retain(|claimed| *claimed != self.0);
    }
}

/// Where a worker's temp dir lives, beside the journals.
fn temp_dir_path(dir: &Path, worker_id: &str) -> PathBuf {
    dir.join("tmp").join(worker_id)
}

/// Fails unless `path` itself (not a symlink's target) is a directory.
fn require_real_dir(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{} is not a real directory",
            path.display()
        )))
    }
}

fn remove_temp_dir(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(%error, path = %path.display(), "worker temp dir not removed"),
    }
}

fn user_message(text: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": text}})
}

fn control_response(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

/// The `can_use_tool` answer for the user's decision, and the answers it
/// carries (for the journal). An `AskUserQuestion` is allowed with
/// `updatedInput.answers`, a map from each question to its answer (trial 1,
/// case 3).
fn answer_response(
    pending: &Pending,
    params: &WorkerAnswerParams,
) -> Result<(Value, Value), WorkerError> {
    let question = &pending.question;
    let deny = |default: &str| {
        let message = params.message.as_deref().unwrap_or(default);
        Ok((json!({"behavior": "deny", "message": message}), Value::Null))
    };
    match question.kind {
        WorkerQuestionKind::Choice => {
            if params.decision == Some(WorkerDecision::Deny) {
                return deny("The user declined to answer.");
            }
            if params.answers.len() != question.questions.len() {
                return Err(WorkerError::Invalid(format!(
                    "this question needs {} answer(s), got {}",
                    question.questions.len(),
                    params.answers.len()
                )));
            }
            let mut answers = serde_json::Map::new();
            for (choice, raw) in question.questions.iter().zip(&params.answers) {
                let answer = resolve_choice(choice, raw).ok_or_else(|| {
                    WorkerError::Invalid(format!("an empty answer to: {}", choice.question))
                })?;
                answers.insert(choice.question.clone(), Value::String(answer));
            }
            let mut input = pending.input.clone();
            if let Some(object) = input.as_object_mut() {
                object.insert("answers".into(), Value::Object(answers.clone()));
            }
            Ok((
                json!({"behavior": "allow", "updatedInput": input}),
                Value::Object(answers),
            ))
        }
        WorkerQuestionKind::Approval | WorkerQuestionKind::Unknown => {
            if !params.answers.is_empty() {
                return Err(WorkerError::Invalid(
                    "an approval takes allow or deny, not answers".into(),
                ));
            }
            match params.decision {
                Some(WorkerDecision::Allow) => Ok((
                    json!({"behavior": "allow", "updatedInput": pending.input}),
                    Value::Null,
                )),
                Some(WorkerDecision::Deny) => deny("The user denied this tool use."),
                None => Err(WorkerError::Invalid(
                    "an approval needs the decision allow or deny".into(),
                )),
            }
        }
    }
}

/// One answer: an option's 1-based number or label (any case) becomes the
/// label, anything else is the user's own text. A multi-select answer is a
/// comma-separated list of those, joined with `, `.
fn resolve_choice(choice: &WorkerChoiceQuestion, raw: &str) -> Option<String> {
    let resolve = |part: &str| {
        let part = part.trim();
        part.parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| choice.options.get(index))
            .or_else(|| {
                choice
                    .options
                    .iter()
                    .find(|label| label.eq_ignore_ascii_case(part))
            })
            .cloned()
            .unwrap_or_else(|| part.to_owned())
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if !choice.multi_select || choice.options.iter().any(|label| label == raw) {
        return Some(resolve(raw));
    }
    let parts: Vec<String> = raw
        .split(',')
        .map(resolve)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The question a `can_use_tool` request the policy left to the user asks.
fn question_from_request(request_id: &str, request: &Value, reason: &str) -> WorkerQuestion {
    let tool_name = request["tool_name"].as_str().unwrap_or("").to_owned();
    let input = &request["input"];
    let questions: Vec<WorkerChoiceQuestion> = if tool_name == "AskUserQuestion" {
        input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|question| WorkerChoiceQuestion {
                question: question["question"].as_str().unwrap_or("").to_owned(),
                header: string_field(question, "header"),
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option["label"].as_str().map(str::to_owned))
                    .collect(),
                multi_select: question["multiSelect"].as_bool().unwrap_or(false),
            })
            .collect()
    } else {
        Vec::new()
    };
    let text = if tool_name == "AskUserQuestion" {
        questions
            .iter()
            .map(|question| format!("{} [{}]", question.question, question.options.join(" | ")))
            .collect::<Vec<_>>()
            .join(" / ")
    } else if let Some(command) = input["command"].as_str() {
        command.to_owned()
    } else if let Some(path) = ["file_path", "notebook_path", "path", "url"]
        .iter()
        .find_map(|key| input[*key].as_str())
    {
        path.to_owned()
    } else {
        input.to_string()
    };
    WorkerQuestion {
        request_id: request_id.to_owned(),
        kind: if tool_name == "AskUserQuestion" {
            WorkerQuestionKind::Choice
        } else {
            WorkerQuestionKind::Approval
        },
        tool_name,
        text: one_line(&text, 300),
        reason: Some(reason.to_owned()),
        questions,
        since_ms: now_ms(),
        state: WorkerQuestionState::Pending,
        escalated: None,
    }
}

/// A tool's whole input for the user to decide on: a `command` as written,
/// newlines kept, then the input's other fields one per line (a sandbox or
/// background flag changes what allowing means); any other input as
/// indented JSON. Never cut: an approval must not rest on a preview.
fn full_input_text(input: &Value) -> String {
    let Some(command) = input["command"].as_str() else {
        return serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    };
    let mut text = command.to_owned();
    let others: Vec<String> = input
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.as_str() != "command")
        .map(|(key, value)| match value.as_str() {
            Some(value) => format!("{key}: {value}"),
            None => format!("{key}: {value}"),
        })
        .collect();
    if !others.is_empty() {
        text.push_str("\n\n");
        text.push_str(&others.join("\n"));
    }
    text
}

/// `text` on one line, cut to `max` characters with an ellipsis.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}
