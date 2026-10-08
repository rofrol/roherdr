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
//! is gone. Worker pipes are not handed over: a handoff is refused while a
//! worker's process is alive ([`prepare_for_handoff`]).
//!
//! Evidence for the message shapes and flags: `docs/headless-worker-trial-2026-10-07.md`.

mod log;
mod policy;
mod slot;
mod store;
#[cfg(test)]
mod tests;

pub(crate) use log::log_lines;

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tracing::warn;

use crate::api::schema::{
    WorkerAnswerParams, WorkerChoiceQuestion, WorkerDecision, WorkerInfo, WorkerKillReport,
    WorkerQuestion, WorkerQuestionKind, WorkerStartParams, WorkerState, WorkerTurnResult,
    WorkerWaitUntil,
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
    Unsupported(String),
    Io(std::io::Error),
}

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
            Self::Unsupported(_) => "worker_unsupported",
            Self::Io(_) => "worker_io_error",
        }
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
            | Self::Unsupported(message) => f.write_str(message),
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
    /// The takeover claim: set by `takeover`, cleared by `takeover_failed`.
    /// Kept in every state, an exited worker's too.
    takeover_ms: Option<u64>,
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
    /// The `seq` of the last event the store recorded for it.
    last_seq: i64,
    /// Why the worker's record is incomplete: a store or journal write
    /// failed. Not an event: it lives only in this server's memory.
    degraded: Option<String>,
}

/// How many settled questions a worker remembers for `worker_question_gone`.
const RESOLVED_QUESTIONS_KEPT: usize = 32;

/// A question with the tool input its answer is built from.
#[derive(Debug, Clone)]
struct Pending {
    question: WorkerQuestion,
    input: Value,
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
            takeover_ms: None,
            takeover_tab: None,
            takeover_error: None,
            takeover_unfinished: false,
            refusal: None,
            exited: false,
            lost: false,
            end_note: None,
            slot: None,
            last_seq: 0,
            degraded: None,
        }
    }

    /// Drops an answered or cancelled question, recording `how` it ended;
    /// the turn goes on once none is left.
    fn settle_question(&mut self, request_id: Option<&str>, how: &'static str) {
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

    /// Drops every pending question, recording `how` they ended.
    fn clear_questions(&mut self, how: &'static str) {
        for pending in std::mem::take(&mut self.questions) {
            self.remember_resolved(pending.question.request_id, how);
        }
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

    /// The ids of the pending questions, to tell which an event settled.
    fn pending_ids(&self) -> Vec<String> {
        self.questions
            .iter()
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

    /// Whether the worker's process is known to be gone: its exit is
    /// recorded, or it was lost and its process group no longer exists.
    fn process_gone(&self) -> bool {
        self.exited
            || (self.lost
                && self
                    .pid
                    .is_none_or(|pid| !crate::platform::process_group_alive(pid)))
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
                    self.takeover_tab = None;
                    self.takeover_error = None;
                    self.takeover_unfinished = false;
                    return;
                }
                "takeover_failed" => {
                    self.takeover_ms = None;
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
                self.pid = event
                    .get("pid")
                    .and_then(Value::as_u64)
                    .map(|pid| pid as u32);
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
                    self.questions.push(Pending {
                        question,
                        input: event.get("input").cloned().unwrap_or_else(|| json!({})),
                    });
                }
            }
            (Direction::Herdr, "answer") => {
                self.settle_question(event["request_id"].as_str(), "answered");
            }
            (Direction::Herdr, "signal") => {
                if event["signal"].as_str() == Some("SIGTERM") {
                    self.stop_requested_ms = event["at_ms"].as_u64();
                }
            }
            (Direction::Herdr, "exited") => {
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
            }
            (Direction::Herdr, "lost") => {
                // A worker between turns lost nothing but its process: it
                // keeps its last turn's state. A stop request stays, so a
                // later `exited` is judged as the stop's.
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
                self.settle_question(event["response"]["request_id"].as_str(), "answered");
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
            questions: self
                .questions
                .iter()
                .map(|pending| pending.question.clone())
                .collect(),
            stop_requested_ms: self.stop_requested_ms,
            takeover_ms: self.takeover_ms,
            takeover_tab_id: self.takeover_tab.clone(),
            takeover_error: self.takeover_error.clone(),
            takeover_unfinished: self.takeover_unfinished,
            end_note: self.end_note.clone(),
            degraded: self.degraded.clone(),
            journal_path: journal_path.display().to_string(),
        }
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
    let mut lines = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(record) = serde_json::from_str::<Value>(&String::from_utf8_lossy(line)) else {
            continue;
        };
        let Some(direction) = record["dir"].as_str().and_then(Direction::parse) else {
            continue;
        };
        let record_value = match (record.get("event"), record["raw"].as_str()) {
            (Some(event), _) => Ok(event.clone()),
            (None, Some(raw)) => Err(raw.to_owned()),
            (None, None) => continue,
        };
        lines.push(JournalLine {
            ts_ms: record["ts_ms"].as_u64().unwrap_or(0),
            direction,
            record: record_value,
        });
    }
    Ok(lines)
}

