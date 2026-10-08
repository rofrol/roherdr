//! A headless worker's broker: a small process, the herdr binary in a hidden
//! mode (`herdr __worker-broker`), that owns the worker's pipes so the worker
//! outlives the server that started it.
//!
//! The server starts one broker per new worker. The broker leaves the
//! server's session, starts the `claude -p` worker as its child with the
//! three pipes, and serves them on a Unix socket next to the worker's
//! journal (`<worker dir>/<id>.sock`). A server reads the worker's stdout
//! and stderr lines and writes its stdin through that socket; a server that
//! starts while the worker runs connects to the socket again
//! ([`connect`]). One server is attached at a time: a new connection
//! replaces the old one. Only a server that holds the worker's journal lock
//! connects, so a replaced connection belongs to a server that is gone.
//!
//! The broker and the worker live and die together: the broker exits once
//! the worker exits and the attached server took the exit, and a guard
//! process kills the worker when the broker dies
//! ([`crate::platform::fork_death_guard`]). The broker does not watch the
//! server: it must outlive it.
//!
//! The socket opens nothing new to the worker: a sandbox that let it
//! connect to a Unix socket would let it reach herdr's API socket too.
//!
//! Slice 2 of worker survival: output the broker could not hand to a server
//! waits in its memory, bounded ([`OUTBOX_LIMIT`]; a line beyond it is
//! dropped and counted). Lines already written to a server that died before
//! storing them are lost: re-attaching does not replay them. Slice 3 adds a
//! spool with sequence numbers that a server re-attaches from.
//!
//! The wire, one line per message:
//! - broker to server: `p <pid> <protocol>` (the worker's pid and
//!   [`PROTOCOL`], first on every connection), `o <line>` (stdout),
//!   `e <line>` (stderr), `l <count>` (lines dropped), `x <json>` (the
//!   worker's `exited` event, last);
//! - server to broker: `i <line>` (one stdin line), `c` (close stdin).
//!
//! A line without its newline (a peer that died mid-write) is dropped.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::lock;

/// The hidden subcommand that runs a broker.
pub(crate) const BROKER_ARG: &str = "__worker-broker";
/// What the broker runs, as JSON ([`Spec`]); removed before the worker starts.
const SPEC_ENV: &str = "HERDR_WORKER_BROKER_SPEC";
/// The broker's report on its stdout once its socket listens and the
/// worker runs, with the worker's pid; or why it could not start.
const READY: &str = "herdr-worker-broker ready ";
const FAILED: &str = "herdr-worker-broker failed ";
/// The output the broker keeps while no server reads it, in bytes.
const OUTBOX_LIMIT: usize = 64 << 20;
/// The wire's version, in the greeting: a broker of another herdr build
/// that speaks another one is not re-attached to.
const PROTOCOL: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Spec {
    socket: PathBuf,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    /// The broker's stderr, removed at its end when nothing went wrong.
    log: PathBuf,
}

/// How a server starts a broker: herdr itself, or in tests the test binary
/// running [`tests::broker_process_entry`].
#[derive(Debug, Clone)]
pub(super) struct Launcher {
    exe: PathBuf,
    args: Vec<String>,
}

impl Launcher {
    pub(super) fn herdr() -> std::io::Result<Self> {
        Ok(Self {
            exe: crate::platform::launch_executable()?,
            args: vec![BROKER_ARG.to_owned()],
        })
    }
}

/// A broker that started its worker, connected.
pub(super) struct Started {
    /// The broker, this server's child: reap it once it ends.
    pub(super) process: Child,
    pub(super) link: Link,
}

