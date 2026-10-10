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
pub(crate) mod coordinators;
mod history;
#[cfg(test)]
mod install_script_tests;
mod item_coordinators;
mod log;
mod policy;
mod pre_tool_checks;
pub(crate) mod reports;
mod review;
mod runs;
mod slot;
mod store;
#[cfg(test)]
mod tests;
mod todo_titles;
mod verify;

pub(crate) use coordinators::coordinators;
#[cfg(test)]
pub(crate) use coordinators::set_test_coordinators;
pub(crate) use item_coordinators::resume_item_coordinators_at_start;
pub(crate) use log::log_lines;
pub(crate) use runs::resume_runs_at_start;
#[cfg(unix)]
pub(crate) use runs::{envs_for_handoff, restore_handed_off_envs};

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::api::schema::{
    WorkerAnswerParams, WorkerAttentionReason, WorkerChoiceQuestion, WorkerCommandTarget,
    WorkerDecision, WorkerDenyAndStopParams, WorkerDrain, WorkerDrainAction, WorkerInfo,
    WorkerInterruptParams, WorkerItemRuns, WorkerKillParams, WorkerKillReport, WorkerObligation,
    WorkerPromptParams, WorkerQuestion, WorkerQuestionDetail, WorkerQuestionKind,
    WorkerQuestionState, WorkerRun, WorkerRunOutcome, WorkerRunsParams, WorkerSettledQuestion,
    WorkerStartParams, WorkerState, WorkerTurnResult, WorkerVerification, WorkerVerifyParams,
    WorkerWaitDrainedParams, WorkerWaitUntil,
};
use crate::platform::Signal;

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

#[derive(Debug)]
pub(crate) enum WorkerError {
    NotFound(String),
    Invalid(String),
    NotRunning(String),
    Busy(String),
    NoQuestion(String),
    /// The named question is no longer pending; the message says why.
    QuestionGone(String),
    /// A kill of an exited or lost worker without `force`; the message says
    /// what it would signal.
    NeedsForce(String),
    /// An interrupt named a turn that has already ended.
    TurnEnded(String),
    Unsupported(String),
    /// A command id was reused with another method or other parameters.
    CommandConflict(String),
    /// A command id whose command a server restart cut off before its
    /// outcome was stored.
    CommandInterrupted(String),
    /// The refusal a command id got the first time, returned again.
    Replayed(&'static str, String),
    /// The repository has an active coordinator, or the pane coordinates
    /// another one; the message names it.
    CoordinatorActive(String),
    CoordinatorNotFound(String),
    /// New turns are not admitted while the server drains for an install
    /// ([`WorkerSupervisor::drain`]); the message names it.
    Draining(String),
    /// The worker's output since a server restart is not proven complete
    /// ([`Status::continuity_gap`]); the message names the gap.
    ContinuityGap(String),
    /// No `todo.run` run has that id.
    RunNotFound(String),
    /// A `todo.resume` named an event that is not the run's pending one.
    EventStale(String),
    /// The repository already has a run in progress; the message names it.
    RunActive(String),
    /// A `todo.resume` from a pane other than the run's owner, whose pane is
    /// still there; the message names the owner.
    RunOwnedElsewhere(String),
    /// A `todo.run`'s preflight refused it; the message says which check.
    Preflight(String),
    /// A new run or attempt refused while Claude's usage is high or
    /// unknown; the message names each window and its value, or the
    /// failed read of Claude's usage.
    UsageGate(String),
    /// `history.item` named an item without records.
    HistoryNotFound(String),
    /// No report has that id.
    ReportNotFound(String),
    /// `report.close` from a pane that does not hold the herdr
    /// repository's coordination tenure; the message says what it holds.
    ReportCloseRefused(String),
    Io(std::io::Error),
}

/// Every code a [`WorkerError`] has, so a stored refusal keeps its code.
const WORKER_ERROR_CODES: &[&str] = &[
    "worker_not_found",
    "invalid_request",
    "worker_not_running",
    "worker_busy",
    "worker_no_question",
    "worker_question_gone",
    "worker_needs_force",
    "worker_turn_ended",
    "worker_unsupported",
    "worker_command_conflict",
    "worker_command_interrupted",
    "worker_io_error",
    "coordinator_active",
    "coordinator_not_found",
    "workers_draining",
    "worker_continuity_gap",
    "todo_run_not_found",
    "todo_event_stale",
    "todo_run_active",
    "run_owned_elsewhere",
    "todo_preflight_failed",
    "usage_gate",
    "history_item_not_found",
    "report_not_found",
    "report_close_refused",
];

impl WorkerError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "worker_not_found",
            Self::Invalid(_) => "invalid_request",
            Self::NotRunning(_) => "worker_not_running",
            Self::Busy(_) => "worker_busy",
            Self::NoQuestion(_) => "worker_no_question",
            Self::QuestionGone(_) => "worker_question_gone",
            Self::NeedsForce(_) => "worker_needs_force",
            Self::TurnEnded(_) => "worker_turn_ended",
            Self::Unsupported(_) => "worker_unsupported",
            Self::CommandConflict(_) => "worker_command_conflict",
            Self::CommandInterrupted(_) => "worker_command_interrupted",
            Self::Replayed(code, _) => code,
            Self::Io(_) => "worker_io_error",
            Self::CoordinatorActive(_) => "coordinator_active",
            Self::CoordinatorNotFound(_) => "coordinator_not_found",
            Self::Draining(_) => "workers_draining",
            Self::ContinuityGap(_) => "worker_continuity_gap",
            Self::RunNotFound(_) => "todo_run_not_found",
            Self::EventStale(_) => "todo_event_stale",
            Self::RunActive(_) => "todo_run_active",
            Self::RunOwnedElsewhere(_) => "run_owned_elsewhere",
            Self::Preflight(_) => "todo_preflight_failed",
            Self::UsageGate(_) => "usage_gate",
            Self::HistoryNotFound(_) => "history_item_not_found",
            Self::ReportNotFound(_) => "report_not_found",
            Self::ReportCloseRefused(_) => "report_close_refused",
        }
    }

    /// A refusal stored with a command id, as it is returned again.
    fn replayed(code: &str, message: String) -> Self {
        let code = WORKER_ERROR_CODES
            .iter()
            .find(|known| **known == code)
            .copied()
            .unwrap_or("worker_io_error");
        Self::Replayed(code, message)
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "worker {id} not found"),
            Self::Invalid(message)
            | Self::NotRunning(message)
            | Self::Busy(message)
            | Self::NoQuestion(message)
            | Self::QuestionGone(message)
            | Self::NeedsForce(message)
            | Self::TurnEnded(message)
            | Self::Unsupported(message)
            | Self::CommandConflict(message)
            | Self::CommandInterrupted(message)
            | Self::Replayed(_, message)
            | Self::CoordinatorActive(message)
            | Self::CoordinatorNotFound(message)
            | Self::Draining(message)
            | Self::ContinuityGap(message)
            | Self::RunNotFound(message)
            | Self::EventStale(message)
            | Self::RunActive(message)
            | Self::RunOwnedElsewhere(message)
            | Self::Preflight(message)
            | Self::UsageGate(message)
            | Self::HistoryNotFound(message)
            | Self::ReportNotFound(message)
            | Self::ReportCloseRefused(message) => f.write_str(message),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl From<std::io::Error> for WorkerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// What a journal line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// A line the CLI wrote to stdout.
    Out,
    /// A line herdr wrote to the CLI's stdin.
    In,
    /// Herdr's own record (start, permission decisions, sessions, exit).
    Herdr,
    /// A line the CLI wrote to stderr.
    Err,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::In => "in",
            Self::Herdr => "herdr",
            Self::Err => "err",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "out" => Self::Out,
            "in" => Self::In,
            "herdr" => Self::Herdr,
            "err" => Self::Err,
            _ => return None,
        })
    }
}

