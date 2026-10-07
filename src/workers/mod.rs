//! Headless Claude workers owned by the server.
//!
//! A worker is `claude -p` over stream-json pipes, without a terminal, in its
//! own process group. One reader thread per worker journals every line in and
//! out (`<state dir>/workers/<id>.jsonl`), answers `can_use_tool` requests
//! through [`policy`], and folds the events into the worker's state. A request
//! the policy leaves to the user becomes a pending question, shown in the
//! client's `?` list and answered with `worker.answer`; the worker waits for
//! it without a time limit. The same fold replays a journal after a server
//! restart, where a worker that had not exited is recorded as `lost`.
//!
//! Evidence for the message shapes and flags: `docs/headless-worker-trial-2026-10-07.md`.

mod log;
mod policy;
#[cfg(test)]
mod tests;

pub(crate) use log::log_lines;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tracing::warn;

use crate::api::schema::{
    WorkerAnswerParams, WorkerChoiceQuestion, WorkerDecision, WorkerInfo, WorkerQuestion,
    WorkerQuestionKind, WorkerStartParams, WorkerState, WorkerTurnResult, WorkerWaitUntil,
};
use crate::platform::Signal;

/// Appended to the worker's system prompt.
const WORKER_CONTRACT: &str = "You are a headless worker started by herdr. Nobody watches \
your output live. Work only inside your working directory. Commands and tools outside \
herdr's short allow list, and your questions, wait until the user answers, which can take \
long: ask only when you cannot go on without it, otherwise finish the task or stop and say \
what blocks you.";

/// The settings that keep the user's CLAUDE.md and login but turn the
/// global hooks off (trial 2, T2-1).
const WORKER_SETTINGS: &str = r#"{"disableAllHooks":true}"#;