/// Starts a broker that runs `program args` in `cwd`, serving it on
/// `socket`, and connects to it. `configure` sets the environment on the
/// broker's command; the worker inherits it. The broker's own errors go to
/// `log`.
pub(super) fn start(
    launcher: &Launcher,
    socket: &Path,
    log: &Path,
    program: &Path,
    args: &[String],
    cwd: &Path,
    configure: impl FnOnce(&mut Command),
) -> std::io::Result<Started> {
    let spec = serde_json::to_string(&Spec {
        socket: socket.to_owned(),
        program: program.to_owned(),
        args: args.to_vec(),
        cwd: cwd.to_owned(),
        log: log.to_owned(),
    })?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let mut command = Command::new(&launcher.exe);
    command.args(&launcher.args);
    configure(&mut command);
    command
        .env(SPEC_ENV, spec)
        // Out of the worker's directory, so ending a folder slot's leftover
        // processes does not take the broker for one.
        .current_dir(socket.parent().unwrap_or(Path::new("/")))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log);
    crate::platform::configure_background_command(&mut command);
    let mut process = command.spawn()?;
    let report = process.stdout.take().map(read_report);
    let pid = match report {
        Some(Ok(pid)) => pid,
        Some(Err(error)) => return Err(abandon(process, error)),
        None => return Err(abandon(process, std::io::Error::other("no broker stdout"))),
    };
    let link = match connect(socket) {
        Ok(link) => link,
        Err(error) => return Err(abandon(process, error)),
    };
    if link.pid != pid {
        let error = std::io::Error::other(format!(
            "the broker reported pid {pid}, its socket {}",
            link.pid
        ));
        return Err(abandon(process, error));
    }
    Ok(Started { process, link })
}

/// Reads the broker's report, skipping what is not one (a test binary
/// prints its own lines, and the start of the report's).
fn read_report(stdout: std::process::ChildStdout) -> std::io::Result<u32> {
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some((_, pid)) = line.split_once(READY) {
            return pid
                .trim()
                .parse()
                .map_err(|_| std::io::Error::other(format!("bad broker report: {line}")));
        }
        if let Some((_, error)) = line.split_once(FAILED) {
            return Err(std::io::Error::other(format!(
                "the worker broker failed: {error}"
            )));
        }
    }
    Err(std::io::Error::other(
        "the worker broker exited before it started the worker",
    ))
}

/// Ends a broker that did not start right. Its guard kills a worker it
/// started.
fn abandon(mut process: Child, error: std::io::Error) -> std::io::Error {
    let _ = process.kill();
    let _ = process.wait();
    error
}

/// A connection to a broker, after its greeting.
pub(super) struct Link {
    /// The worker's pid, as the broker greeted.
    pub(super) pid: u32,
    reader: BufReader<UnixStream>,
}

/// Connects to the broker serving `socket`. Fails when no broker serves it
/// (it ended, and with it its worker).
pub(super) fn connect(socket: &Path) -> std::io::Result<Link> {
    let stream = UnixStream::connect(socket)?;
    let mut reader = BufReader::new(stream);
    let greeting = read_message(&mut reader)?
        .ok_or_else(|| std::io::Error::other("the worker broker closed the connection"))?;
    let pid = greeted_pid(&greeting).ok_or_else(|| {
        std::io::Error::other(format!(
            "unexpected broker greeting: {}",
            String::from_utf8_lossy(&greeting)
        ))
    })?;
    Ok(Link { pid, reader })
}

/// The worker's pid in a greeting of this [`PROTOCOL`].
fn greeted_pid(greeting: &[u8]) -> Option<u32> {
    let (pid, protocol) = std::str::from_utf8(greeting.strip_prefix(b"p ")?)
        .ok()?
        .split_once(' ')?;
    (protocol.parse() == Ok(PROTOCOL)).then(|| pid.parse().ok())?
}

impl Link {
    /// The worker's stdin, and its output.
    pub(super) fn split(self) -> std::io::Result<(UnixStream, Messages)> {
        let input = self.reader.get_ref().try_clone()?;
        Ok((
            input,
            Messages {
                reader: self.reader,
            },
        ))
    }
}