/// A worker's state, folded from its journal events by [`Status::apply`].
#[derive(Debug, Clone)]
struct Status {
    worker_id: String,
    state: WorkerState,
    cwd: String,
    name: String,
    workspace_id: Option<String>,
    model: Option<String>,
    pid: Option<u32>,
    session_id: Option<String>,
    turns: u32,
    last_result: Option<WorkerTurnResult>,
    rate_limit: Option<Value>,
    /// Each recorded tool session with its leader's start token
    /// ([`crate::platform::process_start_token`]); `None` for a session a
    /// journal recorded before herdr kept it, which `kill` does not trust.
    tool_sessions: BTreeMap<u32, Option<u64>>,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    /// Requests left to the user, oldest first.
    questions: Vec<Pending>,
    /// The most recently settled questions, oldest first, with what ended
    /// each, so an answer to one of them says what happened to it.
    resolved: VecDeque<(String, String)>,
    stop_requested_ms: Option<u64>,
    /// Its owner sent a stop or kill (a `signal` with `by_owner`): the exit
    /// that follows is acknowledged as it is recorded ([`Self::mark_seq`]),
    /// so the owner owes nothing for an end it asked for. Not stored: after
    /// a restart in between, the exit is an obligation, as one nobody asked
    /// for.
    stop_by_owner: bool,
    /// The takeover claim: set by `takeover`, cleared by `takeover_failed`.
    /// Kept in every state, an exited worker's too.
    takeover_ms: Option<u64>,
    /// The claim's unique id (`takeover`'s `takeover_id`), which its tab
    /// carries in `HERDR_TAKEOVER_ID` and its title, so a restart can find
    /// the tab. None for a claim recorded before the id.
    takeover_id: Option<String>,
    /// The tab a takeover opened (`takeover_tab_opened`).
    takeover_tab: Option<String>,
    /// Why the last takeover failed (`takeover_failed`).
    takeover_error: Option<String>,
    /// A claim replayed from the journal without its tab: the server that
    /// made it ended before recording the tab.
    takeover_unfinished: bool,
    /// The category of a model refusal in the running turn
    /// (`system/model_refusal_no_fallback`); it fails the turn.
    refusal: Option<String>,
    /// The process's exit is recorded. The state is then `exited`, or
    /// `failed` when the exit failed a finished turn.
    exited: bool,
    /// A later server recorded the worker `lost`: the server that ran it
    /// ended first. The state is then `lost`, or the last turn's end state
    /// when that turn had ended (with `end_note`). A later `exited` replaces
    /// it.
    lost: bool,
    /// Why a worker that keeps its last turn's state is gone.
    end_note: Option<String>,
    /// The folder slot it runs in, if any.
    slot: Option<String>,
    /// The `seq` of its latest event: the store's, or, after a store write
    /// failed, one counted on in this server's memory, so waits still see
    /// each event as new ([`WorkerSupervisor::commit_locked`]).
    last_seq: i64,
    /// The `seq` of the user message that began the current or last turn.
    turn_seq: Option<i64>,
    /// The `seq` of the event that last ended a turn (a `result`, the exit,
    /// `lost`); meaningful while [`Status::turn_ended`].
    turn_end_seq: i64,
    /// The `seq` of the event that made the worker gone; meaningful while
    /// [`Status::is_gone`].
    gone_seq: i64,
    /// Why the worker's record is incomplete: a store or journal write
    /// failed. Not an event: it is stored with the worker's row the next
    /// time the store takes a write, and a reopen finds a gap the store
    /// never learned of in the journal export ([`note_journal_gap`]).
    degraded: Option<String>,
    /// The pane and agent session that started it (`started`'s `owner`).
    owner_pane: Option<String>,
    owner_session: Option<String>,
    /// The coordination tenure the owner pane was bound to then
    /// (`started`'s `owner.coordinator_id`).
    owner_coordinator: Option<String>,
    /// The highest `seq` its owner acknowledged (`acked`); only grows.
    acked_seq: i64,
    /// Why its owner is gone for good (`owner_gone`: its pane closed, its
    /// agent exited): every question, pending or later, goes to the user.
    owner_gone: Option<String>,
    /// The TODO item it works on (`started`'s `item`).
    item: Option<String>,
    /// That item's title in its repository's `TODO.md` when the worker
    /// started (`started`'s `item_title`): a finished item leaves the file,
    /// and its runs keep the title.
    item_title: Option<String>,
    /// The repository of its directory (`started`'s `repo`): the parent of
    /// its git common directory.
    repo: Option<String>,
    /// When its `started` and the event that made it gone were recorded,
    /// in Unix milliseconds ([`Self::mark_seq`]).
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
    /// The shas its turns' results named on `WORKER-DONE <sha>` lines.
    done_commits: Vec<String>,
    /// How many questions it asked.
    questions_asked: u32,
    /// It exited or was lost while a turn ran.
    ended_mid_turn: bool,
    /// The latest `worker.verify` of its work (`verification`).
    verification: Option<WorkerVerification>,
    /// The broker that owns its pipes (`started`'s `broker`); none for a
    /// worker whose pipes the server owned.
    broker: Option<BrokerRecord>,
    /// The broker seq of the last line of its output that is stored
    /// ([`WorkerSupervisor::record_spooled`]); 0 before any.
    broker_seq: u64,
    /// Why a re-attach could not prove that the stored record holds every
    /// line the worker wrote (`continuity_gap`'s `reason`). Its state and
    /// session may then be stale, so it takes no prompt and no takeover.
    continuity_gap: Option<String>,
    /// The headless tenure it runs as (`started`'s `coordinator`): an item
    /// coordinator ([`item_coordinators`]), whose every question goes to
    /// the user at once.
    coordinates: Option<String>,
}

/// Where a worker's broker serves it, and what a server that re-attaches
/// needs to answer its permission requests again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
struct BrokerRecord {
    pid: u32,
    socket: PathBuf,
    /// The worker's directory as asked for, before resolving links (its
    /// resolved one is [`Status::cwd`]).
    cwd: PathBuf,
    /// The pre-tool checks its hook runs ([`pre_tool_checks`]), as they
    /// were when it started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pre_tool_checks: Vec<Vec<String>>,
}

/// How many settled questions a worker remembers for `worker_question_gone`.
const RESOLVED_QUESTIONS_KEPT: usize = 32;

/// An open question with the tool input its answer is built from.
#[derive(Debug, Clone)]
struct Pending {
    question: WorkerQuestion,
    input: Value,
    /// The `seq` of the event that asked it.
    asked_seq: i64,
    /// Its answer is stored and being written (`answer_intent`): it is no
    /// longer answerable, and only the answer's outcome (`answer_sent`,
    /// `answer_failed`, `answer_expired`) settles it, never a cleanup.
    answering: bool,
    /// What ended its turn or its worker while its answer was in flight: a
    /// failed write then settles it so instead of making it pending again.
    cleared: Option<String>,
    /// Why it was handed to the user (`escalated`, `owner_gone`); once set it
    /// stays, whatever its owner does later.
    escalated: Option<String>,
}

/// What an event's `seq` marks, taken before the event is folded in
/// ([`Status::before`], [`Status::mark_seq`]).
struct Before {
    /// The open questions, each with whether its answer was in flight.
    pending: Vec<(String, bool)>,
    turn_ended: bool,
    gone: (bool, bool),
}

impl Status {
    fn new(worker_id: String) -> Self {
        Self {
            worker_id,
            state: WorkerState::Starting,
            cwd: String::new(),
            name: String::new(),
            workspace_id: None,
            model: None,
            pid: None,
            session_id: None,
            turns: 0,
            last_result: None,
            rate_limit: None,
            tool_sessions: BTreeMap::new(),
            exit_code: None,
            exit_signal: None,
            questions: Vec::new(),
            resolved: VecDeque::new(),
            stop_requested_ms: None,
            stop_by_owner: false,
            takeover_ms: None,
            takeover_id: None,
            takeover_tab: None,
            takeover_error: None,
            takeover_unfinished: false,
            refusal: None,
            exited: false,
            lost: false,
            end_note: None,
            slot: None,
            last_seq: 0,
            turn_seq: None,
            turn_end_seq: 0,
            gone_seq: 0,
            degraded: None,
            owner_pane: None,
            owner_session: None,
            owner_coordinator: None,
            acked_seq: 0,
            owner_gone: None,
            item: None,
            item_title: None,
            repo: None,
            started_ms: None,
            ended_ms: None,
            done_commits: Vec::new(),
            questions_asked: 0,
            ended_mid_turn: false,
            verification: None,
            broker: None,
            broker_seq: 0,
            continuity_gap: None,
            coordinates: None,
        }
    }

    /// Drops an answered or cancelled question, recording `how` it ended;
    /// the turn goes on once none is left.
    fn settle_question(&mut self, request_id: Option<&str>, how: &str) {
        if let Some(request_id) = request_id {
            let before = self.questions.len();
            self.questions
                .retain(|pending| pending.question.request_id != request_id);
            if self.questions.len() < before {
                self.remember_resolved(request_id.to_owned(), how);
            }
        }
        if self.questions.is_empty() && self.state == WorkerState::WaitingApproval {
            self.state = WorkerState::Working;
        }
    }

    /// Drops every pending question, recording `how` they ended. One whose
    /// answer is in flight stays until the answer's outcome settles it
    /// (the cleanup guard): the answer may already be on its way.
    fn clear_questions(&mut self, how: &str) {
        let mut in_flight = Vec::new();
        for mut pending in std::mem::take(&mut self.questions) {
            if pending.answering {
                pending.cleared.get_or_insert_with(|| how.to_owned());
                in_flight.push(pending);
            } else {
                self.remember_resolved(pending.question.request_id, how);
            }
        }
        self.questions = in_flight;
    }

    /// Writing an answer failed: the question is pending again, or, when
    /// its turn or worker ended meanwhile, settled as that cleanup said.
    fn answer_failed(&mut self, request_id: Option<&str>) {
        let Some(pending) = self
            .questions
            .iter_mut()
            .find(|pending| Some(pending.question.request_id.as_str()) == request_id)
        else {
            return;
        };
        match pending.cleared.clone() {
            Some(how) => self.settle_question(request_id, &how),
            None => pending.answering = false,
        }
    }

    fn is_answering(&self, request_id: Option<&str>) -> bool {
        self.questions.iter().any(|pending| {
            pending.answering && Some(pending.question.request_id.as_str()) == request_id
        })
    }

    fn remember_resolved(&mut self, request_id: String, how: &str) {
        self.resolved.retain(|(id, _)| *id != request_id);
        if self.resolved.len() == RESOLVED_QUESTIONS_KEPT {
            self.resolved.pop_front();
        }
        self.resolved.push_back((request_id, how.to_owned()));
    }

    /// What ended a question that is no longer pending, if it is recent.
    fn resolution(&self, request_id: &str) -> Option<&str> {
        self.resolved
            .iter()
            .find(|(id, _)| id == request_id)
            .map(|(_, how)| how.as_str())
    }

    /// The ids of the open questions, each with whether its answer is in
    /// flight, to tell what an event changed.
    fn open_questions(&self) -> Vec<(String, bool)> {
        self.questions
            .iter()
            .map(|pending| (pending.question.request_id.clone(), pending.answering))
            .collect()
    }

    fn before(&self) -> Before {
        Before {
            pending: self.open_questions(),
            turn_ended: self.turn_ended(),
            gone: (self.exited, self.lost),
        }
    }

    /// Marks what the event just folded in, which got `seq` and was
    /// recorded at `ts_ms`, changed: the questions it asked, the start, the
    /// turn it began, ended or the end it brought.
    fn mark_seq(
        &mut self,
        seq: i64,
        ts_ms: u64,
        before: &Before,
        direction: Direction,
        record: &store::Recorded<'_>,
    ) {
        self.last_seq = seq;
        for pending in &mut self.questions {
            if !before
                .pending
                .iter()
                .any(|(id, _)| *id == pending.question.request_id)
            {
                pending.asked_seq = seq;
            }
        }
        let kind = match record {
            store::Recorded::Event(event) => event.get("type").and_then(Value::as_str),
            store::Recorded::Raw(_) => None,
        };
        if (direction, kind) == (Direction::Herdr, Some("started")) {
            self.started_ms.get_or_insert(ts_ms);
        }
        if (direction, kind) == (Direction::In, Some("user")) && !self.is_gone() {
            self.turn_seq = Some(seq);
        }
        let result = (direction, kind) == (Direction::Out, Some("result"));
        if self.turn_ended() && (!before.turn_ended || result) {
            self.turn_end_seq = seq;
        }
        if self.is_gone() && (self.exited, self.lost) != before.gone {
            self.gone_seq = seq;
            self.ended_ms = Some(ts_ms);
            // The exit its owner asked for is handled already, with all
            // that came before it.
            if self.exited && self.stop_by_owner {
                self.acked_seq = self.acked_seq.max(seq);
            }
        }
    }

