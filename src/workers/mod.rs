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
mod supervisor;
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
use supervisor::lifecycle::{remove_temp_dir, temp_dir_path};
use supervisor::questions::{control_response, question_from_request};

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

    fn journal_path(&self, worker_id: &str) -> PathBuf {
        self.shared.dir.join(format!("{worker_id}.jsonl"))
    }
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

fn user_message(text: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": text}})
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