/// Reads one whole message, without its newline; `None` at the end of the
/// stream or at a line cut off by it.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    if line.pop() != Some(b'\n') {
        return Ok(None);
    }
    Ok(Some(line))
}

/// What a broker sends a server.
#[derive(Debug, PartialEq)]
pub(super) enum Message {
    Out(String),
    Err(String),
    /// Lines dropped while no server read them.
    Lost(u64),
    /// The worker's `exited` event; the last message.
    Exit(Value),
}

/// The output of a worker, from its broker.
pub(super) struct Messages {
    reader: BufReader<UnixStream>,
}

impl Messages {
    /// The next message; `None` once the connection ended (the broker died,
    /// or this connection was cut).
    pub(super) fn next(&mut self) -> Option<Message> {
        loop {
            let line = read_message(&mut self.reader).ok()??;
            let text = |payload: &[u8]| String::from_utf8_lossy(payload).into_owned();
            let message = match line.split_first() {
                Some((b'o', rest)) => Message::Out(text(rest.get(1..).unwrap_or_default())),
                Some((b'e', rest)) => Message::Err(text(rest.get(1..).unwrap_or_default())),
                Some((b'l', rest)) => match text(rest).trim().parse() {
                    Ok(count) => Message::Lost(count),
                    Err(_) => continue,
                },
                Some((b'x', rest)) => match serde_json::from_slice(rest) {
                    Ok(exited) => Message::Exit(exited),
                    Err(_) => Message::Exit(json!({"type": "exited", "code": null})),
                },
                _ => continue,
            };
            return Some(message);
        }
    }
}

/// Writes one stdin line (`line` ends with its newline) in one write, so a
/// failed write leaves at most a cut line, which the broker drops.
pub(super) fn write_input(stream: &mut UnixStream, line: &str) -> std::io::Result<()> {
    let mut message = Vec::with_capacity(line.len() + 2);
    message.extend_from_slice(b"i ");
    message.extend_from_slice(line.as_bytes());
    if !message.ends_with(b"\n") {
        message.push(b'\n');
    }
    stream.write_all(&message).and_then(|()| stream.flush())
}

/// Closes the worker's stdin; it then ends once it read what it was sent.
pub(super) fn close_input(stream: &mut UnixStream) {
    let _ = stream.write_all(b"c\n");
}

/// The broker's output waiting for a server.
#[derive(Default)]
struct Outbox {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    /// Lines dropped since the last `l` message.
    lost: u64,
    /// The attached server, and how many have attached.
    conn: Option<Arc<UnixStream>>,
    generation: u64,
}

impl Outbox {
    /// Queues one message unless the outbox is full; the count of dropped
    /// lines goes first once there is room again.
    fn push(&mut self, message: Vec<u8>, limit: usize) {
        if self.lost > 0 {
            let note = format!("l {}\n", self.lost).into_bytes();
            if self.bytes + note.len() + message.len() > limit {
                self.lost += 1;
                return;
            }
            self.lost = 0;
            self.bytes += note.len();
            self.queue.push_back(note);
        }
        if self.bytes + message.len() > limit {
            self.lost += 1;
            return;
        }
        self.bytes += message.len();
        self.queue.push_back(message);
    }

    /// Queues the last message whatever the limit, after the dropped count.
    fn push_last(&mut self, message: Vec<u8>) {
        if self.lost > 0 {
            let note = format!("l {}\n", self.lost).into_bytes();
            self.lost = 0;
            self.bytes += note.len();
            self.queue.push_back(note);
        }
        self.bytes += message.len();
        self.queue.push_back(message);
    }
}

struct Shared {
    outbox: Mutex<Outbox>,
    changed: Condvar,
    stdin: Mutex<Option<ChildStdin>>,
    pid: u32,
}