    /// Why a waiter that has seen everything up to `after` should look at
    /// the worker now, if it should: a pending question asked after it, a
    /// turn ended after it, or the worker's end, which counts however old,
    /// since nothing can follow it.
    fn attention(&self, after: Option<i64>) -> Option<WorkerAttentionReason> {
        let after = after.unwrap_or(i64::MIN);
        if self.is_gone() {
            Some(WorkerAttentionReason::Gone)
        } else if self
            .questions
            .iter()
            .any(|pending| pending.asked_seq > after)
        {
            Some(WorkerAttentionReason::Question)
        } else if self.turn_ended() && self.turn_end_seq > after {
            Some(WorkerAttentionReason::TurnEnd)
        } else {
            None
        }
    }

    /// What the owner has to handle that it has not acknowledged: the
    /// worker's end after `acked_seq`, else the unanswered questions asked
    /// after it, else a turn that ended after it. Unlike [`Self::attention`]
    /// the end counts only once acknowledged, and a question whose answer
    /// is in flight is handled already.
    fn obligation(&self) -> Option<(WorkerAttentionReason, Vec<WorkerQuestion>)> {
        let acked = self.acked_seq;
        if self.is_gone() {
            return (self.gone_seq > acked).then(|| (WorkerAttentionReason::Gone, Vec::new()));
        }
        let questions: Vec<WorkerQuestion> = self
            .questions
            .iter()
            .filter(|pending| !pending.answering && pending.asked_seq > acked)
            .map(Pending::shown)
            .collect();
        if !questions.is_empty() {
            Some((WorkerAttentionReason::Question, questions))
        } else if self.turn_ended() && self.turn_end_seq > acked {
            Some((WorkerAttentionReason::TurnEnd, Vec::new()))
        } else {
            None
        }
    }

    /// Whether a pending question waits quietly for the worker's owner: it
    /// has one, and nothing has handed the question to the user yet.
    fn is_quiet(&self, pending: &Pending) -> bool {
        self.owner_pane.is_some() && pending.escalated.is_none()
    }

    /// The ids of the questions that still wait quietly for the owner, whose
    /// answer is not in flight.
    fn quiet_questions(&self) -> Vec<String> {
        self.questions
            .iter()
            .filter(|pending| !pending.answering && self.is_quiet(pending))
            .map(|pending| pending.question.request_id.clone())
            .collect()
    }

    /// A takeover claimed by a server that is gone without recording its
    /// tab: whether it opened the tab is unknown.
    fn mark_unfinished_takeover(&mut self) {
        self.takeover_unfinished = self.takeover_ms.is_some() && self.takeover_tab.is_none();
    }

    fn is_gone(&self) -> bool {
        self.exited || self.lost
    }

    /// An ended worker whose owner has not acknowledged its end yet: its
    /// handoff is open, so its result still waits for review.
    fn end_unacked(&self) -> bool {
        self.is_gone() && self.owner_pane.is_some() && self.gone_seq > self.acked_seq
    }

    /// Whether the sidebar lists the worker: while it runs, and after its
    /// end until its owner acknowledges that end. A worker without an owner
    /// leaves at its end: nobody would acknowledge it, and its run stays in
    /// its item's history (`worker.runs`).
    fn listed(&self) -> bool {
        !self.is_gone() || self.end_unacked()
    }

    /// Whether the worker's process is known to be gone: its exit is
    /// recorded, or it was lost and its process group no longer exists.
    fn process_gone(&self) -> bool {
        self.exited
            || (self.lost
                && self
                    .pid
                    .is_none_or(|pid| !crate::platform::process_group_alive(pid)))
    }

    /// Whether a turn runs: the first has not ended yet, or a prompt began
    /// another after the last one ended.
    fn mid_turn(&self) -> bool {
        !matches!(
            self.state,
            WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
        )
    }

    fn turn_ended(&self) -> bool {
        self.is_gone()
            || matches!(
                self.state,
                WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
            )
    }