/// Folds a journal into a state in memory, without the store.
fn replay_journal(worker_id: &str, path: &Path) -> std::io::Result<Status> {
    let mut status = Status::new(worker_id.to_owned());
    for line in read_journal(path)? {
        if let Ok(event) = &line.record {
            status.apply(line.direction, event);
        }
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
                let before = status.pending_ids();
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
                tx.questions(seq, &before, &status)?;
            }
            status.last_seq = seq;
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

/// A running worker's input pipe; absent for one loaded from the store.
struct Live {
    number: u64,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
}

impl Live {
    /// Writes one line to the CLI and records it, in that order under one
    /// lock, so the recorded order of inputs is the order the CLI saw.
    fn send(&self, supervisor: &WorkerSupervisor, event: &Value) -> Result<(), WorkerError> {
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return Err(WorkerError::NotRunning(
                "the worker's input is closed".into(),
            ));
        };
        let mut line = event.to_string();
        line.push('\n');
        pipe.write_all(line.as_bytes())?;
        pipe.flush()?;
        supervisor.record(self.number, Direction::In, event);
        Ok(())
    }

    fn close_input(&self) {
        lock(&self.stdin).take();
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

#[derive(Default)]
struct Registry {
    next_number: u64,
    workers: BTreeMap<u64, Entry>,
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

/// A question some worker waits on, with the worker it belongs to.
#[derive(Debug, Clone)]
pub(crate) struct PendingWorkerQuestion {
    pub(crate) worker_id: String,
    pub(crate) cwd: String,
    pub(crate) question: WorkerQuestion,
}

/// Every pending question of the server's supervisor; empty when no
/// supervisor was opened (it is never opened just to answer this).
pub(crate) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    SUPERVISOR
        .get()
        .map(WorkerSupervisor::pending_questions)
        .unwrap_or_default()
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
}

/// The server's supervisor, opened on first use. Its journals live in
/// herdr's state directory, apart per named session.
pub(crate) fn supervisor() -> &'static WorkerSupervisor {
    SUPERVISOR.get_or_init(|| {
        let state_dir = crate::config::state_dir();
        let dir = match crate::session::active_name() {
            Some(name) => state_dir.join("sessions").join(name).join("workers"),
            None => state_dir.join("workers"),
        };
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
    pub(crate) fn open(dir: PathBuf, program: PathBuf) -> Self {
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
            }),
        };
        for (number, _held) in unowned {
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
            registry.workers[&number].status.process_gone()
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
                    Ok(Some(status)) => Ok(status),
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
    /// their pipes. Without `force` it refuses while any worker's process is
    /// alive, in a turn or idle between turns, naming each and how to end
    /// it. With `force` it sends each SIGTERM and goes on.
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

    pub(crate) fn start(&self, params: &WorkerStartParams) -> Result<WorkerInfo, WorkerError> {
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
        if !crate::platform::WORKER_SANDBOX_SUPPORTED {
            return Err(WorkerError::Unsupported(
                "headless workers run Bash in Claude Code's sandbox, which this platform does not \
                 have; refusing to start a worker without it"
                    .into(),
            ));
        }
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
        let settings = worker_settings(&cwd_real, &temp_dir, &slot_caches).to_string();
        let contract = worker_contract(&temp_dir, slot.as_ref());

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

        let mut command = Command::new(&self.shared.program);
        command
            .args(&args)
            .current_dir(&cwd_real)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("HERDR_") {
                command.env_remove(&key);
            } else if is_credential_env(&name, bedrock, vertex) {
                removed_env.push(name.into_owned());
                command.env_remove(&key);
            }
        }
        removed_env.sort();
        // The slot's caches, inside the slot, so a sandboxed build writes
        // only there instead of the user's `~/.cache/zig` or another target.
        if let Some(slot) = &slot {
            let zig_cache = slot.zig_cache_dir();
            command
                .env("CARGO_TARGET_DIR", slot.target_dir())
                .env("ZIG_GLOBAL_CACHE_DIR", &zig_cache)
                .env("ZIG_LOCAL_CACHE_DIR", &zig_cache);
        }
        crate::platform::configure_worker_process(&mut command);
        let mut child = command.spawn().map_err(|error| {
            remove_temp_dir(&temp_dir);
            WorkerError::Io(std::io::Error::new(
                error.kind(),
                format!("cannot start {}: {error}", self.shared.program.display()),
            ))
        })?;
        let pid = child.id();
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            remove_temp_dir(&temp_dir);
            return Err(WorkerError::Io(std::io::Error::other(
                "worker pipes missing",
            )));
        };

        let policy = policy::Policy::new(&cwd_path, &cwd_real, &temp_dir);
        let name = params
            .name
            .as_deref()
            .map(|name| one_line(name, 80))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| task_name(prompt));
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
            "program": self.shared.program.display().to_string(),
            "args": args,
            "folder_slot": slot.as_ref().map(|slot| json!({
                "name": params.folder_slot,
                "branch": slot.branch,
                "base": slot.base,
                "created": slot.created,
                "fresh_build": params.fresh_build,
                "killed_leftover_pids": slot_killed,
            })),
        });
        let live = Arc::new(Live {
            number,
            stdin: Arc::new(Mutex::new(Some(stdin))),
        });
        {
            let mut registry = lock(&self.shared.registry);
            let mut entry = Entry::new(Status::new(worker_id.clone()), journal_path);
            entry.live = Some(Arc::clone(&live));
            registry.workers.insert(number, entry);
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&started),
            );
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(
                    &json!({"type": "policy", "file_tool_roots": policy.roots()}),
                ),
            );
        }
        self.shared.changed.notify_all();
        notify_clients();

        let stderr_supervisor = self.clone();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-worker-err", move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                stderr_supervisor.record_raw(number, Direction::Err, &line);
            }
        }) {
            warn!(%error, "worker stderr reader unavailable");
        }
        let reader = Reader {
            supervisor: self.clone(),
            number,
            pid,
            policy,
            temp_dir: temp_dir.clone(),
            live: Arc::clone(&live),
            owner_lock,
        };
        if let Err(error) =
            crate::thread_spawn::spawn_named("herdr-worker", move || reader.run(stdout, child))
        {
            // Without a reader nobody would reap or journal the process.
            let _ = crate::platform::signal_process_group(pid, Signal::Kill);
            remove_temp_dir(&temp_dir);
            self.record(
                number,
                Direction::Herdr,
                &json!({"type": "exited", "code": null}),
            );
            return Err(WorkerError::Io(error));
        }

        // The CLI buffers input written before `system/init` (trial 1).
        live.send(self, &user_message(prompt))?;
        self.status(&worker_id)
    }

    /// Records one event of a worker and wakes those waiting on it.
    fn record(&self, number: u64, direction: Direction, event: &Value) {
        self.record_any(number, direction, store::Recorded::Event(event));
    }

    /// Records a line the CLI wrote that is not a JSON event.
    fn record_raw(&self, number: u64, direction: Direction, line: &str) {
        self.record_any(number, direction, store::Recorded::Raw(line));
    }

    fn record_any(&self, number: u64, direction: Direction, record: store::Recorded<'_>) {
        let shown_changed = {
            let mut registry = lock(&self.shared.registry);
            self.commit_locked(&mut registry, number, direction, record)
        };
        self.shared.changed.notify_all();
        if shown_changed {
            notify_clients();
        }
    }

    /// The one way an event enters a worker's record, under the registry
    /// lock, so the recorded order is the order it is folded in: the event
    /// is folded into the worker's status, then appended to the store with
    /// the projections it changes in one transaction (which gives it its
    /// `seq`), then exported to the JSONL journal. A failed write is not
    /// swallowed: the status keeps the event (it happened) and is marked
    /// degraded with the error. The caller wakes the waiters; returns
    /// whether what the clients show changed.
    fn commit_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> bool {
        let Some(entry) = registry.workers.get_mut(&number) else {
            return false;
        };
        let shown_before = Self::shown(&entry.status);
        let pending_before = entry.status.pending_ids();
        if let store::Recorded::Event(event) = record {
            entry.status.apply(direction, event);
        }
        let ts_ms = now_ms();
        let written = match &self.shared.store {
            Ok(store) => store
                .transaction(|tx| {
                    let seq = tx.event(&store::EventRow {
                        worker_id: &entry.status.worker_id,
                        direction,
                        record: &record,
                        ts_ms,
                    })?;
                    if !entry.foreign {
                        tx.questions(seq, &pending_before, &entry.status)?;
                        tx.worker(&entry.status, seq)?;
                    }
                    Ok(seq)
                })
                .map_err(|error| format!("a worker store write failed: {error}")),
            Err(error) => Err(error.clone()),
        };
        let seq = match written {
            Ok(seq) => {
                entry.status.last_seq = seq;
                Some(seq)
            }
            Err(error) => {
                warn!(%error, worker_id = entry.status.worker_id, "worker event not stored");
                entry.status.degraded = Some(error);
                None
            }
        };
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
        Self::shown(&entry.status) != shown_before
    }

    /// The parts of a status the clients show. Questions are only added or
    /// removed, never replaced in place, so their count tells a change.
    fn shown(status: &Status) -> (WorkerState, usize, bool, bool, bool) {
        (
            status.state,
            status.questions.len(),
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
            })
            .collect()
    }

    fn pending_questions(&self) -> Vec<PendingWorkerQuestion> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .flat_map(|entry| {
                entry
                    .status
                    .questions
                    .iter()
                    .map(|pending| PendingWorkerQuestion {
                        worker_id: entry.status.worker_id.clone(),
                        cwd: entry.status.cwd.clone(),
                        question: pending.question.clone(),
                    })
            })
            .collect()
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

    pub(crate) fn list(&self) -> Vec<WorkerInfo> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect()
    }

    /// Blocks until the worker reaches `until`, woken by its state changes.
    /// `keep_waiting` runs at least every `liveness_check` so a caller whose
    /// client went away or whose server stops can give up; it never decides
    /// the outcome. Returns `None` when the caller gave up.
    pub(crate) fn wait(
        &self,
        worker_id: &str,
        until: WorkerWaitUntil,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<WorkerInfo>, WorkerError> {
        let mut registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        loop {
            let entry = &registry.workers[&number];
            let reached = match until {
                WorkerWaitUntil::TurnEnd => entry.status.turn_ended(),
                WorkerWaitUntil::Exit => entry.status.is_gone(),
            };
            if reached {
                return Ok(Some(entry.status.info(&entry.journal_path)));
            }
            if !keep_waiting() {
                return Ok(None);
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

    /// Sends the next user message. Accepted only between turns, so a
    /// `worker.wait` after it waits for this turn's end.
    pub(crate) fn prompt(&self, worker_id: &str, text: &str) -> Result<WorkerInfo, WorkerError> {
        if text.trim().is_empty() {
            return Err(WorkerError::Invalid("text must not be empty".into()));
        }
        let (_, live, status) = self.live(worker_id)?;
        if status.takeover_ms.is_some() {
            return Err(Self::taken_over(worker_id));
        }
        if !status.turn_ended() {
            return Err(WorkerError::Busy(format!(
                "worker {worker_id} is in a turn; wait for it or interrupt it first"
            )));
        }
        live.send(self, &user_message(text))?;
        self.status(worker_id)
    }

    /// Asks the CLI to interrupt its turn (a control request). The turn
    /// then ends with a `result` whose `terminal_reason` is `aborted_*`.
    pub(crate) fn interrupt(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        let (number, live, _) = self.live(worker_id)?;
        live.send(
            self,
            &json!({
            "type": "control_request",
            "request_id": format!("herdr-interrupt-{number}-{}", now_ms()),
            "request": {"subtype": "interrupt"},
            }),
        )?;
        self.status(worker_id)
    }

    /// Closes the worker's input, sends SIGTERM to its process group and
    /// returns. The outcome comes from the process's exit event, which turns
    /// the state to `exited`; until then the status carries
    /// `stop_requested_ms`, so asking again shows a worker that is still
    /// alive. A repeated stop sends nothing. It never escalates; `worker.kill`
    /// does.
    pub(crate) fn stop(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        let (number, live, status) = self.live(worker_id)?;
        if status.stop_requested_ms.is_some() {
            return self.status(worker_id);
        }
        let pid = status
            .pid
            .ok_or_else(|| WorkerError::NotRunning(format!("worker {worker_id} has no process")))?;
        self.record_tool_sessions(number, pid);
        let signal = json!({"type": "signal", "signal": "SIGTERM", "at_ms": now_ms()});
        self.record(number, Direction::Herdr, &signal);
        live.close_input();
        crate::platform::signal_process_group(pid, Signal::Terminate)?;
        self.status(worker_id)
    }

    /// Answers the worker's pending question that `request_id` names, with
    /// the user's decision. Without `request_id` it answers the only pending
    /// question and refuses when several are pending.
    pub(crate) fn answer(&self, params: &WorkerAnswerParams) -> Result<WorkerInfo, WorkerError> {
        let worker_id = params.worker_id.as_str();
        let (live, request_id, response) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if let Some(request_id) = params.request_id.as_deref() {
                let is_pending = entry
                    .status
                    .questions
                    .iter()
                    .any(|pending| pending.question.request_id == request_id);
                if let (false, Some(how)) = (is_pending, entry.status.resolution(request_id)) {
                    return Err(WorkerError::QuestionGone(format!(
                        "question {request_id} of worker {worker_id} is no longer pending: {how}"
                    )));
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
            let questions = &entry.status.questions;
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
            let answer = json!({
                "type": "answer",
                "request_id": request_id,
                "tool_name": pending.question.tool_name,
                "decision": response["behavior"],
                "answers": answers,
                "by": "user",
            });
            // Settled under the lock, so a second answer finds no question.
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&answer),
            );
            (live, request_id, response)
        };
        self.shared.changed.notify_all();
        notify_clients();
        live.send(self, &control_response(&request_id, response))?;
        self.status(worker_id)
    }

    /// Force-stops a worker: SIGKILL to its process group, then to the
    /// processes of its recorded tool sessions that are still the ones
    /// recorded ([`plan_session_kill`]). A worker that exited or was lost is
    /// signalled only with `force`; without it the kill is refused with what
    /// it would signal, since its sessions may have ended long ago.
    pub(crate) fn kill(
        &self,
        worker_id: &str,
        force: bool,
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
            self.record(
                number,
                Direction::Herdr,
                &json!({"type": "signal", "signal": "SIGKILL"}),
            );
            crate::platform::signal_process_group(pid, Signal::Kill)?;
        }
        let sessions = self.with_entry(worker_id, |entry| entry.status.tool_sessions.clone())?;
        let plan = plan_session_kill(
            &sessions,
            crate::platform::process_start_token,
            crate::platform::session_members,
        );
        crate::platform::signal_processes(&plan.pids, Signal::Kill);
        self.record(
            number,
            Direction::Herdr,
            &json!({
                "type": "killed_tool_processes",
                "pids": plan.pids,
                "unverified_sessions": plan.unverified_sessions,
                "stale_sessions": plan.stale_sessions,
            }),
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
    /// takeover releases it ([`Self::fail_takeover`]), and one an earlier
    /// server left unfinished can be tried again.
    pub(crate) fn begin_takeover(&self, worker_id: &str) -> Result<Takeover, WorkerError> {
        let takeover = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let status = &entry.status;
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
            let session_id = status.session_id.clone().ok_or_else(|| {
                WorkerError::NotRunning(format!("worker {worker_id} has no session yet"))
            })?;
            let takeover = Takeover {
                worker_id: worker_id.to_owned(),
                workspace_id: status.workspace_id.clone(),
                name: status.name.clone(),
                cwd: status.cwd.clone(),
                session_id,
            };
            let event = json!({"type": "takeover", "at_ms": now_ms()});
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
            self.interrupt(worker_id)?;
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
    }
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
    temp_dir: PathBuf,
    live: Arc<Live>,
    /// This server's lock on the journal, let go once the exit is stored.
    owner_lock: File,
}

impl Reader {
    fn run(self, stdout: std::process::ChildStdout, mut child: Child) {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                self.supervisor
                    .record_raw(self.number, Direction::Out, &line);
                continue;
            };
            self.supervisor.record(self.number, Direction::Out, &event);
            self.supervisor.record_tool_sessions(self.number, self.pid);
            if event["type"].as_str() == Some("control_request")
                && event["request"]["subtype"].as_str() == Some("can_use_tool")
            {
                self.answer_permission(&event);
            }
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
        // The temp dir goes first, so a worker shown exited has none; the
        // lock last, so a server waiting for it finds the exit stored.
        remove_temp_dir(&self.temp_dir);
        self.supervisor
            .record(self.number, Direction::Herdr, &exited);
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