/// The broker process's entry point: runs the [`Spec`] in [`SPEC_ENV`] and
/// returns the exit code.
pub(crate) fn serve_from_env() -> i32 {
    let spec = std::env::var(SPEC_ENV)
        .ok()
        .and_then(|spec| serde_json::from_str::<Spec>(&spec).ok());
    let Some(spec) = spec else {
        report(&format!("{FAILED}no valid {SPEC_ENV}"));
        return 2;
    };
    match serve(&spec) {
        Ok(()) => 0,
        Err(error) => {
            report(&format!("{FAILED}{error}"));
            eprintln!("herdr worker broker: {error}");
            let _ = std::fs::remove_file(&spec.socket);
            1
        }
    }
}

fn report(line: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}").and_then(|()| stdout.flush());
}

fn serve(spec: &Spec) -> std::io::Result<()> {
    let context = |what: &str| {
        let what = what.to_owned();
        move |error: std::io::Error| std::io::Error::new(error.kind(), format!("{what}: {error}"))
    };
    crate::platform::start_new_session().map_err(context("new session"))?;
    // Before any thread of the broker's own and before the worker, so the
    // guard holds none of their descriptors.
    let mut guard = crate::platform::fork_death_guard().map_err(context("death guard"))?;
    match std::fs::remove_file(&spec.socket) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(context("stale socket")(error))
        }
        _ => {}
    }
    let listener = UnixListener::bind(&spec.socket).map_err(context("socket"))?;
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_remove(SPEC_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::platform::configure_worker_process(&mut command);
    let mut child = command.spawn().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("cannot start {}: {error}", spec.program.display()),
        )
    })?;
    let pid = child.id();
    if let Err(error) = guard.arm(pid) {
        let _ = crate::platform::signal_process_group(pid, crate::platform::Signal::Kill);
        return Err(error);
    }
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(std::io::Error::other("worker pipes missing"));
    };
    let shared = Arc::new(Shared {
        outbox: Mutex::new(Outbox::default()),
        changed: Condvar::new(),
        stdin: Mutex::new(Some(stdin)),
        pid,
    });
    let accepting = Arc::clone(&shared);
    crate::thread_spawn::spawn_named("broker-accept", move || accept(&accepting, &listener))?;
    let errors = Arc::clone(&shared);
    crate::thread_spawn::spawn_named("broker-stderr", move || pump(&errors, stderr, b'e'))?;
    let sending = Arc::clone(&shared);
    let sender = crate::thread_spawn::spawn_named("broker-send", move || send(&sending))?;
    report(&format!("{READY}{pid}"));

    pump(&shared, stdout, b'o');
    // EOF: the worker closed stdout, so it exited or is about to; closing
    // its stdin lets one that waits for it end.
    lock(&shared.stdin).take();
    let exited = match child.wait() {
        Ok(status) => json!({
            "type": "exited",
            "code": status.code(),
            "signal": crate::platform::exit_status_signal(&status),
        }),
        Err(error) => json!({"type": "exited", "code": null, "error": error.to_string()}),
    };
    guard.disarm();
    let mut last = b"x ".to_vec();
    last.extend_from_slice(exited.to_string().as_bytes());
    last.push(b'\n');
    lock(&shared.outbox).push_last(last);
    shared.changed.notify_all();
    // Until a server took the exit: one that attaches later still gets it.
    let _ = sender.join();
    let _ = std::fs::remove_file(&spec.socket);
    if std::fs::metadata(&spec.log).is_ok_and(|log| log.len() == 0) {
        let _ = std::fs::remove_file(&spec.log);
    }
    Ok(())
}

/// Queues each line of a worker's stream, tagged.
fn pump(shared: &Shared, stream: impl Read, tag: u8) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        let mut message = Vec::with_capacity(line.len() + 3);
        message.extend_from_slice(&[tag, b' ']);
        message.extend_from_slice(&line);
        message.push(b'\n');
        lock(&shared.outbox).push(message, OUTBOX_LIMIT);
        shared.changed.notify_all();
    }
}