    fn apply(&mut self, direction: Direction, event: &Value) {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        // A takeover goes on after the worker has exited (its tab opens
        // then), so its events count in every state.
        if direction == Direction::Herdr {
            match kind {
                "takeover" => {
                    self.takeover_ms = Some(event["at_ms"].as_u64().unwrap_or(0));
                    self.takeover_id = string_field(event, "takeover_id");
                    self.takeover_tab = None;
                    self.takeover_error = None;
                    self.takeover_unfinished = false;
                    return;
                }
                "takeover_failed" => {
                    self.takeover_ms = None;
                    self.takeover_id = None;
                    self.takeover_error = Some(
                        string_field(event, "error").unwrap_or_else(|| "unknown error".into()),
                    );
                    self.takeover_unfinished = false;
                    return;
                }
                "takeover_tab_opened" => {
                    self.takeover_tab = Some(string_field(event, "tab_id").unwrap_or_default());
                    self.takeover_unfinished = false;
                    return;
                }
                // An answer's outcome also comes after the turn or the
                // worker ended while it was in flight.
                "answer_sent" => {
                    self.settle_question(event["request_id"].as_str(), "answered");
                    return;
                }
                "answer_failed" => {
                    self.answer_failed(event["request_id"].as_str());
                    return;
                }
                "answer_expired" => {
                    let how = string_field(event, "how").unwrap_or_else(|| "expired".into());
                    self.settle_question(event["request_id"].as_str(), &how);
                    return;
                }
                // Verified after the worker ended, as it usually is.
                "verification" => {
                    self.verification = serde_json::from_value(event["verification"].clone()).ok();
                    return;
                }
                // Its coordinator resumed in another pane or handed off:
                // the new owner handles its events, a gone worker's end
                // too.
                "owner_moved" => {
                    self.owner_pane = string_field(event, "pane_id");
                    self.owner_session = string_field(event, "session_id");
                    self.owner_coordinator = string_field(event, "coordinator_id");
                    self.owner_gone = None;
                    return;
                }
                // The owner handles a gone worker's end too.
                "acked" => {
                    let seq = event["seq"].as_i64().unwrap_or(0);
                    self.acked_seq = self.acked_seq.max(seq);
                    return;
                }
                _ => {}
            }
        }
        // An exit recorded after `lost` (by the server that still ran the
        // worker) replaces it.
        let exit_after_lost =
            self.lost && !self.exited && (direction, kind) == (Direction::Herdr, "exited");
        if self.is_gone() && !exit_after_lost {
            return;
        }
        match (direction, kind) {
            (Direction::Herdr, "started") => {
                self.cwd = string_field(event, "cwd").unwrap_or_default();
                self.name = string_field(event, "name").unwrap_or_default();
                self.workspace_id = string_field(event, "workspace_id");
                self.model = string_field(event, "model");
                self.slot = event["folder_slot"]["name"].as_str().map(str::to_owned);
                self.owner_pane = event["owner"]["pane_id"].as_str().map(str::to_owned);
                self.owner_session = event["owner"]["session_id"].as_str().map(str::to_owned);
                self.owner_coordinator =
                    event["owner"]["coordinator_id"].as_str().map(str::to_owned);
                self.item = string_field(event, "item");
                self.item_title = string_field(event, "item_title");
                self.repo = string_field(event, "repo");
                self.coordinates = event["coordinator"]["coordinator_id"]
                    .as_str()
                    .map(str::to_owned);
                self.pid = event
                    .get("pid")
                    .and_then(Value::as_u64)
                    .map(|pid| pid as u32);
                self.broker = event
                    .get("broker")
                    .and_then(|broker| serde_json::from_value(broker.clone()).ok());
            }
            (Direction::Herdr, "continuity_gap") => {
                let reason = string_field(event, "reason").unwrap_or_default();
                self.degraded = Some(format!("output continuity not proven: {reason}"));
                self.continuity_gap = Some(reason);
            }
            (Direction::Herdr, "tool_sessions") => {
                for recorded in event["sessions"].as_array().into_iter().flatten() {
                    // Older journals list bare session ids.
                    let (session, leader_start) = match recorded.as_u64() {
                        Some(session) => (session, None),
                        None => match recorded["session"].as_u64() {
                            Some(session) => (session, recorded["leader_start"].as_u64()),
                            None => continue,
                        },
                    };
                    let known = self.tool_sessions.entry(session as u32).or_default();
                    if known.is_none() {
                        *known = leader_start;
                    }
                }
            }
            (Direction::Herdr, "question") => {
                let question = event
                    .get("question")
                    .cloned()
                    .and_then(|question| serde_json::from_value(question).ok());
                if let Some(question) = question {
                    self.questions_asked += 1;
                    self.questions.push(Pending {
                        question,
                        input: event.get("input").cloned().unwrap_or_else(|| json!({})),
                        asked_seq: 0,
                        answering: false,
                        cleared: None,
                        escalated: self.owner_gone.clone().or_else(|| {
                            self.coordinates
                                .as_ref()
                                .map(|_| item_coordinators::QUESTION_TO_USER.to_owned())
                        }),
                    });
                }
            }
            // A journal from before the answer outbox settled the question
            // with its answer.
            (Direction::Herdr, "answer") => {
                self.settle_question(event["request_id"].as_str(), "answered");
            }
            (Direction::Herdr, "answer_intent") => {
                let request_id = event["request_id"].as_str();
                if let Some(pending) = self
                    .questions
                    .iter_mut()
                    .find(|pending| Some(pending.question.request_id.as_str()) == request_id)
                {
                    pending.answering = true;
                }
            }
            (Direction::Herdr, "escalated") => {
                let cause = string_field(event, "cause").unwrap_or_else(|| "escalated".into());
                let ids: Vec<&str> = event["request_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                for pending in &mut self.questions {
                    if ids.contains(&pending.question.request_id.as_str()) {
                        pending.escalated.get_or_insert_with(|| cause.clone());
                    }
                }
            }
            (Direction::Herdr, "owner_gone") => {
                let cause = string_field(event, "cause").unwrap_or_else(|| "owner gone".into());
                for pending in &mut self.questions {
                    pending.escalated.get_or_insert_with(|| cause.clone());
                }
                self.owner_gone.get_or_insert(cause);
            }
            (Direction::Herdr, "signal") => {
                if event["signal"].as_str() == Some("SIGTERM") {
                    self.stop_requested_ms = event["at_ms"].as_u64();
                }
                if event["by_owner"].as_bool() == Some(true) {
                    self.stop_by_owner = true;
                }
            }
            (Direction::Herdr, "exited") => {
                self.ended_mid_turn = self.mid_turn();
                let code = event.get("code").and_then(Value::as_i64);
                // A refused turn can end with `result/success` and then
                // exit code 1 (T3-1): that turn is `failed`, not `exited`.
                // Herdr's own stop and an interrupted last turn also exit
                // non-zero, but neither follows a `finished` turn.
                let failed_turn = match (
                    code.filter(|code| *code != 0),
                    self.state == WorkerState::Finished,
                    self.stop_requested_ms,
                ) {
                    (Some(code), true, None) => {
                        if let Some(result) = self.last_result.as_mut() {
                            result.failure.get_or_insert_with(|| {
                                format!("the CLI exited with code {code} after the turn")
                            });
                        }
                        true
                    }
                    _ => false,
                };
                self.clear_questions("the worker exited");
                self.stop_requested_ms = None;
                self.exited = true;
                self.lost = false;
                self.end_note = None;
                self.state = if failed_turn {
                    WorkerState::Failed
                } else {
                    WorkerState::Exited
                };
                self.exit_code = code.map(|v| v as i32);
                self.exit_signal = event
                    .get("signal")
                    .and_then(Value::as_i64)
                    .map(|v| v as i32);
                // The broker ended the worker rather than lose its output.
                if let Some(error) = event["spool_error"].as_str() {
                    self.degraded = Some(format!(
                        "the worker's broker could not write its output spool and ended the \
                         worker: {error}"
                    ));
                }
            }
            (Direction::Herdr, "lost") => {
                // A worker between turns lost nothing but its process: it
                // keeps its last turn's state. A stop request stays, so a
                // later `exited` is judged as the stop's.
                self.ended_mid_turn = self.mid_turn();
                self.clear_questions("the worker was lost");
                self.lost = true;
                if matches!(
                    self.state,
                    WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
                ) {
                    self.end_note = Some("ended by a server restart".into());
                } else {
                    self.state = WorkerState::Lost;
                }
            }
            (Direction::Out, "system") => {
                if event.get("subtype").and_then(Value::as_str) == Some("model_refusal_no_fallback")
                {
                    self.refusal = Some(
                        string_field(event, "api_refusal_category")
                            .unwrap_or_else(|| "unknown".into()),
                    );
                }
                if event.get("subtype").and_then(Value::as_str) == Some("init") {
                    if let Some(session_id) = string_field(event, "session_id") {
                        self.session_id = Some(session_id);
                    }
                    if self.state == WorkerState::Starting {
                        self.state = WorkerState::Working;
                    }
                }
            }
            (Direction::Out, "control_request") => {
                if event["request"]["subtype"].as_str() == Some("can_use_tool") {
                    self.state = WorkerState::WaitingApproval;
                }
            }
            (Direction::In, "control_response") => {
                // An answer's own line: `answer_sent` settles it once written.
                let request_id = event["response"]["request_id"].as_str();
                if !self.is_answering(request_id) {
                    self.settle_question(request_id, "answered");
                }
            }
            (Direction::Out, "control_cancel_request") => {
                self.settle_question(event["request_id"].as_str(), "cancelled");
            }
            (Direction::In, "user") => {
                // A refusal stays until the turn's `result` takes it: a
                // message sent during the turn does not clear it.
                if self.state != WorkerState::Starting {
                    self.state = WorkerState::Working;
                }
            }
            (Direction::Out, "rate_limit_event") => {
                self.rate_limit = event.get("rate_limit_info").cloned();
            }
            (Direction::Out, "result") => {
                let result = WorkerTurnResult {
                    subtype: string_field(event, "subtype").unwrap_or_default(),
                    is_error: event
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    terminal_reason: string_field(event, "terminal_reason"),
                    api_error_status: event.get("api_error_status").and_then(Value::as_i64),
                    text: string_field(event, "result"),
                    failure: self
                        .refusal
                        .take()
                        .map(|category| format!("the model refused the turn ({category})")),
                    permission_denials: event["permission_denials"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                };
                self.state = turn_end_state(&result);
                for sha in result
                    .text
                    .as_deref()
                    .map(done_commits)
                    .into_iter()
                    .flatten()
                {
                    if !self.done_commits.contains(&sha) {
                        self.done_commits.push(sha);
                    }
                }
                self.clear_questions("its turn ended");
                self.turns += 1;
                self.last_result = Some(result);
            }
            _ => {}
        }
    }

    fn info(&self, journal_path: &Path) -> WorkerInfo {
        WorkerInfo {
            worker_id: self.worker_id.clone(),
            state: self.state,
            cwd: self.cwd.clone(),
            name: self.name.clone(),
            workspace_id: self.workspace_id.clone(),
            model: self.model.clone(),
            pid: self.pid,
            session_id: self.session_id.clone(),
            turns: self.turns,
            last_result: self.last_result.clone(),
            rate_limit: self.rate_limit.clone(),
            tool_sessions: self.tool_sessions.keys().copied().collect(),
            exit_code: self.exit_code,
            exit_signal: self.exit_signal,
            questions: self.questions.iter().map(Pending::shown).collect(),
            settled_questions: self
                .resolved
                .iter()
                .map(|(request_id, how)| WorkerSettledQuestion {
                    request_id: request_id.clone(),
                    state: settled_state(how),
                    how: how.clone(),
                })
                .collect(),
            stop_requested_ms: self.stop_requested_ms,
            takeover_ms: self.takeover_ms,
            takeover_id: self.takeover_id.clone(),
            takeover_tab_id: self.takeover_tab.clone(),
            takeover_error: self.takeover_error.clone(),
            takeover_unfinished: self.takeover_unfinished,
            end_note: self.end_note.clone(),
            degraded: self.degraded.clone(),
            journal_path: journal_path.display().to_string(),
            seq: Some(self.last_seq),
            turn_seq: self.turn_seq,
            owner_pane_id: self.owner_pane.clone(),
            owner_session_id: self.owner_session.clone(),
            owner_coordinator_id: self.owner_coordinator.clone(),
            acked_seq: (self.acked_seq > 0).then_some(self.acked_seq),
            item: self.item.clone(),
            repo: self.repo.clone(),
            survives_handoff: self.broker.is_some() && !self.is_gone(),
        }
    }

    fn run(&self, journal_path: &Path) -> WorkerRun {
        // Judged by how its last turn ended, unless it ended in a turn.
        let last_turn = self.last_result.as_ref().map(turn_end_state);
        let outcome = if self.degraded.is_some() {
            WorkerRunOutcome::Degraded
        } else if !self.is_gone() {
            WorkerRunOutcome::Running
        } else if self.ended_mid_turn {
            if self.lost {
                WorkerRunOutcome::Lost
            } else {
                WorkerRunOutcome::Exited
            }
        } else if self.state == WorkerState::Failed || last_turn == Some(WorkerState::Failed) {
            WorkerRunOutcome::Failed
        } else if last_turn == Some(WorkerState::Finished) {
            WorkerRunOutcome::Finished
        } else if self.lost {
            WorkerRunOutcome::Lost
        } else {
            WorkerRunOutcome::Exited
        };
        WorkerRun {
            worker_id: self.worker_id.clone(),
            name: self.name.clone(),
            repo: self.repo.clone(),
            started_ms: self.started_ms,
            ended_ms: self.ended_ms,
            outcome,
            turns: self.turns,
            commits: self.done_commits.clone(),
            questions: self.questions_asked,
            journal_path: journal_path.display().to_string(),
            verification: self.verification.clone(),
        }
    }
}

impl Pending {
    /// The question as the API shows it, with where its answer is.
    fn shown(&self) -> WorkerQuestion {
        let mut question = self.question.clone();
        question.escalated = self.escalated.clone();
        question.state = if self.answering {
            WorkerQuestionState::Answering
        } else {
            WorkerQuestionState::Pending
        };
        question
    }
}

/// The state of a question that ended `how`.
fn settled_state(how: &str) -> WorkerQuestionState {
    match store::question_state(how) {
        "answered" => WorkerQuestionState::Answered,
        "cancelled" => WorkerQuestionState::Cancelled,
        _ => WorkerQuestionState::Expired,
    }
}

/// Judges a turn by its `result` and a model refusal in it (which can end
/// with `result/success`, T3-1); a non-zero exit code after a `finished`
/// turn is recorded in its `failure` when the process exits (it is also 1
/// after an interrupted last turn and 143 after SIGTERM).
fn turn_end_state(result: &WorkerTurnResult) -> WorkerState {
    if result.failure.is_some() {
        WorkerState::Failed
    } else if result
        .terminal_reason
        .as_deref()
        .is_some_and(|reason| reason.starts_with("aborted"))
    {
        WorkerState::Interrupted
    } else if result.subtype == "success" && !result.is_error {
        WorkerState::Finished
    } else {
        WorkerState::Failed
    }
}

/// The commit shas a turn's reply names on its `WORKER-DONE <sha> | ...`
/// lines (the worker's last line, by the coordinators' convention).
fn done_commits(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("WORKER-DONE "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|sha| (7..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .collect()
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

/// One worker's append-only JSONL journal: an export of its events for
/// `herdr worker log` and debugging, written after each event's store
/// commit. The store, not this file, is the worker's record.
struct Journal {
    file: Mutex<File>,
}

impl Journal {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Appends one record with the `seq` the store gave it, if any.
    fn export(
        &self,
        seq: Option<i64>,
        ts_ms: u64,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> std::io::Result<()> {
        let mut line = json!({"ts_ms": ts_ms, "dir": direction.as_str()});
        if let Some(seq) = seq {
            line["seq"] = json!(seq);
        }
        match record {
            store::Recorded::Event(event) => line["event"] = event.clone(),
            store::Recorded::Raw(raw) => line["raw"] = json!(raw),
        }
        let mut line = line.to_string();
        line.push('\n');
        lock(&self.file).write_all(line.as_bytes())
    }
}

/// One line of a JSONL journal.
struct JournalLine {
    /// The `seq` the store gave the record, when it was exported with one.
    seq: Option<i64>,
    ts_ms: u64,
    direction: Direction,
    /// The event, or the line the CLI wrote that was not JSON.
    record: Result<Value, String>,
}

/// Reads a journal's records. A line that is not a record (torn, unknown)
/// is skipped; bytes that are not UTF-8 are replaced, so one torn character
/// loses only its line.
fn read_journal(path: &Path) -> std::io::Result<Vec<JournalLine>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter_map(parse_journal_line)
        .collect())
}

fn parse_journal_line(line: &[u8]) -> Option<JournalLine> {
    let record = serde_json::from_str::<Value>(&String::from_utf8_lossy(line)).ok()?;
    let direction = record["dir"].as_str().and_then(Direction::parse)?;
    let record_value = match (record.get("event"), record["raw"].as_str()) {
        (Some(event), _) => Ok(event.clone()),
        (None, Some(raw)) => Err(raw.to_owned()),
        (None, None) => return None,
    };
    Some(JournalLine {
        seq: record["seq"].as_i64(),
        ts_ms: record["ts_ms"].as_u64().unwrap_or(0),
        direction,
        record: record_value,
    })
}

/// How many records at the end of a worker's journal export the store does
/// not hold: those after the last one exported with a `seq`, since only a
/// failed store write exports a record without one once the store exists.
/// Only that tail is parsed. A journal with no `seq` at all (written before
/// the store, then imported) is compared by count with the stored events.
fn journal_gap(store: &store::Store, worker_id: &str, path: &Path) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let mut trailing = 0;
    for line in bytes.rsplit(|byte| *byte == b'\n') {
        let Some(record) = parse_journal_line(line) else {
            continue;
        };
        if record.seq.is_some() {
            return Ok(trailing);
        }
        trailing += 1;
    }
    let stored = store
        .event_count(worker_id)
        .map_err(|error| error.to_string())?;
    Ok(trailing.saturating_sub(usize::try_from(stored).unwrap_or(0)))
}

/// Marks a worker loaded from the store degraded when its journal export
/// holds events the store missed: a store write failed and no later write
/// stored the mark before that server ended. A mark the store holds wins.
fn note_journal_gap(store: &store::Store, status: &mut Status, path: &Path) {
    if status.degraded.is_some() || !path.exists() {
        return;
    }
    match journal_gap(store, &status.worker_id, path) {
        Ok(0) => {}
        Ok(missing) => {
            warn!(
                worker_id = status.worker_id,
                missing, "worker journal holds events the store does not"
            );
            status.degraded = Some(format!(
                "the worker store is missing the last {missing} event(s) of this worker that \
                 its journal export holds: a store write failed"
            ));
        }
        Err(error) => {
            warn!(%error, path = %path.display(), "worker journal unreadable");
        }
    }
}

/// Folds a journal into a state in memory, without the store. A record
/// exported without a `seq` counts on from the one before it.
fn replay_journal(worker_id: &str, path: &Path) -> std::io::Result<Status> {
    let mut status = Status::new(worker_id.to_owned());
    for line in read_journal(path)? {
        let before = status.before();
        let record = match &line.record {
            Ok(event) => {
                status.apply(line.direction, event);
                store::Recorded::Event(event)
            }
            Err(raw) => store::Recorded::Raw(raw),
        };
        let seq = line
            .seq
            .filter(|seq| *seq > status.last_seq)
            .unwrap_or(status.last_seq + 1);
        status.mark_seq(seq, line.ts_ms, &before, line.direction, &record);
    }
    status.mark_unfinished_takeover();
    Ok(status)
}

/// Imports a journal written before the store (or while it failed) into
/// it, in one transaction: its events in order, each with the questions it
/// asked or settled, then the worker's row. A worker the store already
/// holds is never imported again, since its row exists.
fn import_journal(store: &store::Store, worker_id: &str, path: &Path) -> Result<Status, String> {
    let lines = read_journal(path).map_err(|error| error.to_string())?;
    let mut status = Status::new(worker_id.to_owned());
    store
        .transaction(|tx| {
            let mut seq = 0;
            for line in &lines {
                let before = status.before();
                let record = match &line.record {
                    Ok(event) => {
                        status.apply(line.direction, event);
                        store::Recorded::Event(event)
                    }
                    Err(raw) => store::Recorded::Raw(raw),
                };
                seq = tx.event(&store::EventRow {
                    worker_id,
                    direction: line.direction,
                    record: &record,
                    ts_ms: line.ts_ms,
                })?;
                tx.questions(seq, &before.pending, &status)?;
                status.mark_seq(seq, line.ts_ms, &before, line.direction, &record);
            }
            tx.worker(&status, seq)
        })
        .map_err(|error| error.to_string())?;
    status.mark_unfinished_takeover();
    Ok(status)
}

/// The lock file beside a worker's journal, held by the server that runs
/// the worker.
fn owner_lock_path(journal_path: &Path) -> PathBuf {
    journal_path.with_extension("lock")
}

/// Takes the lock of a new worker's journal for this server; taken before
/// the journal exists, so another server never sees the journal unowned.
fn own_journal(journal_path: &Path, worker_id: &str) -> Result<File, WorkerError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(owner_lock_path(journal_path))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(WorkerError::Busy(format!(
            "worker {worker_id} is run by another server"
        ))),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Who owns a journal found at start.
enum Ownership {
    /// No server runs the worker; the lock, if the journal has one, is
    /// held while the journal is replayed.
    Unowned { _held: Option<File> },
    /// Another server holds the lock: its worker may still run.
    Owned(File),
}

fn journal_ownership(journal_path: &Path) -> std::io::Result<Ownership> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(owner_lock_path(journal_path))
    {
        Ok(file) => file,
        // Written by a server from before the lock files.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Ownership::Unowned { _held: None })
        }
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => Ok(Ownership::Unowned { _held: Some(file) }),
        Err(TryLockError::WouldBlock) => Ok(Ownership::Owned(file)),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Where a running worker's stdin lines go.
enum Input {
    /// The pipe this server owns (a worker started without a broker).
    Pipe(ChildStdin),
    /// The worker's broker, which owns the pipe ([`broker`]).
    #[cfg(unix)]
    Broker(broker::Writer),
}

impl Input {
    /// Writes one line, which ends with its newline. A broker writes it to
    /// the worker only if no line with `id` went before ([`input_id`]).
    fn write_line(&mut self, id: &str, line: &str) -> std::io::Result<()> {
        match self {
            Self::Pipe(pipe) => {
                let _ = id;
                pipe.write_all(line.as_bytes()).and_then(|()| pipe.flush())
            }
            #[cfg(unix)]
            Self::Broker(writer) => writer.write_input(id, line),
        }
    }

    /// Closes the worker's stdin.
    fn close(self) {
        match self {
            Self::Pipe(pipe) => drop(pipe),
            #[cfg(unix)]
            Self::Broker(writer) => writer.close_input(),
        }
    }
}

/// A worker's process as [`WorkerSupervisor::spawn_worker`] started it.
struct Spawned {
    pid: u32,
    input: Input,
    source: Source,
    /// Its broker's pid and socket, when it has one.
    broker: Option<(u32, PathBuf)>,
}

/// Where a worker's output comes from.
enum Source {
    Pipes {
        stdout: std::process::ChildStdout,
        stderr: std::process::ChildStderr,
        child: Child,
    },
    #[cfg(unix)]
    Broker {
        messages: broker::Messages,
        /// The broker, this server's child.
        process: Child,
    },
}

/// The id a worker's broker knows a stdin line by, so it writes the line at
/// most once ([`broker`]): a `control_response` goes by its request's id
/// (an answer sent again after a re-attach is the same line), a client
/// command by its receipt's id, anything else by its event's `seq`.
fn input_id(event: &Value, receipt: Option<&Receipt>, seq: i64) -> String {
    if event["type"].as_str() == Some("control_response") {
        if let Some(request_id) = event["response"]["request_id"].as_str() {
            return format!("r:{request_id}");
        }
    }
    match receipt {
        Some(receipt) => format!("c:{}", receipt.command_id),
        None => format!("s:{seq}"),
    }
}

/// A running worker's input; absent for one loaded from the store.
struct Live {
    number: u64,
    stdin: Arc<Mutex<Option<Input>>>,
    /// This server let go of the worker's broker, handing the worker to the
    /// next server (a live handoff), or a test cut it off as a server that
    /// died would be: its reader ends without recording an exit.
    #[cfg(unix)]
    detached: std::sync::atomic::AtomicBool,
    /// Fails the next write to the pipe, as a broken pipe would.
    #[cfg(test)]
    fail_next_write: std::sync::atomic::AtomicBool,
    /// Holds the next write to the pipe: it signals the first channel when
    /// reached and writes once the second gets a message, so a test can act
    /// between a recorded line and its write (a caller dying mid-answer).
    #[cfg(test)]
    hold_next_write: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

impl Live {
    fn new(number: u64, stdin: Input) -> Self {
        Self {
            number,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            #[cfg(unix)]
            detached: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_write: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            hold_next_write: Mutex::new(None),
        }
    }

    fn send(&self, supervisor: &WorkerSupervisor, event: &Value) -> Result<i64, WorkerError> {
        self.send_checked(supervisor, event, None, |_, _| Ok(()))
    }

    fn write_line(&self, pipe: &mut Input, id: &str, line: &str) -> std::io::Result<()> {
        #[cfg(test)]
        if self
            .fail_next_write
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "forced write failure",
            ));
        }
        #[cfg(test)]
        if let Some((reached, release)) = lock(&self.hold_next_write).take() {
            let _ = reached.send(());
            let _ = release.recv();
        }
        pipe.write_line(id, line)
    }

