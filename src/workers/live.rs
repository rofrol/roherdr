//! A running worker's process: how it is started ([`WorkerSupervisor::spawn_worker`]),
//! the input it is written through ([`Live`], a pipe this server owns or its
//! broker), and the thread that reads its output ([`Reader`]), records each
//! line and answers its requests until its exit.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tracing::warn;

#[cfg(unix)]
use super::broker;
use super::status::{Direction, Status};
use super::{
    control_response, lock, notify_clients, pi, policy, pre_tool_checks, question_from_request,
    remove_temp_dir, store, Receipt, Registry, WorkerError, WorkerSupervisor,
};

/// Where a running worker's stdin lines go.
pub(super) enum Input {
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
pub(super) struct Spawned {
    pub(super) pid: u32,
    pub(super) input: Input,
    pub(super) source: Source,
    /// Its broker's pid and socket, when it has one.
    pub(super) broker: Option<(u32, PathBuf)>,
}

/// Where a worker's output comes from.
pub(super) enum Source {
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
pub(super) fn input_id(event: &Value, receipt: Option<&Receipt>, seq: i64) -> String {
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

/// A pi worker's input beside its pipe ([`pi`]): whether herdr's extension
/// proved itself, and the prompt lines held back until it does.
#[derive(Debug, Default)]
struct PiInput {
    ready: bool,
    /// Each line's [`input_id`] and text.
    held: Vec<(String, String)>,
}

/// A running worker's input; absent for one loaded from the store.
pub(super) struct Live {
    pub(super) number: u64,
    stdin: Arc<Mutex<Option<Input>>>,
    /// Set for a pi worker, whose lines herdr translates ([`pi::input_line`]).
    pi: Option<Mutex<PiInput>>,
    /// This server let go of the worker's broker, handing the worker to the
    /// next server (a live handoff), or a test cut it off as a server that
    /// died would be: its reader ends without recording an exit.
    #[cfg(unix)]
    detached: std::sync::atomic::AtomicBool,
    /// Fails the next write to the pipe, as a broken pipe would.
    #[cfg(test)]
    pub(super) fail_next_write: std::sync::atomic::AtomicBool,
    /// Holds the next write to the pipe: it signals the first channel when
    /// reached and writes once the second gets a message, so a test can act
    /// between a recorded line and its write (a caller dying mid-answer).
    #[cfg(test)]
    pub(super) hold_next_write:
        Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

impl Live {
    pub(super) fn new(number: u64, stdin: Input) -> Self {
        Self::with_pi(number, stdin, None)
    }

    /// A pi worker's input: its prompts wait until herdr's extension proved
    /// itself, unless `ready` (a worker re-attached after that).
    pub(super) fn new_pi(number: u64, stdin: Input, ready: bool) -> Self {
        Self::with_pi(
            number,
            stdin,
            Some(PiInput {
                ready,
                held: Vec::new(),
            }),
        )
    }

    fn with_pi(number: u64, stdin: Input, pi: Option<PiInput>) -> Self {
        Self {
            number,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            pi: pi.map(Mutex::new),
            #[cfg(unix)]
            detached: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_write: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            hold_next_write: Mutex::new(None),
        }
    }

    pub(super) fn send(
        &self,
        supervisor: &WorkerSupervisor,
        event: &Value,
    ) -> Result<i64, WorkerError> {
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
    pub(super) fn send_checked(
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
        // A pi worker gets the line translated, or nothing for an event it
        // has no use for; its prompt waits for the gate.
        let Some(line) = self.line_for(event) else {
            return Ok(seq);
        };
        let id = input_id(event, receipt, seq);
        if let Some(pi) = &self.pi {
            let mut pi = lock(pi);
            if !pi.ready && pi::is_prompt(event) {
                pi.held.push((id, line));
                return Ok(seq);
            }
        }
        if let Err(error) = self.write_line(pipe, &id, &line) {
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
    pub(super) fn resend(&self, event: &Value, id: &str) -> std::io::Result<()> {
        let mut stdin = lock(&self.stdin);
        let Some(pipe) = stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the worker's input is closed",
            ));
        };
        match self.line_for(event) {
            Some(line) => self.write_line(pipe, id, &line),
            None => Ok(()),
        }
    }

    /// The line the worker's CLI gets for `event`, with its newline.
    fn line_for(&self, event: &Value) -> Option<String> {
        let mut line = match &self.pi {
            None => event.to_string(),
            Some(_) => pi::input_line(event)?.to_string(),
        };
        line.push('\n');
        Some(line)
    }

    /// Writes a pi worker's held prompts now that herdr's extension proved
    /// itself, and every later one at once.
    pub(super) fn release_held(&self) -> std::io::Result<()> {
        let mut stdin = lock(&self.stdin);
        let held = match &self.pi {
            Some(pi) => {
                let mut pi = lock(pi);
                pi.ready = true;
                std::mem::take(&mut pi.held)
            }
            None => return Ok(()),
        };
        let Some(pipe) = stdin.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the worker's input is closed",
            ));
        };
        for (id, line) in held {
            self.write_line(pipe, &id, &line)?;
        }
        Ok(())
    }

