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
pub(crate) mod coordinator_handoff;
pub(crate) mod coordinators;
mod decisions;
mod error;
mod grant_questions;
mod history;
pub(crate) mod home_repo;
mod inbox;
#[cfg(test)]
mod install_script_tests;
mod item_coordinators;
mod journal;
mod live;
mod log;
mod pi;
mod policy;
mod pre_tool_checks;
mod registry;
pub(crate) mod reports;
mod review;
mod runs;
mod slot;
mod status;
mod store;
pub(crate) mod supervision;
mod supervision_rules;
mod supervisor;
mod target_branch;
#[cfg(test)]
mod tests;
pub(crate) mod todo_editor;
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
pub(crate) use runs::activity::{ActiveRun, RunWait, WaitEdge};
pub(crate) use runs::review_eval::{review_eval, EvalOptions};
#[cfg(unix)]
pub(crate) use runs::{envs_for_handoff, restore_handed_off_envs};
pub(crate) use runs::{resume_runs_at_start, usage_measurement, usage_reading_published};
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
    WorkerAgent, WorkerAnswerAsParams, WorkerAnswerParams, WorkerChoiceQuestion,
    WorkerCommandTarget, WorkerDecision, WorkerDenyAndStopParams, WorkerDrain, WorkerDrainAction,
    WorkerHeldOutput, WorkerInfo, WorkerInterruptParams, WorkerItemRuns, WorkerKillParams,
    WorkerKillReport, WorkerObligation, WorkerPipeHolder, WorkerPromptParams, WorkerQuestion,
    WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionState, WorkerRun, WorkerRunsParams,
    WorkerStartParams, WorkerState, WorkerVerification, WorkerVerifyParams,
    WorkerWaitDrainedParams, WorkerWaitUntil,
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
use supervisor::lifecycle::{pi_extension_path, remove_temp_dir, temp_dir_path};
use supervisor::questions::{control_response, question_from_request};
#[cfg(all(test, unix))]
use supervisor::takeover::ProcessProbe;
pub(crate) use supervisor::takeover::{find_takeover_tab, LiveProcesses};

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
    // pi's logins, the ChatGPT plan's among them (a pi worker's own CLI
    // reads it outside the sandbox).
    ("~/.pi/agent/auth.json", false),
];

/// Environment files in the worktree, at any depth. The sandbox's `denyRead`
/// takes globs (Observed with 2.1.293: `<worktree>/**/.env` blocked `cat
/// .env` and `cat sub/.env`).
const ENV_FILE_GLOBS: &[&str] = &["**/.env", "**/.env.*", "**/.envrc"];

/// Resolve the server-only key path without reading the credential. Require an
/// absolute path so server and sandbox cannot disagree about relative roots.
pub(crate) fn anthropic_key_path(
    config: &crate::config::WorkersConfig,
) -> std::io::Result<Option<PathBuf>> {
    if config.anthropic_api_cycle.len() > 64
        || !config
            .anthropic_api_cycle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(std::io::Error::other("invalid Anthropic API credit cycle"));
    }
    if !config.anthropic_api_cap_usd.is_finite() || config.anthropic_api_cap_usd < 0.0 {
        return Err(std::io::Error::other("invalid Anthropic API spending cap"));
    }
    let raw = &config.anthropic_api_key_file;
    if raw.is_empty() {
        return Ok(None);
    }
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        home_repo::home_dir()
            .ok_or_else(|| std::io::Error::other("key path requires HOME"))?
            .join(rest)
    } else {
        PathBuf::from(raw)
    };
    if !path.is_absolute() {
        return Err(std::io::Error::other("key path must be absolute"));
    }
    Ok(Some(path.canonicalize()?))
}

fn deny_worker_api_key(settings: &mut Value, key: &Path) -> std::io::Result<()> {
    let key = key.display().to_string();
    settings["sandbox"]["filesystem"]["denyRead"]
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("invalid worker settings"))?
        .push(json!(key));
    settings["permissions"]["deny"]
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("invalid worker settings"))?
        .extend([json!(format!("Read({key})")), json!(format!("Edit({key})"))]);
    Ok(())
}