    /// Records one line for the CLI, then writes it, both under the input
    /// lock, so the recorded order of inputs is the order the CLI saw and
    /// the line is folded in before anything the CLI answers it with (a
    /// fast `result` folded first would be undone by its own prompt).
    /// `check` runs on the worker's status under the registry lock, in the
    /// same hold as the record, so two senders cannot both pass it; when it
    /// refuses, nothing is recorded or sent. A failed write is recorded as
    /// `input_failed` naming the line's `seq`. Returns that `seq`. A
    /// command's `receipt` is reserved with the line's record.
    fn send_checked(
        &self,
        supervisor: &WorkerSupervisor,
        event: &Value,
        receipt: Option<&Receipt>,
        check: impl FnOnce(&Status, &Registry) -> Result<(), WorkerError>,
    ) -> Result<i64, WorkerError> {
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return Err(WorkerError::NotRunning(
                "the worker's input is closed".into(),
            ));
        };
        let committed = {
            let mut registry = lock(&supervisor.shared.registry);
            let Some(entry) = registry.workers.get(&self.number) else {
                return Err(WorkerError::NotFound(format!("w{}", self.number)));
            };
            check(&entry.status, &registry)?;
            supervisor.commit_command_locked(
                &mut registry,
                self.number,
                Direction::In,
                store::Recorded::Event(event),
                receipt,
            )
        };
        supervisor.shared.changed.notify_all();
        if committed.shown_changed {
            notify_clients();
        }
        let seq = committed.seq.unwrap_or_default();
        let mut line = event.to_string();
        line.push('\n');
        if let Err(error) = self.write_line(pipe, &input_id(event, receipt, seq), &line) {
            supervisor.record(
                self.number,
                Direction::Herdr,
                &json!({"type": "input_failed", "seq": seq, "error": error.to_string()}),
            );
            return Err(error.into());
        }
        Ok(seq)
    }

    /// Writes `event` again, unrecorded, for a worker whose broker writes
    /// a line with `id` at most once: a line a gone server recorded (or
    /// meant to) and may or may not have sent. What the broker did comes
    /// back as an `input_written` event.
    #[cfg(unix)]
    fn resend(&self, event: &Value, id: &str) -> std::io::Result<()> {
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the worker's input is closed",
            ));
        };
        let mut line = event.to_string();
        line.push('\n');
        self.write_line(pipe, id, &line)
    }

    fn close_input(&self) {
        if let Some(input) = lock(&self.stdin).take() {
            input.close();
        }
    }

    /// Lets go of the worker's broker without closing the worker's input:
    /// the reader ends without recording an exit and gives the journal's
    /// lock back, and the worker and its broker go on for the next server.
    /// Whether this server hands the worker over (a live handoff) or a test
    /// cuts it off as a server that died would be. False for a worker
    /// without a broker, which ends with this server.
    #[cfg(unix)]
    fn detach(&self) -> bool {
        let stdin = lock(&self.stdin);
        let Some(Input::Broker(writer)) = stdin.as_ref() else {
            return false;
        };
        self.detached
            .store(true, std::sync::atomic::Ordering::SeqCst);
        writer.detach();
        true
    }

    #[cfg(all(test, unix))]
    fn sever(&self) {
        self.detach();
    }
}