    pub(super) fn close_input(&self) {
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
    pub(super) fn detach(&self) -> bool {
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
    pub(super) fn sever(&self) {
        self.detach();
    }
}

/// What became of one line of a worker's output from its broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Spooled {
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
    pub(super) fn is_stored(self) -> bool {
        matches!(self, Self::Stored | Self::Duplicate)
    }
}

/// The event that a re-attach could not prove the worker's output whole.
#[cfg(unix)]
pub(super) fn continuity_gap_event(gap: &str) -> Value {
    json!({"type": "continuity_gap", "reason": gap})
}

/// The event for `lines` the broker dropped while its spool was full.
#[cfg(unix)]
pub(super) fn output_lost(lines: u64) -> Value {
    json!({
        "type": "output_lost",
        "lines": lines,
        "reason": "the broker's spool was full while no server stored its lines",
    })
}

/// The event that records a stdin line the broker wrote to the worker, by
/// its [`input_id`]; `again`: it had written it before and did not now.
#[cfg(unix)]
pub(super) fn input_written(id: &str, again: bool) -> Value {
    json!({"type": "input_written", "id": id, "again": again})
}

impl WorkerSupervisor {
    /// Starts a worker's process: through a broker when this server has a
    /// launcher, so the worker outlives the server; else with pipes this
    /// server owns. `configure` sets its environment.
    pub(super) fn spawn_worker(
        &self,
        worker_id: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        configure: impl Fn(&mut Command),
    ) -> Result<Spawned, WorkerError> {
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
}

/// The thread that owns one worker's stdout and process.
pub(super) struct Reader {
    pub(super) supervisor: WorkerSupervisor,
    pub(super) number: u64,
    pub(super) pid: u32,
    pub(super) policy: policy::Policy,
    /// The pre-tool checks its `hook_callback` requests run.
    pub(super) checks: pre_tool_checks::Runner,
    pub(super) temp_dir: PathBuf,
    /// The broker's spool, removed once the exit is stored.
    pub(super) spool: Option<PathBuf>,
    pub(super) live: Arc<Live>,
    /// This server's lock on the journal, let go once the exit is stored.
    pub(super) owner_lock: File,
    /// A pi worker's output translator ([`pi`]) and the file of herdr's
    /// extension, removed with the temp dir; none for Claude.
    pub(super) pi: Option<std::cell::RefCell<pi::Translator>>,
    pub(super) pi_extension: Option<PathBuf>,
    #[cfg(unix)]
    pub(super) credit_proxy: Option<crate::platform::anthropic_proxy::LoopbackProxy>,
}

impl Reader {
    /// Reads the stdout of a worker whose pipes this server owns, then
    /// reaps it.
    pub(super) fn run(self, stdout: std::process::ChildStdout, mut child: Child) {
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
    pub(super) fn run_broker(self, mut messages: broker::Messages, broker: Option<Child>) {
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
        let event = match &self.pi {
            None => event,
            Some(translator) => {
                let out = translator.borrow_mut().output(&event);
                match out {
                    pi::Out::Skip => return Spooled::Skipped,
                    pi::Out::Record(event) => event,
                    pi::Out::Cancel(input) => {
                        if let Err(error) = self.live.send(&self.supervisor, &input) {
                            warn!(%error, "pi dialog cancellation not delivered");
                        }
                        return Spooled::Skipped;
                    }
                    pi::Out::Init { event, gate_ready } => {
                        let spooled = self.record_line(
                            broker_seq,
                            Direction::Out,
                            store::Recorded::Event(&event),
                        );
                        if spooled != Spooled::Duplicate {
                            self.open_gate(gate_ready);
                        }
                        return spooled;
                    }
                }
            }
        };
        #[cfg(unix)]
        let mut event = event;
        // Held output waiting for its tool call goes into the turn it
        // happened in, before the turn's end.
        if event["type"].as_str() == Some("result") {
            #[cfg(unix)]
            if let Some(proxy) = &self.credit_proxy {
                match crate::platform::anthropic_proxy::AnthropicProxy::accounting(
                    &proxy.ledger,
                    Some(&proxy.run),
                ) {
                    Ok(accounting) if accounting.pending() == 0 => {
                        event["total_cost_usd"] = json!(accounting.spent_usd);
                        event["credit_accounting"] =
                            json!({"spent_usd": accounting.spent_usd, "worker": proxy.run});
                    }
                    _ => {
                        event["is_error"] = json!(true);
                        event["result"] =
                            json!("Anthropic credit accounting unavailable; spending is closed");
                        event["total_cost_usd"] = Value::Null;
                    }
                }
            }
            self.supervisor.flush_held_output(self.number, true);
        }
        let spooled = self.record_line(broker_seq, Direction::Out, store::Recorded::Event(&event));
        if event["type"].as_str() == Some("assistant") {
            self.supervisor.flush_held_output(self.number, false);
        }
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

    /// Once pi answered herdr's first command: writes the held prompt when
    /// herdr's extension reported itself before that answer; otherwise ends
    /// the worker before it gets any prompt, since its tools would run
    /// without herdr's gate and sandbox.
    fn open_gate(&self, gate_ready: bool) {
        if gate_ready {
            if let Err(error) = self.live.release_held() {
                warn!(%error, "pi worker prompt not delivered");
                self.supervisor.record(
                    self.number,
                    Direction::Herdr,
                    &json!({"type": "input_failed", "error": error.to_string()}),
                );
            }
            return;
        }
        self.supervisor.record(
            self.number,
            Direction::Herdr,
            &json!({
                "type": "pi_gate_missing",
                "error": "pi started without herdr's tool gate extension, so its tools would run \
                          unchecked and unsandboxed; the worker is ended before its prompt",
            }),
        );
        self.live.close_input();
        if let Err(error) =
            crate::platform::signal_process_group(self.pid, crate::platform::Signal::Kill)
        {
            warn!(%error, "pi worker without its gate not signalled");
        }
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
        if let Some(reason) = policy::background_wait_denial(tool, &tool_input)
            .or_else(|| policy::process_kill_denial(tool, &tool_input))
        {
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
    pub(super) fn settle_stored_requests(&self) {
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
        if let Some(extension) = &self.pi_extension {
            let _ = std::fs::remove_file(extension);
        }
        self.supervisor.flush_held_output(self.number, true);
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