fn configured_anthropic_key_path() -> std::io::Result<Option<PathBuf>> {
    let loaded = crate::config::load_live_config()
        .map_err(|_| std::io::Error::other("cannot load worker credential policy"))?;
    anthropic_key_path(&loaded.config.workers)
}

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
/// same for every worktree of it. The repository rooted at the home
/// directory whose git directory is registered elsewhere
/// ([`home_repo`]) is the home directory, from HOME and from its worktrees.
fn repository_of(dir: &Path) -> Option<String> {
    if let Some(home) = home_repo::repository_of_home_dir(dir) {
        return Some(home.display().to_string());
    }
    let space = crate::workspace::git_space_metadata(dir)?;
    if let Some(home) = home_repo::repository_of_common_dir(Path::new(&space.key)) {
        return Some(home.display().to_string());
    }
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
    /// The `pi` a pi worker runs ([`pi`]); a test sets its stub.
    pi_program: Mutex<PathBuf>,
    /// The worker store, or why it could not be opened; then every event's
    /// write fails and marks its worker degraded.
    store: Result<store::Store, String>,
    registry: Mutex<Registry>,
    changed: Condvar,
    /// Held while a folder slot is prepared and its worker registered, so
    /// two starts never prepare one slot at once.
    slot_lock: Mutex<()>,
    /// The tenure holder processes whose exit this server watches
    /// ([`crate::platform::watch_process_exit`]), each once.
    holder_watches: Mutex<BTreeSet<store::HolderProcess>>,
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
    /// The same for a pi run's gate, in place of OpenAI's ChatGPT plan.
    #[cfg(test)]
    openai_usage: Mutex<Result<crate::api::schema::ProviderUsage, String>>,
    /// The same for a pi run on a `deepseek/` model, in place of DeepSeek's
    /// prepaid balance.
    #[cfg(test)]
    deepseek_usage: Mutex<Result<crate::api::schema::ProviderUsage, String>>,
    /// The `[usage]` section a test sets for the usage gate's thresholds,
    /// in place of the config file, which the server reads.
    #[cfg(test)]
    usage_gate_config: Mutex<crate::config::UsageConfig>,
    /// The program a test sets for a run's automatic review in place of
    /// `program`, which the server calls.
    #[cfg(test)]
    review_program: Mutex<Option<PathBuf>>,
    /// Which of `worker.run`'s ending signals a test makes fail without
    /// sending it, so the run's handling of a failed stop is tested.
    #[cfg(all(test, unix))]
    gated_run_end_failure: Mutex<supervisor::gated_run::GatedRunEndFailure>,
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
/// The todo runs in progress with what their drivers publish of their
/// steps, for the client snapshot; none before the supervisor opened.
pub(crate) fn active_runs() -> Vec<ActiveRun> {
    SUPERVISOR
        .get()
        .map(WorkerSupervisor::active_runs)
        .unwrap_or_default()
}

/// The `todo.wait`s that block now: who waits on which run.
pub(crate) fn run_waits() -> Vec<RunWait> {
    runs::activity::run_waits()
}

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

/// Whether `pane_id` holds a started successor whose ack is pending.
pub(crate) fn is_pending_successor(pane_id: &str) -> bool {
    coordinators::installed().is_some_and(|supervisor| supervisor.is_pending_successor(pane_id))
}

/// Tells the supervisor which agent session `pane_id` runs now (`None`:
/// none), so superseded sessions are fenced wherever they run.
pub(crate) fn note_pane_session(pane_id: &str, session: Option<&str>) {
    if let Some(supervisor) = coordinators::installed() {
        supervisor.note_pane_session(pane_id, session);
    }
}

/// Forgets the agent session `pane_id` ran: the pane closed.
pub(crate) fn forget_pane_session(pane_id: &str) {
    if let Some(supervisor) = coordinators::installed() {
        supervisor.note_pane_session(pane_id, None);
    }
}

/// Forgets the sessions of every pane not in `live`, at startup.
pub(crate) fn prune_pane_sessions(live: &[String]) {
    if let Some(supervisor) = coordinators::installed() {
        supervisor.prune_pane_sessions(live);
    }
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

/// Reconciles the usage slots when the import assumes ownership: it retires
/// the source's unbound starts at the import, and again, with the same
/// source identity, when that process's exit event arrives, since a call the
/// source finishes while shutting down is admitted after the import.
///
/// A live handoff exists on Unix only (its manifest and descriptor passing
/// do), so there is no source to reconcile on Windows; the start of a Windows
/// server runs [`WorkerSupervisor::reconcile_admissions`], which retires what
/// a gone server admitted.
#[cfg(unix)]
pub(crate) fn reconcile_admissions_after_handoff(source: Option<(u32, Option<u64>)>) {
    let supervisor = supervisor();
    supervisor.reconcile_handoff_admissions(source);
    if let Some((pid, Some(start))) = source {
        // The source can still be finishing an API call admitted before
        // the handoff. Its exit is the producing event, not a timer: retire
        // what that call left by the identity that admitted it.
        let supervisor = supervisor.clone();
        if let Err(error) = crate::platform::watch_process_exit(
            pid,
            start,
            Box::new(move || {
                supervisor.reconcile_handoff_admissions(Some((pid, Some(start))));
            }),
        ) {
            tracing::warn!(%error, "cannot watch the handoff source's admission cleanup");
        }
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

/// Forgets the repositories found so far: a registration
/// ([`home_repo::register`]) changes what some directories name.
pub(crate) fn forget_repositories() {
    lock(&REPOSITORIES).clear();
}

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