struct Entry {
    status: Status,
    journal_path: PathBuf,
    live: Option<Arc<Live>>,
    /// Another server runs the worker (the old server of a live handoff)
    /// and writes its projection; this one appends only events for it
    /// until that server lets go ([`WorkerSupervisor::adopt_when_released`]).
    foreign: bool,
    /// The JSONL export, opened on the first event this server records.
    export: Option<Arc<Journal>>,
}

impl Entry {
    /// Whether a live handoff keeps the worker running: its broker owns
    /// its pipes.
    fn survives_handoff(&self) -> bool {
        self.status.broker.is_some()
    }

    fn new(status: Status, journal_path: PathBuf) -> Self {
        Self {
            status,
            journal_path,
            live: None,
            foreign: false,
            export: None,
        }
    }
}

/// What `worker.wait` with `until: attention` returns
/// ([`WorkerSupervisor::wait_attention`]).
#[derive(Debug, Clone)]
pub(crate) struct Attention {
    pub(crate) reason: WorkerAttentionReason,
    pub(crate) questions: Vec<WorkerQuestion>,
    pub(crate) seq: i64,
    pub(crate) worker: WorkerInfo,
}

/// What [`WorkerSupervisor::commit_locked`] did.
/// What became of one line of a worker's output from its broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spooled {
    /// Its event is committed with its broker seq.
    Stored,
    /// The store already held it: a line sent again after a re-attach.
    Duplicate,
    /// Kept in memory only: the store write failed.
    NotStored,
    /// A blank line, recorded nowhere.
    Skipped,
}

impl Spooled {
    /// The store holds the line: the broker may forget it.
    fn is_stored(self) -> bool {
        matches!(self, Self::Stored | Self::Duplicate)
    }
}

/// The event that a re-attach could not prove the worker's output whole.
#[cfg(unix)]
fn continuity_gap_event(gap: &str) -> Value {
    json!({"type": "continuity_gap", "reason": gap})
}

/// The event for `lines` the broker dropped while its spool was full.
#[cfg(unix)]
fn output_lost(lines: u64) -> Value {
    json!({
        "type": "output_lost",
        "lines": lines,
        "reason": "the broker's spool was full while no server stored its lines",
    })
}

/// The event that records a stdin line the broker wrote to the worker, by
/// its [`input_id`]; `again`: it had written it before and did not now.
#[cfg(unix)]
fn input_written(id: &str, again: bool) -> Value {
    json!({"type": "input_written", "id": id, "again": again})
}

/// Workers' statuses after an `owner_moved` event written in a transaction
/// not committed yet, each with its number and the event's `seq`
/// ([`WorkerSupervisor::stage_owner_moves`]).
type StagedMoves = Vec<(u64, Status, i64)>;

struct Committed {
    /// What the clients show of the worker changed.
    shown_changed: bool,
    /// The event's `seq` in memory; `None` for an unknown worker.
    seq: Option<i64>,
    /// The store committed the event.
    stored: bool,
}

#[derive(Default)]
struct Registry {
    next_number: u64,
    workers: BTreeMap<u64, Entry>,
    /// The client command ids this server runs now, each with its method
    /// and parameters; a repeat waits for the outcome ([`WorkerSupervisor::command`]).
    commands: BTreeMap<String, (&'static str, String)>,
    /// Owner panes whose agent cannot handle a question now (it hit a limit,
    /// or is blocked on its own question to the user), with why: a question
    /// its workers ask meanwhile goes to the user at once. Only this
    /// server's memory: herdr re-evaluates the owners when it starts
    /// ([`owners_at_start`]).
    stuck_owners: BTreeMap<String, String>,
    /// When each owner pane last showed an event herdr saw (its agent's
    /// state, a limit, an ack), Unix milliseconds. Shown next to a quiet
    /// question; it decides nothing.
    owner_seen_ms: BTreeMap<String, u64>,
    /// Owner panes whose pane closed or whose agent exited, with why: a
    /// `todo.resume` from another pane takes over a run they own. Only this
    /// server's memory: herdr re-evaluates the owners when it starts
    /// ([`owners_at_start`]), the runs' owners among them.
    gone_owners: BTreeMap<String, String>,
    /// The drain before an install, while new turns are refused. Only this
    /// server's memory: the server a handoff starts does not drain.
    drain: Option<Drain>,
    /// Worker starts that passed the drain check and have not registered
    /// their worker (or failed) yet; a drain waits for them too.
    starts_admitted: u32,
}

/// What drains, and since when ([`WorkerSupervisor::drain`]).
#[derive(Debug, Clone)]
struct Drain {
    reason: String,
    started_ms: u64,
}

/// A worker start admitted past the drain check, counted in
/// [`Registry::starts_admitted`] until dropped.
struct StartAdmission<'a> {
    supervisor: &'a WorkerSupervisor,
}

impl Drop for StartAdmission<'_> {
    fn drop(&mut self) {
        let mut registry = lock(&self.supervisor.shared.registry);
        registry.starts_admitted = registry.starts_admitted.saturating_sub(1);
        drop(registry);
        self.supervisor.shared.changed.notify_all();
    }
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

fn worker_number(worker_id: &str) -> Option<u64> {
    worker_id.strip_prefix('w')?.parse().ok()
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

/// A notification a driver raised for the user (a todo run's stop reason),
/// which the server sends to its client shells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserNotice {
    pub(crate) title: String,
    pub(crate) body: String,
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

/// A question some worker waits on, with the worker it belongs to.
#[derive(Debug, Clone)]
pub(crate) struct PendingWorkerQuestion {
    pub(crate) worker_id: String,
    pub(crate) cwd: String,
    pub(crate) question: WorkerQuestion,
    /// It waits for the worker's owner, not for the user.
    pub(crate) quiet: bool,
    /// When the owner last showed an event herdr saw; display only.
    pub(crate) owner_seen_ms: Option<u64>,
}

/// Every pending question of the server's supervisor; empty when no
/// supervisor was opened (it is never opened just to answer this).
pub(crate) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    SUPERVISOR
        .get()
        .map(WorkerSupervisor::pending_questions)
        .unwrap_or_default()
}

/// What a worker owner's pane or agent did, as herdr's own events report
/// it ([`WorkerSupervisor::owner_event`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerEvent {
    /// The pane closed: the owner is gone for good.
    PaneClosed,
    /// The agent in it exited: the owner is gone for good.
    AgentExited,
    /// Its turn ended on a usage or credit limit (`pane.report_limit`).
    Limited,
    /// It waits on its own question to the user.
    Blocked,
    /// It went idle or done: a question still unanswered then was not
    /// answered in its turn (acknowledging is not answering).
    TurnEnded,
    /// It works again: it can handle new questions.
    Working,
}

impl OwnerEvent {
    /// What an owner pane's agent status change means: its agent left the
    /// pane (`released`), works again, is blocked, or went idle or done from
    /// a state that was neither (a turn's end; idle and done only differ in
    /// whether the user has seen it).
    pub(crate) fn from_agent_status(
        previous: crate::api::schema::AgentStatus,
        now: crate::api::schema::AgentStatus,
        released: bool,
    ) -> Option<Self> {
        use crate::api::schema::AgentStatus;
        let resting = |status| matches!(status, AgentStatus::Idle | AgentStatus::Done);
        if released {
            Some(Self::AgentExited)
        } else if previous == now {
            None
        } else if now == AgentStatus::Working {
            Some(Self::Working)
        } else if now == AgentStatus::Blocked {
            Some(Self::Blocked)
        } else if resting(now) && !resting(previous) {
            Some(Self::TurnEnded)
        } else {
            None
        }
    }

    /// Why it hands its workers' quiet questions to the user, if it does.
    fn cause(self) -> Option<&'static str> {
        Some(match self {
            Self::PaneClosed => "the coordinator's pane closed",
            Self::AgentExited => "the coordinator's agent exited",
            Self::Limited => "the coordinator hit a usage or credit limit",
            Self::Blocked => "the coordinator is blocked on its own question to the user",
            Self::TurnEnded => "the coordinator ended its turn without answering",
            Self::Working => return None,
        })
    }
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