#[derive(Debug)]
pub(crate) enum WorkerError {
    NotFound(String),
    Invalid(String),
    NotRunning(String),
    Busy(String),
    NoQuestion(String),
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
            | Self::NoQuestion(message) => f.write_str(message),
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
    tool_sessions: BTreeSet<u32>,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    /// Requests left to the user, oldest first.
    questions: Vec<Pending>,
    stop_requested_ms: Option<u64>,
    takeover_ms: Option<u64>,
}

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
            tool_sessions: BTreeSet::new(),
            exit_code: None,
            exit_signal: None,
            questions: Vec::new(),
            stop_requested_ms: None,
            takeover_ms: None,
        }
    }

    /// Drops an answered or cancelled question; the turn goes on once none
    /// is left.
    fn settle_question(&mut self, request_id: Option<&str>) {
        if let Some(request_id) = request_id {
            self.questions
                .retain(|pending| pending.question.request_id != request_id);
        }
        if self.questions.is_empty() && self.state == WorkerState::WaitingApproval {
            self.state = WorkerState::Working;
        }
    }

    fn is_gone(&self) -> bool {
        matches!(self.state, WorkerState::Exited | WorkerState::Lost)
    }

    fn turn_ended(&self) -> bool {
        self.is_gone()
            || matches!(
                self.state,
                WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
            )
    }

    fn apply(&mut self, direction: Direction, event: &Value) {
        if self.is_gone() {
            return;
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match (direction, kind) {
            (Direction::Herdr, "started") => {
                self.cwd = string_field(event, "cwd").unwrap_or_default();
                self.name = string_field(event, "name").unwrap_or_default();
                self.workspace_id = string_field(event, "workspace_id");
                self.model = string_field(event, "model");
                self.pid = event
                    .get("pid")
                    .and_then(Value::as_u64)
                    .map(|pid| pid as u32);
            }
            (Direction::Herdr, "tool_sessions") => {
                self.tool_sessions.extend(
                    event["sessions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_u64)
                        .map(|session| session as u32),
                );
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
            (Direction::Herdr, "takeover") => {
                self.takeover_ms = event["at_ms"].as_u64();
            }
            (Direction::Herdr, "answer") => {
                self.settle_question(event["request_id"].as_str());
            }
            (Direction::Herdr, "signal") => {
                if event["signal"].as_str() == Some("SIGTERM") {
                    self.stop_requested_ms = event["at_ms"].as_u64();
                }
            }
            (Direction::Herdr, "exited") => {
                self.questions.clear();
                self.stop_requested_ms = None;
                self.state = WorkerState::Exited;
                self.exit_code = event.get("code").and_then(Value::as_i64).map(|v| v as i32);
                self.exit_signal = event
                    .get("signal")
                    .and_then(Value::as_i64)
                    .map(|v| v as i32);
            }
            (Direction::Herdr, "lost") => {
                self.questions.clear();
                self.stop_requested_ms = None;
                self.state = WorkerState::Lost;
            }
            (Direction::Out, "system") => {
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
                self.settle_question(event["response"]["request_id"].as_str());
            }
            (Direction::Out, "control_cancel_request") => {
                self.settle_question(event["request_id"].as_str());
            }
            (Direction::In, "user") => {
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
                };
                self.state = turn_end_state(&result);
                self.questions.clear();
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
            tool_sessions: self.tool_sessions.iter().copied().collect(),
            exit_code: self.exit_code,
            exit_signal: self.exit_signal,
            questions: self
                .questions
                .iter()
                .map(|pending| pending.question.clone())
                .collect(),
            stop_requested_ms: self.stop_requested_ms,
            takeover_ms: self.takeover_ms,
            journal_path: journal_path.display().to_string(),
        }
    }
}

/// Judges a turn by its `result`, never by the exit code (1 after an
/// interrupted last turn, 143 after SIGTERM).
fn turn_end_state(result: &WorkerTurnResult) -> WorkerState {
    if result
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

/// One worker's append-only JSONL journal.
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

    fn record(&self, direction: Direction, event: &Value) {
        self.write(json!({"ts_ms": now_ms(), "dir": direction.as_str(), "event": event}));
    }

    fn record_raw(&self, direction: Direction, line: &str) {
        self.write(json!({"ts_ms": now_ms(), "dir": direction.as_str(), "raw": line}));
    }

    fn write(&self, record: Value) {
        let mut line = record.to_string();
        line.push('\n');
        let mut file = lock(&self.file);
        if let Err(error) = file.write_all(line.as_bytes()) {
            warn!(%error, "worker journal write failed");
        }
    }
}

/// Reads a journal back into a state. Unknown or broken lines are skipped.
fn replay_journal(worker_id: &str, path: &Path) -> std::io::Result<Status> {
    let mut status = Status::new(worker_id.to_owned());
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(direction) = record["dir"].as_str().and_then(Direction::parse) else {
            continue;
        };
        if let Some(event) = record.get("event") {
            status.apply(direction, event);
        }
    }
    Ok(status)
}

/// A running worker's pipes and journal; absent for one replayed from disk.
struct Live {
    journal: Arc<Journal>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
}

impl Live {
    /// Writes one line to the CLI and journals it, in that order under one
    /// lock, so the journal's order of inputs is the order the CLI saw.
    fn send(&self, event: &Value) -> Result<(), WorkerError> {
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
        self.journal.record(Direction::In, event);
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
}

#[derive(Default)]
struct Registry {
    next_number: u64,
    workers: BTreeMap<u64, Entry>,
}

struct Shared {
    dir: PathBuf,
    program: PathBuf,
    registry: Mutex<Registry>,
    changed: Condvar,
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
    /// Opens the journal directory and replays it. A worker whose journal
    /// does not end in an exit was left by a previous server: it is marked
    /// `lost` there and here.
    pub(crate) fn open(dir: PathBuf, program: PathBuf) -> Self {
        let mut registry = Registry {
            next_number: 1,
            ..Registry::default()
        };
        if let Err(error) = std::fs::create_dir_all(&dir) {
            warn!(%error, dir = %dir.display(), "worker journal directory unavailable");
        }
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(worker_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some(number) = worker_number(worker_id) else {
                continue;
            };
            let mut status = match replay_journal(worker_id, &path) {
                Ok(status) => status,
                Err(error) => {
                    warn!(%error, path = %path.display(), "worker journal unreadable");
                    continue;
                }
            };
            if !status.is_gone() {
                let lost = json!({"type": "lost", "reason": "server restarted"});
                match Journal::open(&path) {
                    Ok(journal) => journal.record(Direction::Herdr, &lost),
                    Err(error) => warn!(%error, "cannot mark worker lost in its journal"),
                }
                status.apply(Direction::Herdr, &lost);
            }
            registry.next_number = registry.next_number.max(number + 1);
            registry.workers.insert(
                number,
                Entry {
                    status,
                    journal_path: path,
                    live: None,
                },
            );
        }
        Self {
            shared: Arc::new(Shared {
                dir,
                program,
                registry: Mutex::new(registry),
                changed: Condvar::new(),
            }),
        }
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

        let number = {
            let mut registry = lock(&self.shared.registry);
            let number = registry.next_number;
            registry.next_number += 1;
            number
        };
        let worker_id = format!("w{number}");
        std::fs::create_dir_all(&self.shared.dir)?;
        let journal_path = self.shared.dir.join(format!("{worker_id}.jsonl"));
        let journal = Arc::new(Journal::open(&journal_path)?);

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
            WORKER_SETTINGS,
            "--append-system-prompt",
            WORKER_CONTRACT,
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
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR_") {
                command.env_remove(key);
            }
        }
        crate::platform::configure_worker_process(&mut command);
        let mut child = command.spawn().map_err(|error| {
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
            return Err(WorkerError::Io(std::io::Error::other(
                "worker pipes missing",
            )));
        };

        let (policy, policy_warnings) = policy::Policy::load(&cwd_path, &cwd_real);
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
            "pid": pid,
            "program": self.shared.program.display().to_string(),
            "args": args,
        });
        journal.record(Direction::Herdr, &started);
        journal.record(
            Direction::Herdr,
            &json!({
                "type": "policy",
                "bash_rules": policy.rules(),
                "warnings": policy_warnings,
            }),
        );
        let mut status = Status::new(worker_id.clone());
        status.apply(Direction::Herdr, &started);

        let live = Arc::new(Live {
            journal: Arc::clone(&journal),
            stdin: Arc::new(Mutex::new(Some(stdin))),
        });
        {
            let mut registry = lock(&self.shared.registry);
            registry.workers.insert(
                number,
                Entry {
                    status,
                    journal_path: journal_path.clone(),
                    live: Some(Arc::clone(&live)),
                },
            );
        }

        let stderr_journal = Arc::clone(&journal);
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-worker-err", move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                stderr_journal.record_raw(Direction::Err, &line);
            }
        }) {
            warn!(%error, "worker stderr reader unavailable");
        }
        let reader = Reader {
            supervisor: self.clone(),
            number,
            pid,
            policy,
            live: Arc::clone(&live),
        };
        if let Err(error) =
            crate::thread_spawn::spawn_named("herdr-worker", move || reader.run(stdout, child))
        {
            // Without a reader nobody would reap or journal the process.
            let _ = crate::platform::signal_process_group(pid, Signal::Kill);
            self.update(
                number,
                Direction::Herdr,
                &json!({"type": "exited", "code": null}),
            );
            return Err(WorkerError::Io(error));
        }

        // The CLI buffers input written before `system/init` (trial 1).
        live.send(&user_message(prompt))?;
        self.status(&worker_id)
    }

    fn update(&self, number: u64, direction: Direction, event: &Value) {
        let mut registry = lock(&self.shared.registry);
        let mut shown_changed = false;
        if let Some(entry) = registry.workers.get_mut(&number) {
            let before = Self::shown(&entry.status);
            entry.status.apply(direction, event);
            shown_changed = Self::shown(&entry.status) != before;
        }
        drop(registry);
        self.shared.changed.notify_all();
        if shown_changed {
            notify_clients();
        }
    }

    /// The parts of a status the clients show. Questions are only added or
    /// removed, never replaced in place, so their count tells a change.
    fn shown(status: &Status) -> (WorkerState, usize, bool, bool) {
        (
            status.state,
            status.questions.len(),
            status.session_id.is_some(),
            status.takeover_ms.is_some(),
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
                takeover: entry.status.takeover_ms.is_some(),
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
        let (number, live, status) = self.live(worker_id)?;
        if !status.turn_ended() {
            return Err(WorkerError::Busy(format!(
                "worker {worker_id} is in a turn; wait for it or interrupt it first"
            )));
        }
        let message = user_message(text);
        live.send(&message)?;
        self.update(number, Direction::In, &message);
        self.status(worker_id)
    }

    /// Asks the CLI to interrupt its turn (a control request). The turn
    /// then ends with a `result` whose `terminal_reason` is `aborted_*`.
    pub(crate) fn interrupt(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        let (number, live, _) = self.live(worker_id)?;
        live.send(&json!({
            "type": "control_request",
            "request_id": format!("herdr-interrupt-{number}-{}", now_ms()),
            "request": {"subtype": "interrupt"},
        }))?;
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
        self.record_tool_sessions(number, pid, &live.journal);
        let signal = json!({"type": "signal", "signal": "SIGTERM", "at_ms": now_ms()});
        live.journal.record(Direction::Herdr, &signal);
        self.update(number, Direction::Herdr, &signal);
        live.close_input();
        crate::platform::signal_process_group(pid, Signal::Terminate)?;
        self.status(worker_id)
    }

    /// Answers the worker's oldest pending question, or the one
    /// `request_id` names, with the user's decision.
    pub(crate) fn answer(&self, params: &WorkerAnswerParams) -> Result<WorkerInfo, WorkerError> {
        let worker_id = params.worker_id.as_str();
        let (number, live, request_id, response, answer) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let live = match (&entry.live, entry.status.is_gone()) {
                (Some(live), false) => Arc::clone(live),
                _ => {
                    return Err(WorkerError::NotRunning(format!(
                        "worker {worker_id} is not running"
                    )))
                }
            };
            let pending = match params.request_id.as_deref() {
                Some(request_id) => entry
                    .status
                    .questions
                    .iter()
                    .find(|pending| pending.question.request_id == request_id),
                None => entry.status.questions.first(),
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
            entry.status.apply(Direction::Herdr, &answer);
            (number, live, request_id, response, answer)
        };
        self.shared.changed.notify_all();
        notify_clients();
        live.journal.record(Direction::Herdr, &answer);
        let message = control_response(&request_id, response);
        live.send(&message)?;
        self.update(number, Direction::In, &message);
        self.status(worker_id)
    }

    /// Force-stops a worker: SIGKILL to its process group, then to every
    /// process in the tool sessions recorded for it. Also works on a worker
    /// that already exited or was lost, to end its orphaned tools.
    pub(crate) fn kill(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        let (number, live, status) = {
            let registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = &registry.workers[&number];
            (number, entry.live.clone(), entry.status.clone())
        };
        let journal = match &live {
            Some(live) => Arc::clone(&live.journal),
            None => Arc::new(Journal::open(&self.journal_path(worker_id))?),
        };
        if let (Some(pid), false) = (status.pid, status.is_gone()) {
            self.record_tool_sessions(number, pid, &journal);
            journal.record(
                Direction::Herdr,
                &json!({"type": "signal", "signal": "SIGKILL"}),
            );
            crate::platform::signal_process_group(pid, Signal::Kill)?;
        }
        let sessions = self.with_entry(worker_id, |entry| entry.status.tool_sessions.clone())?;
        let mut killed = Vec::new();
        for session in sessions {
            let members = crate::platform::session_members(session);
            if !members.is_empty() {
                crate::platform::signal_processes(&members, Signal::Kill);
                killed.extend(members);
            }
        }
        journal.record(
            Direction::Herdr,
            &json!({"type": "killed_tool_processes", "pids": killed}),
        );
        self.status(worker_id)
    }

    /// Claims a worker for a takeover and journals it. Refused while a
    /// question waits on the user (answer it first, or the interrupt would
    /// throw the answer away), for a worker without a session yet, for one
    /// that was lost (its process is not ours to end) and a second time.
    pub(crate) fn begin_takeover(&self, worker_id: &str) -> Result<Takeover, WorkerError> {
        let (takeover, journal, event) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let status = &entry.status;
            if status.state == WorkerState::Lost {
                return Err(WorkerError::NotRunning(format!(
                    "worker {worker_id} was lost: its process is not this server's to end"
                )));
            }
            if !status.questions.is_empty() {
                return Err(WorkerError::Busy(format!(
                    "worker {worker_id} waits on your answer; answer it before taking over"
                )));
            }
            if status.takeover_ms.is_some() {
                return Err(WorkerError::Busy(format!(
                    "worker {worker_id} is already being taken over"
                )));
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
            let journal = match &entry.live {
                Some(live) => Arc::clone(&live.journal),
                None => Arc::new(Journal::open(&entry.journal_path)?),
            };
            let event = json!({"type": "takeover", "at_ms": now_ms()});
            // Claimed under the lock, so a second takeover is refused.
            entry.status.apply(Direction::Herdr, &event);
            (takeover, journal, event)
        };
        journal.record(Direction::Herdr, &event);
        self.shared.changed.notify_all();
        notify_clients();
        Ok(takeover)
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
        let status = self.status(worker_id)?;
        if matches!(status.state, WorkerState::Exited | WorkerState::Lost) {
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

    fn journal_path(&self, worker_id: &str) -> PathBuf {
        self.shared.dir.join(format!("{worker_id}.jsonl"))
    }

    /// Records the sessions of the CLI's descendants that are new. Claude
    /// Code runs each Bash tool with `setsid`, so after a crash only these
    /// recorded sessions find its tools (trial 2, T2-3).
    fn record_tool_sessions(&self, number: u64, pid: u32, journal: &Journal) {
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
                .filter(|session| !entry.status.tool_sessions.contains(session))
                .collect()
        };
        if new.is_empty() {
            return;
        }
        let event = json!({"type": "tool_sessions", "sessions": new});
        journal.record(Direction::Herdr, &event);
        self.update(number, Direction::Herdr, &event);
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
    live: Arc<Live>,
}

impl Reader {
    fn run(self, stdout: std::process::ChildStdout, mut child: Child) {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                self.live.journal.record_raw(Direction::Out, &line);
                continue;
            };
            self.live.journal.record(Direction::Out, &event);
            self.supervisor.update(self.number, Direction::Out, &event);
            self.supervisor
                .record_tool_sessions(self.number, self.pid, &self.live.journal);
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
        self.live.journal.record(Direction::Herdr, &exited);
        self.supervisor
            .update(self.number, Direction::Herdr, &exited);
    }

    fn answer_permission(&self, event: &Value) {
        let request = &event["request"];
        let request_id = event["request_id"].as_str().unwrap_or("");
        let tool_name = request["tool_name"].as_str().unwrap_or("");
        let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
        let decision = self.policy.decide(tool_name, &input);
        let (behavior, message) = match &decision {
            policy::Decision::Allow => ("allow", Value::Null),
            policy::Decision::Deny(message) => ("deny", Value::String(message.clone())),
            policy::Decision::Ask(reason) => ("ask", Value::String(reason.clone())),
        };
        self.live.journal.record(
            Direction::Herdr,
            &json!({
                "type": "permission",
                "tool_name": tool_name,
                "tool_use_id": request.get("tool_use_id"),
                "decision": behavior,
                "message": message,
            }),
        );
        let response = match decision {
            policy::Decision::Allow => json!({"behavior": "allow", "updatedInput": input}),
            policy::Decision::Deny(message) => json!({"behavior": "deny", "message": message}),
            policy::Decision::Ask(reason) => {
                // No timer: the worker waits until the user answers.
                let question = question_from_request(request_id, request, &reason);
                let event = json!({"type": "question", "question": question, "input": input});
                self.live.journal.record(Direction::Herdr, &event);
                self.supervisor
                    .update(self.number, Direction::Herdr, &event);
                return;
            }
        };
        let answer = control_response(request_id, response);
        match self.live.send(&answer) {
            Ok(()) => self.supervisor.update(self.number, Direction::In, &answer),
            Err(error) => warn!(%error, "worker permission answer not delivered"),
        }
    }
}