/// Attaches each connecting server in turn, greeting it with the worker's
/// pid; the previous connection is cut.
fn accept(shared: &Arc<Shared>, listener: &UnixListener) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if writeln!(stream, "p {} {PROTOCOL}", shared.pid).is_err() {
            continue;
        }
        let stream = Arc::new(stream);
        let previous = {
            let mut outbox = lock(&shared.outbox);
            outbox.generation += 1;
            outbox.conn.replace(Arc::clone(&stream))
        };
        shared.changed.notify_all();
        if let Some(previous) = previous {
            let _ = previous.shutdown(std::net::Shutdown::Both);
        }
        let receiving = Arc::clone(shared);
        if let Err(error) = crate::thread_spawn::spawn_named("broker-input", move || {
            receive(&receiving, &stream);
        }) {
            eprintln!("herdr worker broker: no input thread: {error}");
        }
    }
}

/// Writes each stdin line a server sends to the worker.
fn receive(shared: &Shared, stream: &UnixStream) {
    let mut reader = BufReader::new(stream);
    while let Ok(Some(message)) = read_message(&mut reader) {
        if let Some(line) = message.strip_prefix(b"i ") {
            let mut stdin = lock(&shared.stdin);
            let written = stdin.as_mut().map(|pipe| {
                pipe.write_all(line)
                    .and_then(|()| pipe.write_all(b"\n"))
                    .and_then(|()| pipe.flush())
            });
            if let Some(Err(_)) = written {
                // The worker is gone; its exit follows.
                stdin.take();
            }
        } else if message == b"c" {
            lock(&shared.stdin).take();
        }
    }
}