/// What the sidebar shows of a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerSummary {
    pub(crate) worker_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) name: String,
    pub(crate) cwd: String,
    pub(crate) state: WorkerState,
    pub(crate) session_id: Option<String>,
    pub(crate) takeover: bool,
    /// Whether the sidebar lists it ([`Status::listed`]): while it runs,
    /// and after its end until its owner acknowledges that end.
    pub(crate) listed: bool,
}

/// How many TODO items of one repository have workers on them, for the
/// coordinator's Items button: items with a running worker, and items with
/// an ended run whose owner has not acknowledged its end (a result or a
/// failure to review).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ItemCounts {
    pub(crate) in_progress: u32,
    pub(crate) attention: u32,
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

/// What a takeover hands to the tab that resumes the worker's session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Takeover {
    pub(crate) worker_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) name: String,
    pub(crate) cwd: String,
    pub(crate) session_id: String,
    /// The claim's id, which the tab carries in [`TAKEOVER_ID_ENV`] and in
    /// its title.
    pub(crate) takeover_id: String,
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

/// The environment variable a takeover tab is created with: its claim's id.
pub(crate) const TAKEOVER_ID_ENV: &str = "HERDR_TAKEOVER_ID";

/// A takeover whose claim an earlier server recorded without its tab: what
/// recovery looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnfinishedTakeover {
    pub(crate) worker_id: String,
    /// None for a claim recorded before takeovers had ids: then only a
    /// process resuming the session can be found.
    pub(crate) takeover_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// A tab recovery may adopt: its id, its title and the pids of its panes'
/// shells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TakeoverTabCandidate {
    pub(crate) tab_id: String,
    pub(crate) title: String,
    pub(crate) shell_pids: Vec<u32>,
}

/// What recovery found of an unfinished takeover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TakeoverFound {
    /// The tab that carries its id, or whose pane runs a process resuming
    /// its session.
    Tab(String),
    /// A process resuming its session outside herdr's tabs.
    Process(u32),
    Nothing,
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

/// The server's supervisor, opened on first use ([`workers_dir`]).
pub(crate) fn supervisor() -> &'static WorkerSupervisor {
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

    /// Starts a worker's process: through a broker when this server has a
    /// launcher, so the worker outlives the server; else with pipes this
    /// server owns. `configure` sets its environment.
    fn spawn_worker(
        &self,
        worker_id: &str,
        args: &[String],
        cwd: &Path,
        configure: impl Fn(&mut Command),
    ) -> Result<Spawned, WorkerError> {
        let program = &self.shared.program;
        #[cfg(not(unix))]
        let _ = worker_id;
        #[cfg(unix)]
        if let Some(launcher) = &self.shared.broker {
            let socket = self.shared.dir.join(format!("{worker_id}.sock"));
            let log = self.shared.dir.join(format!("{worker_id}.broker.log"));
            let started = broker::start(launcher, &socket, &log, program, args, cwd, configure)
                .map_err(|error| {
                    WorkerError::Io(std::io::Error::new(
                        error.kind(),
                        format!(
                            "cannot start {} through its broker: {error}",
                            program.display()
                        ),
                    ))
                })?;
            let broker_pid = started.process.id();
            let pid = started.link.pid;
            let (input, messages) = match started.link.split() {
                Ok(split) => split,
                Err(error) => {
                    // Its guard ends the worker.
                    let mut process = started.process;
                    let _ = process.kill();
                    let _ = process.wait();
                    return Err(error.into());
                }
            };
            return Ok(Spawned {
                pid,
                input: Input::Broker(input),
                source: Source::Broker {
                    messages,
                    process: started.process,
                },
                broker: Some((broker_pid, socket)),
            });
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure(&mut command);
        crate::platform::configure_worker_process(&mut command);
        let mut child = command.spawn().map_err(|error| {
            WorkerError::Io(std::io::Error::new(
                error.kind(),
                format!("cannot start {}: {error}", program.display()),
            ))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkerError::Io(std::io::Error::other(
                "worker pipes missing",
            )));
        };
        Ok(Spawned {
            pid: child.id(),
            input: Input::Pipe(stdin),
            source: Source::Pipes {
                stdout,
                stderr,
                child,
            },
            broker: None,
        })
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
            status.listed(),
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
                listed: entry.status.listed(),
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
        pending
    }

    fn entry_number(registry: &Registry, worker_id: &str) -> Result<u64, WorkerError> {
        worker_number(worker_id)
            .filter(|number| registry.workers.contains_key(number))
            .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))
    }

    fn with_entry<T>(
        &self,
        worker_id: &str,
        read: impl FnOnce(&Entry) -> T,
    ) -> Result<T, WorkerError> {
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        Ok(read(&registry.workers[&number]))
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
    pub(crate) fn wait_attention(
        &self,
        worker_id: &str,
        after: Option<i64>,
        liveness_check: Duration,
        keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<Attention>, WorkerError> {
        self.wait_on(worker_id, liveness_check, keep_waiting, |entry| {
            let status = &entry.status;
            status.attention(after).map(|reason| Attention {
                reason,
                questions: status.questions.iter().map(Pending::shown).collect(),
                seq: status.last_seq,
                worker: status.info(&entry.journal_path),
            })
        })
    }

    /// Records that the worker's owner handled its events up to `seq`
    /// (an `acked` event). Idempotent and monotonic: an ack at or below the
    /// acknowledged `seq` records nothing. A `seq` past the worker's latest
    /// event is refused, since it would hide events that have not happened.
    pub(crate) fn ack(&self, worker_id: &str, seq: i64) -> Result<WorkerInfo, WorkerError> {
        let committed = {
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
            if seq <= status.acked_seq {
                None
            } else {
                Some(self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&json!({"type": "acked", "seq": seq})),
                ))
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
        let answered = self.command(
            params.command_id.as_deref(),
            "worker.answer",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.answer_once(params, receipt),
        )?;
        // A todo run that answers its worker's questions itself goes on
        // once the user answered one it escalated.
        self.question_settled(&params.worker_id);
        Ok(answered)
    }

    fn answer_once(
        &self,
        params: &WorkerAnswerParams,
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

/// The thread that owns one worker's stdout and process.
struct Reader {
    supervisor: WorkerSupervisor,
    number: u64,
    pid: u32,
    policy: policy::Policy,
    /// The pre-tool checks its `hook_callback` requests run.
    checks: pre_tool_checks::Runner,
    temp_dir: PathBuf,
    /// The broker's spool, removed once the exit is stored.
    spool: Option<PathBuf>,
    live: Arc<Live>,
    /// This server's lock on the journal, let go once the exit is stored.
    owner_lock: File,
}

impl Reader {
    /// Reads the stdout of a worker whose pipes this server owns, then
    /// reaps it.
    fn run(self, stdout: std::process::ChildStdout, mut child: Child) {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            self.stdout_line(&line, None);
        }
        // EOF: the CLI closed stdout, so it exited or is about to.
        self.live.close_input();
        let exited = match child.wait() {
            Ok(status) => json!({
                "type": "exited",
                "code": status.code(),
                "signal": crate::platform::exit_status_signal(&status),
            }),
            Err(error) => json!({"type": "exited", "code": null, "error": error.to_string()}),
        };
        self.finish(&exited, None);
    }

    /// Reads a worker's output from its broker until the worker's exit,
    /// acknowledging each line once it is stored (or was already).
    /// `broker` is the broker process when this server started it, reaped
    /// once it ends.
    #[cfg(unix)]
    fn run_broker(self, mut messages: broker::Messages, broker: Option<Child>) {
        let (exited, exit_seq) = loop {
            let Some((seq, message)) = messages.next() else {
                // Detached for the next server (a live handoff, or a test
                // playing a server that died): the journal goes to it, the
                // broker to the system; this process, while it runs, reaps
                // the broker it started once another server ended it.
                if self.live.detached.load(std::sync::atomic::Ordering::SeqCst) {
                    drop(self.owner_lock);
                    if let Some(mut broker) = broker {
                        let _ = broker.wait();
                    }
                    return;
                }
                // The broker never cuts a connection but for a newer
                // server, which takes the journal lock first, so it died;
                // its guard ends the worker. What it synced and did not send
                // is in its spool.
                if let Some((seq, exited)) = self
                    .spool
                    .as_deref()
                    .and_then(|spool| self.supervisor.replay_spool(self.number, spool))
                {
                    break (exited, Some(seq));
                }
                break (
                    json!({
                        "type": "exited",
                        "code": null,
                        "error": "the worker's broker ended before the worker's exit",
                    }),
                    None,
                );
            };
            let spooled = match message {
                broker::Message::Out(line) => self.stdout_line(&line, Some(seq)),
                broker::Message::Err(line) => self.supervisor.record_spooled(
                    self.number,
                    seq,
                    Direction::Err,
                    store::Recorded::Raw(&line),
                ),
                broker::Message::Lost(lines) => self.supervisor.record_spooled(
                    self.number,
                    seq,
                    Direction::Herdr,
                    store::Recorded::Event(&output_lost(lines)),
                ),
                broker::Message::Written { id, again } => self.supervisor.record_spooled(
                    self.number,
                    seq,
                    Direction::Herdr,
                    store::Recorded::Event(&input_written(&id, again)),
                ),
                broker::Message::Exit(exited) => break (exited, Some(seq)),
            };
            if spooled.is_stored() {
                messages.ack(seq);
            }
        };
        self.finish(&exited, exit_seq);
        if let Some(mut broker) = broker {
            let _ = broker.wait();
        }
    }

    /// Records one event or line in this worker's record: with its broker
    /// seq when it came from the broker ([`WorkerSupervisor::record_spooled`]).
    fn record_line(
        &self,
        broker_seq: Option<u64>,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> Spooled {
        match broker_seq {
            Some(seq) => self
                .supervisor
                .record_spooled(self.number, seq, direction, record),
            None => {
                self.supervisor
                    .record_any(self.number, direction, record, None);
                Spooled::Stored
            }
        }
    }

    /// Records one stdout line and acts on it. A line the store already
    /// holds (the broker sends it again after a re-attach) is not recorded
    /// again; a permission request among those is acted on only when the
    /// gone server stored it but neither asked nor settled it
    /// ([`Self::unhandled_request`]).
    fn stdout_line(&self, line: &str, broker_seq: Option<u64>) -> Spooled {
        if line.trim().is_empty() {
            return Spooled::Skipped;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return self.record_line(broker_seq, Direction::Out, store::Recorded::Raw(line));
        };
        let spooled = self.record_line(broker_seq, Direction::Out, store::Recorded::Event(&event));
        let request = event["type"].as_str() == Some("control_request")
            && matches!(
                event["request"]["subtype"].as_str(),
                Some("can_use_tool" | "hook_callback")
            );
        if event["type"].as_str() == Some("control_cancel_request") {
            if let Some(request_id) = event["request_id"].as_str() {
                self.checks.cancel(request_id);
            }
        }
        if spooled == Spooled::Duplicate {
            if request && self.unhandled_request(&event) {
                self.answer_request(&event);
            }
            return spooled;
        }
        self.supervisor.record_tool_sessions(self.number, self.pid);
        if request {
            self.answer_request(&event);
        }
        if event["type"].as_str() == Some("control_response")
            && event["response"]["request_id"].as_str()
                == Some(pre_tool_checks::INITIALIZE_REQUEST_ID)
            && event["response"]["subtype"].as_str() == Some("error")
        {
            let error = event["response"]["error"]
                .as_str()
                .unwrap_or("no reason given");
            self.supervisor.record(
                self.number,
                Direction::Herdr,
                &json!({
                    "type": "pre_tool_check_failed",
                    "error": format!(
                        "the CLI refused to register the pre-tool checks' hook, so none runs: {error}"
                    ),
                }),
            );
        }
        spooled
    }

    /// Answers a `can_use_tool` request through the policy, or a
    /// `hook_callback` request through the pre-tool checks.
    fn answer_request(&self, event: &Value) {
        if event["request"]["subtype"].as_str() == Some("hook_callback") {
            self.answer_hook(event);
        } else {
            self.answer_permission(event);
        }
    }

    /// Answers a `hook_callback` request: herdr's rule against background
    /// waits denies first, as a `permission` event; otherwise the pre-tool
    /// checks run for it on a thread of its own, so the worker's output is
    /// read on meanwhile (a cancel of this request among it). Each check that
    /// failed is a `pre_tool_check_failed` event, and the verdict a
    /// `pre_tool_check` event. A tool no check runs for gets no objection
    /// and no event.
    fn answer_hook(&self, event: &Value) {
        let request = &event["request"];
        let request_id = event["request_id"].as_str().unwrap_or("").to_owned();
        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
        let tool_name = input.get("tool_name").cloned().unwrap_or(Value::Null);
        let tool_use_id = request.get("tool_use_id").cloned().unwrap_or(Value::Null);
        let supervisor = self.supervisor.clone();
        let live = Arc::clone(&self.live);
        let number = self.number;
        let answer = move |supervisor: &WorkerSupervisor, response: Value| {
            let answer = control_response(&request_id, response);
            if let Err(error) = live.send(supervisor, &answer) {
                warn!(%error, "worker pre-tool check answer not delivered");
            }
        };
        if request["callback_id"].as_str() != Some(pre_tool_checks::CALLBACK_ID) {
            supervisor.record(
                number,
                Direction::Herdr,
                &json!({
                    "type": "pre_tool_check_failed",
                    "tool_name": tool_name,
                    "tool_use_id": tool_use_id,
                    "error": format!(
                        "a hook callback herdr did not register ({}); no objection",
                        request["callback_id"]
                    ),
                }),
            );
            answer(&supervisor, json!({}));
            return;
        }
        let tool_input = input
            .get("tool_input")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let tool = tool_name.as_str().unwrap_or("");
        if let Some(reason) = policy::background_wait_denial(tool, &tool_input) {
            supervisor.record(
                number,
                Direction::Herdr,
                &json!({
                    "type": "permission",
                    "tool_name": tool_name,
                    "tool_use_id": tool_use_id,
                    "decision": "deny",
                    "message": reason,
                }),
            );
            answer(&supervisor, pre_tool_checks::denial(reason));
            return;
        }
        if !self.checks.checks_tool(tool) {
            answer(&supervisor, json!({}));
            return;
        }
        let checks = self.checks.clone();
        let check_request_id = event["request_id"].as_str().unwrap_or("").to_owned();
        let spawned = crate::thread_spawn::spawn_named("herdr-worker-check", {
            let supervisor = supervisor.clone();
            move || {
                let outcome = checks.run(&check_request_id, &input);
                for (check, error) in &outcome.failures {
                    supervisor.record(
                        number,
                        Direction::Herdr,
                        &json!({
                            "type": "pre_tool_check_failed",
                            "check": check,
                            "tool_name": tool_name,
                            "tool_use_id": tool_use_id,
                            "error": error,
                        }),
                    );
                }
                let (decision, check, message) = match &outcome.verdict {
                    pre_tool_checks::Verdict::Allow => ("allow", Value::Null, Value::Null),
                    pre_tool_checks::Verdict::Deny(check, reason) => {
                        ("deny", json!(check), Value::String(reason.clone()))
                    }
                };
                supervisor.record(
                    number,
                    Direction::Herdr,
                    &json!({
                        "type": "pre_tool_check",
                        "tool_name": tool_name,
                        "tool_use_id": tool_use_id,
                        "decision": decision,
                        "check": check,
                        "message": message,
                    }),
                );
                answer(&supervisor, outcome.response());
            }
        });
        if let Err(error) = spawned {
            warn!(%error, "worker pre-tool check thread unavailable");
            supervisor.record(
                number,
                Direction::Herdr,
                &json!({
                    "type": "pre_tool_check_failed",
                    "error": format!("no thread to run the pre-tool checks: {error}; no objection"),
                }),
            );
        }
    }

    /// Whether a permission request the store holds still needs its
    /// answer or question: a server stored it and died before it recorded
    /// the question or settled the request, and the worker still waits.
    /// Not when it is asked (pending: the user answers it; answering: the
    /// re-attach sends that answer) or settled. The policy then decides it
    /// again; an answer the gone server did write goes by the request's id,
    /// which the broker writes at most once ([`input_id`]).
    fn unhandled_request(&self, event: &Value) -> bool {
        let Some(request_id) = event["request_id"].as_str() else {
            return false;
        };
        let registry = lock(&self.supervisor.shared.registry);
        let Some(entry) = registry.workers.get(&self.number) else {
            return false;
        };
        let status = &entry.status;
        !status.is_gone()
            && status.resolution(request_id).is_none()
            && !status
                .questions
                .iter()
                .any(|pending| pending.question.request_id == request_id)
    }

    /// After a re-attach, settles the permission requests of the current
    /// turn that a gone server stored and neither asked as a question nor
    /// saw cancelled: the worker still waits for each. One with a recorded
    /// answer (the policy's) goes again, by the request's id, so the broker
    /// writes it only if the gone server did not; one without is decided by
    /// the policy again, as if it just came. The broker does not send them
    /// again: it replays only what the store does not hold.
    #[cfg(unix)]
    fn settle_stored_requests(&self) {
        let (worker_id, since) = {
            let registry = lock(&self.supervisor.shared.registry);
            let Some(entry) = registry.workers.get(&self.number) else {
                return;
            };
            if entry.status.turn_ended() {
                return;
            }
            (
                entry.status.worker_id.clone(),
                entry.status.turn_seq.unwrap_or(0),
            )
        };
        let Ok(store) = &self.supervisor.shared.store else {
            return;
        };
        let requests = match store.unasked_requests(&worker_id, since) {
            Ok(requests) => requests,
            Err(error) => {
                warn!(worker_id, %error, "stored permission requests unreadable");
                return;
            }
        };
        for (request, answer) in requests {
            match answer {
                Some(answer) => {
                    if let Err(error) = self.live.resend(&answer, &input_id(&answer, None, 0)) {
                        warn!(worker_id, %error, "worker permission answer not delivered");
                    }
                }
                None => self.answer_request(&request),
            }
        }
    }

    /// Records the worker's exit, then removes the broker's spool, which
    /// nobody needs once the exit is stored.
    fn finish(self, exited: &Value, broker_seq: Option<u64>) {
        self.live.close_input();
        self.checks.cancel_all();
        // The temp dir goes first, so a worker shown exited has none; the
        // lock last, so a server waiting for it finds the exit stored.
        remove_temp_dir(&self.temp_dir);
        let spooled =
            self.record_line(broker_seq, Direction::Herdr, store::Recorded::Event(exited));
        if let (true, Some(spool)) = (spooled.is_stored(), &self.spool) {
            let _ = std::fs::remove_file(spool);
        }
        drop(self.owner_lock);
    }

    fn answer_permission(&self, event: &Value) {
        let request = &event["request"];
        let request_id = event["request_id"].as_str().unwrap_or("");
        let tool_name = request["tool_name"].as_str().unwrap_or("");
        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
        let decision = self.policy.decide(
            tool_name,
            &input,
            request["decision_reason_type"].as_str(),
            request["blocked_path"].as_str(),
        );
        let (behavior, message) = match &decision {
            policy::Decision::Allow => ("allow", Value::Null),
            policy::Decision::Deny(message) => ("deny", Value::String(message.clone())),
            policy::Decision::Ask(reason) => ("ask", Value::String(reason.clone())),
        };
        self.supervisor.record(
            self.number,
            Direction::Herdr,
            &json!({
                "type": "permission",
                "tool_name": tool_name,
                "tool_use_id": request.get("tool_use_id"),
                "decision": behavior,
                "message": message,
                "decision_reason_type": request.get("decision_reason_type"),
            }),
        );
        let response = match decision {
            policy::Decision::Allow => json!({"behavior": "allow", "updatedInput": input}),
            policy::Decision::Deny(message) => json!({"behavior": "deny", "message": message}),
            policy::Decision::Ask(reason) => {
                // No timer: the worker waits until the user answers.
                let question = question_from_request(request_id, request, &reason);
                let event = json!({"type": "question", "question": question, "input": input});
                self.supervisor
                    .record(self.number, Direction::Herdr, &event);
                return;
            }
        };
        let answer = control_response(request_id, response);
        if let Err(error) = self.live.send(&self.supervisor, &answer) {
            warn!(%error, "worker permission answer not delivered");
        }
    }
}