/// Sends the outbox to the attached server, oldest first; a message whose
/// write failed stays first for the next server. Returns once the exit
/// went out.
fn send(shared: &Shared) {
    loop {
        let (conn, generation, message) = {
            let mut outbox = lock(&shared.outbox);
            loop {
                if let Some(conn) = outbox.conn.clone() {
                    if let Some(message) = outbox.queue.pop_front() {
                        break (conn, outbox.generation, message);
                    }
                }
                outbox = shared
                    .changed
                    .wait(outbox)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let last = message.starts_with(b"x ");
        let written = (&*conn).write_all(&message).and_then(|()| (&*conn).flush());
        let mut outbox = lock(&shared.outbox);
        match written {
            Ok(()) => {
                outbox.bytes = outbox.bytes.saturating_sub(message.len());
                if last {
                    let _ = conn.shutdown(std::net::Shutdown::Write);
                    return;
                }
            }
            Err(_) => {
                outbox.queue.push_front(message);
                if outbox.generation == generation {
                    outbox.conn = None;
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Runs a broker when a test started this test binary as one
    /// ([`test_launcher`]); otherwise passes.
    #[test]
    fn broker_process_entry() {
        if std::env::var_os(SPEC_ENV).is_some() {
            std::process::exit(serve_from_env());
        }
    }

    /// Starts brokers as this test binary running [`broker_process_entry`].
    pub(in crate::workers) fn test_launcher() -> Launcher {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        Launcher {
            exe: std::env::current_exe().unwrap(),
            args: vec![
                "--exact".into(),
                format!("{module}::broker_process_entry"),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
        }
    }

    const GUARD_ENV: &str = "HERDR_TEST_DEATH_GUARD";

    /// Guards a `sleep` in its own process group, as a broker guards its
    /// worker, then waits to be killed; with `disarm`, disarms the guard
    /// first.
    #[test]
    fn guard_process_entry() {
        let Some(mode) = std::env::var_os(GUARD_ENV) else {
            return;
        };
        let mut guard = crate::platform::fork_death_guard().unwrap();
        let mut command = Command::new("sleep");
        command
            .arg("1000")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        crate::platform::configure_worker_process(&mut command);
        // Never waited for: this helper is killed, and the test's guard or
        // the test itself kills the sleeper, which the system then reaps.
        #[allow(clippy::zombie_processes)]
        let sleeper = command.spawn().unwrap();
        guard.arm(sleeper.id()).unwrap();
        if mode == "disarm" {
            guard.disarm();
        }
        report(&format!("guarded {}", sleeper.id()));
        loop {
            std::thread::park();
        }
    }

    /// Starts [`guard_process_entry`] and returns it with the guarded pid.
    fn guarded(mode: &str) -> (Child, u32) {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::guard_process_entry"),
                "--nocapture",
            ])
            .env(GUARD_ENV, mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = helper.stdout.take().unwrap();
        let pid = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.split_once("guarded ")?.1.trim().parse().ok())
            .unwrap();
        (helper, pid)
    }

    #[test]
    fn a_death_guard_kills_its_group_when_its_process_dies() {
        let (mut helper, pid) = guarded("kill");
        assert!(crate::platform::process_group_alive(pid));
        helper.kill().unwrap();
        helper.wait().unwrap();
        // The guard's kill sends this test no event: poll; the bound only
        // fails a broken test.
        let started = std::time::Instant::now();
        while crate::platform::process_group_alive(pid) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "the guard hung"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn a_disarmed_death_guard_leaves_its_group_alone() {
        let (mut helper, pid) = guarded("disarm");
        helper.kill().unwrap();
        helper.wait().unwrap();
        // Disarming reaped the guard, so nothing is left to kill it.
        assert!(crate::platform::process_group_alive(pid));
        assert!(crate::platform::signal_process_group(pid, crate::platform::Signal::Kill).unwrap());
    }

    #[test]
    fn a_full_outbox_drops_lines_and_counts_them_before_the_next() {
        let mut outbox = Outbox::default();
        outbox.push(b"o one\n".to_vec(), 12);
        outbox.push(b"o two\n".to_vec(), 12);
        outbox.push(b"o three\n".to_vec(), 12);
        assert_eq!(outbox.lost, 1);
        let sent = outbox.queue.pop_front().unwrap();
        outbox.bytes -= sent.len();
        outbox.push(b"o 4\n".to_vec(), 12);
        // No room for the count and the line together: both wait.
        assert_eq!(outbox.lost, 2);
        outbox.push_last(b"x {}\n".to_vec());
        let queued: Vec<&[u8]> = outbox.queue.iter().map(Vec::as_slice).collect();
        assert_eq!(queued, [&b"o two\n"[..], b"l 2\n", b"x {}\n"]);
    }

    #[test]
    fn a_greeting_of_another_protocol_is_refused() {
        assert_eq!(greeted_pid(format!("p 42 {PROTOCOL}").as_bytes()), Some(42));
        assert_eq!(
            greeted_pid(format!("p 42 {}", PROTOCOL + 1).as_bytes()),
            None
        );
        assert_eq!(greeted_pid(b"p 42"), None);
        assert_eq!(greeted_pid(b"o 42 1"), None);
    }

    #[test]
    fn messages_parse_and_a_cut_line_ends_the_stream() {
        let (mut broker, server) = UnixStream::pair().unwrap();
        broker
            .write_all(
                b"o {\"a\":1}\ne oops\nl 3\nq ignored\nx {\"type\":\"exited\",\"code\":0}\no cut",
            )
            .unwrap();
        drop(broker);
        let mut messages = Messages {
            reader: BufReader::new(server),
        };
        assert_eq!(messages.next(), Some(Message::Out("{\"a\":1}".into())));
        assert_eq!(messages.next(), Some(Message::Err("oops".into())));
        assert_eq!(messages.next(), Some(Message::Lost(3)));
        assert_eq!(
            messages.next(),
            Some(Message::Exit(json!({"type": "exited", "code": 0})))
        );
        assert_eq!(messages.next(), None);
    }
}
