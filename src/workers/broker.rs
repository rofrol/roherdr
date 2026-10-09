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
//! The spool (slice 3 of worker survival): the broker gives every whole line
//! of the worker's stdout and stderr the next broker sequence number and
//! appends it to an append-only file next to the socket
//! (`<worker dir>/<id>.spool`, [`spool_path`]); a line without its newline
//! yet stays in the broker until the newline comes. A line goes to a server
//! only once it is written and synced. The batches are group commits, not
//! timed: one thread writes everything queued since its last sync in one
//! write and one `fsync`, while the lines that arrive meanwhile queue for
//! the next one. A server acknowledges each line's seq once the line's event
//! is committed in its store (which records that seq in the same
//! transaction), and a server that attaches names the last seq it stored:
//! the broker sends every line after it again, then the live ones, and the
//! server skips one its store already holds. Acknowledged lines leave the
//! broker's memory, and the file is rewritten without them once they
//! outweigh the rest ([`COMPACT_BYTES`]). The worker's exit is the spool's
//! last record, so a server that finds the broker gone reads the file
//! itself ([`read_spool`]). A spool the broker cannot write ends the worker,
//! whose exit then names the error (`spool_error`), rather than dropping
//! output silently; the lines go on from memory.
//!
//! The wire, one line per message:
//! - broker to server: `p <pid> <protocol>` (the worker's pid and
//!   [`PROTOCOL`], first on every connection), then the spool's records,
//!   each `<tag> <seq> <payload>`: `o` (a stdout line), `e` (a stderr line),
//!   `l <seq> <count>` (lines dropped while the spool was full), `n <seq>
//!   <id>[ again]` (a stdin line was written, or was not because its id
//!   was), `x <seq> <json>` (the worker's `exited` event, last);
//! - broker to server, right after an attach: `s <seq>`, the last seq it
//!   had dropped as acknowledged; past the attach's seq it proves a gap;
//! - server to broker: `a <seq>` (attach, first: send the records after
//!   `seq`), `k <seq>` (every record up to `seq` is stored), `i <id> <line>`
//!   (one stdin line), `c` (close stdin); a broker of protocol 2 takes
//!   `i <line>`, without an id, and spools no `n` records.
//!
//! The spool file holds the same records. A line without its newline (a
//! peer that died mid-write, a write cut short) is dropped.
//!
//! Stdin (slice 4): every line a server sends carries an id, the command's
//! receipt id or, for a `control_response`, its request's id
//! ([`Writer::write_input`]). The broker writes a line to the worker once
//! and then spools an `n <seq> <id>` record (the line was written); a line
//! whose id it already wrote is not written again, and its record says
//! `n <seq> <id> again`. So a server that does not know whether a gone
//! server's line reached the worker sends it again: it goes at most once,
//! and the record tells which. Ids are tokens: a byte that is whitespace,
//! `%` or not printable is written `%XX`.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
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
/// The unacknowledged output the broker keeps, in bytes, in its memory and
/// its spool. Beyond it, while no server stores the lines, a line is dropped
/// and counted (an `l` record), so a worker nobody reads cannot fill the
/// disk; 64 MiB holds hours of a worker's stream-json.
const SPOOL_LIMIT: usize = 64 << 20;
/// The acknowledged bytes still in the spool file that make the broker
/// rewrite it with only the unacknowledged records, once they also outweigh
/// those: the file stays under twice [`SPOOL_LIMIT`] plus this, and is not
/// rewritten for every acknowledgement.
const COMPACT_BYTES: u64 = 1 << 20;
/// The wire's version, in the greeting: a broker of another herdr build
/// that speaks another one is not re-attached to, but for
/// [`PROTOCOL_NO_INPUT_IDS`].
const PROTOCOL: u32 = 3;
/// The protocol of slice 3's brokers, which take stdin lines without ids:
/// still re-attached to, so a worker such a build started survives this
/// one's start, but a line it is sent again may go twice.
const PROTOCOL_NO_INPUT_IDS: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct Spec {
    socket: PathBuf,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    /// The broker's stderr, removed at its end when nothing went wrong.
    log: PathBuf,
}

/// The spool of the broker serving `socket`.
pub(super) fn spool_path(socket: &Path) -> PathBuf {
    socket.with_extension("spool")
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
    #[cfg(test)]
    tests::track(socket, process.id());
    let report = process.stdout.take().map(read_report);
    let pid = match report {
        Some(Ok(pid)) => pid,
        Some(Err(error)) => return Err(abandon(process, error)),
        None => return Err(abandon(process, std::io::Error::other("no broker stdout"))),
    };
    let link = match connect(socket, 0) {
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

/// A connection to a broker, attached.
pub(super) struct Link {
    /// The worker's pid, as the broker greeted.
    pub(super) pid: u32,
    /// The last seq the broker had dropped as acknowledged when this
    /// server attached: it replays only the records after it.
    pub(super) floor: u64,
    /// The broker's protocol, [`PROTOCOL`] or [`PROTOCOL_NO_INPUT_IDS`].
    protocol: u32,
    reader: BufReader<UnixStream>,
}

/// Connects to the broker serving `socket` and attaches after `after`, the
/// last broker seq this server's store holds: the broker answers with the
/// last seq it dropped ([`Link::floor`], see [`attach_gap`]) and sends the
/// records after `after`. Fails when no broker serves it (it ended, and with
/// it its worker); a broker of another [`PROTOCOL`] fails with
/// [`std::io::ErrorKind::Unsupported`].
pub(super) fn connect(socket: &Path, after: u64) -> std::io::Result<Link> {
    attach(UnixStream::connect(socket)?, after)
}

/// [`connect`] on a connected stream.
fn attach(stream: UnixStream, after: u64) -> std::io::Result<Link> {
    let mut reader = BufReader::new(stream);
    let greeting = read_message(&mut reader)?
        .ok_or_else(|| std::io::Error::other("the worker broker closed the connection"))?;
    let (pid, protocol) = greeted_pid(&greeting)?;
    reader
        .get_mut()
        .write_all(format!("a {after}\n").as_bytes())?;
    let attached = read_message(&mut reader)?
        .ok_or_else(|| std::io::Error::other("the worker broker closed the connection"))?;
    let floor = attached
        .strip_prefix(b"s ")
        .and_then(parse_seq)
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "unexpected broker answer to an attach: {}",
                String::from_utf8_lossy(&attached)
            ))
        })?;
    Ok(Link {
        pid,
        floor,
        protocol,
        reader,
    })
}

/// The worker's pid and the protocol in a greeting of this [`PROTOCOL`] or
/// [`PROTOCOL_NO_INPUT_IDS`]; a greeting of another one is `Unsupported`,
/// naming it.
fn greeted_pid(greeting: &[u8]) -> std::io::Result<(u32, u32)> {
    let unexpected = || {
        std::io::Error::other(format!(
            "unexpected broker greeting: {}",
            String::from_utf8_lossy(greeting)
        ))
    };
    let (pid, protocol) = std::str::from_utf8(greeting.strip_prefix(b"p ").ok_or_else(unexpected)?)
        .map_err(|_| unexpected())?
        .split_once(' ')
        .ok_or_else(unexpected)?;
    let pid = pid.parse().map_err(|_| unexpected())?;
    match protocol.parse::<u32>() {
        Ok(protocol @ (PROTOCOL | PROTOCOL_NO_INPUT_IDS)) => Ok((pid, protocol)),
        Ok(protocol) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "the worker's broker (worker pid {pid}) speaks wire protocol {protocol}, this \
                 server {PROTOCOL}{}",
                if protocol < 2 {
                    "; it keeps no output spool, so what it read while no server was attached \
                     cannot be replayed"
                } else {
                    ""
                }
            ),
        )),
        Err(_) => Err(unexpected()),
    }
}

/// Why a re-attach does not prove the worker's output whole: the broker had
/// dropped records up to `floor` as acknowledged, past `after`, the last one
/// this server's store holds. `None` when it replays every record after
/// `after`.
pub(super) fn attach_gap(floor: u64, after: u64) -> Option<String> {
    (floor > after).then(|| {
        format!(
            "the broker replays from seq {}, but the store holds only up to seq {after}: \
             lines {}..={floor} are missing",
            floor + 1,
            after + 1
        )
    })
}

impl Link {
    /// What writes to the broker, and the worker's output.
    pub(super) fn split(self) -> std::io::Result<(Writer, Messages)> {
        let writer = Writer {
            stream: Arc::new(Mutex::new(self.reader.get_ref().try_clone()?)),
            ids: self.protocol >= PROTOCOL,
        };
        Ok((
            writer.clone(),
            Messages {
                reader: self.reader,
                writer,
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

fn parse_seq(text: &[u8]) -> Option<u64> {
    std::str::from_utf8(text).ok()?.trim().parse().ok()
}

/// One spool record: `<tag> <seq> <payload>` and its newline.
fn record(tag: u8, seq: u64, payload: &[u8]) -> Vec<u8> {
    let mut record = Vec::with_capacity(payload.len() + 24);
    record.extend_from_slice(&[tag, b' ']);
    record.extend_from_slice(seq.to_string().as_bytes());
    record.push(b' ');
    record.extend_from_slice(payload);
    record.push(b'\n');
    record
}

/// A record without its newline, with its seq; `None` for what is not one.
fn parse_record(line: &[u8]) -> Option<(u64, Message)> {
    let (&tag, rest) = line.split_first()?;
    let rest = rest.strip_prefix(b" ")?;
    let (seq, payload) = match rest.iter().position(|byte| *byte == b' ') {
        Some(space) => (&rest[..space], &rest[space + 1..]),
        None => (rest, &b""[..]),
    };
    let seq = parse_seq(seq)?;
    let text = || String::from_utf8_lossy(payload).into_owned();
    let message = match tag {
        b'o' => Message::Out(text()),
        b'e' => Message::Err(text()),
        b'l' => Message::Lost(parse_seq(payload)?),
        b'n' => {
            let text = std::str::from_utf8(payload).ok()?;
            let (id, again) = match text.strip_suffix(" again") {
                Some(id) => (id, true),
                None => (text, false),
            };
            Message::Written {
                id: decode_id(id)?,
                again,
            }
        }
        b'x' => Message::Exit(
            serde_json::from_slice(payload)
                .unwrap_or_else(|_| json!({"type": "exited", "code": null})),
        ),
        _ => return None,
    };
    Some((seq, message))
}

/// What a spool holds after a seq ([`read_spool`]).
#[derive(Debug, PartialEq)]
pub(super) struct SpoolRead {
    /// The whole records after the seq, oldest first.
    pub(super) records: Vec<(u64, Message)>,
    /// Why they are not every line after the seq: a seq missing, a damaged
    /// record or a last one cut short. `None` when continuity is proven.
    pub(super) gap: Option<String>,
}

/// The records of the spool at `path` after `after`, the last seq the
/// store holds, with whether they continue it without a gap. For a server
/// that finds the broker gone.
pub(super) fn read_spool(path: &Path, after: u64) -> std::io::Result<SpoolRead> {
    let bytes = std::fs::read(path)?;
    let mut records = Vec::new();
    let mut gap = None;
    let mut expected = after + 1;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let Some(line) = line.strip_suffix(b"\n") else {
            gap.get_or_insert_with(|| "the spool ends in a record cut short".to_owned());
            break;
        };
        let Some((seq, message)) = parse_record(line) else {
            gap.get_or_insert_with(|| "the spool holds a damaged record".to_owned());
            continue;
        };
        if seq < expected {
            continue;
        }
        if seq > expected {
            gap.get_or_insert_with(|| {
                format!(
                    "the spool goes on at seq {seq} after seq {}: lines {expected}..={} are \
                     missing",
                    expected - 1,
                    seq - 1
                )
            });
        }
        expected = seq + 1;
        records.push((seq, message));
    }
    Ok(SpoolRead { records, gap })
}

/// What a broker sends a server.
#[derive(Debug, PartialEq)]
pub(super) enum Message {
    Out(String),
    Err(String),
    /// Lines dropped while the spool was full.
    Lost(u64),
    /// The stdin line with this id was written to the worker; `again` when
    /// it had been already, and this one was not.
    Written {
        id: String,
        again: bool,
    },
    /// The worker's `exited` event; the last message.
    Exit(Value),
}

/// The output of a worker, from its broker.
pub(super) struct Messages {
    reader: BufReader<UnixStream>,
    writer: Writer,
}

impl Messages {
    /// The next message with its broker seq; `None` once the connection
    /// ended (the broker died, or this connection was cut).
    pub(super) fn next(&mut self) -> Option<(u64, Message)> {
        loop {
            let line = read_message(&mut self.reader).ok()??;
            if let Some(record) = parse_record(&line) {
                return Some(record);
            }
        }
    }

    /// Tells the broker every record up to `seq` is stored. A failed write
    /// is a cut connection, which [`Self::next`] reports.
    pub(super) fn ack(&self, seq: u64) {
        let _ = self.writer.send(format!("k {seq}\n").as_bytes());
    }
}

/// Writes to a worker's broker: stdin lines and acknowledgements, from
/// several threads, each message in one write under one lock.
#[derive(Clone)]
pub(super) struct Writer {
    stream: Arc<Mutex<UnixStream>>,
    /// The broker takes stdin lines with ids ([`PROTOCOL`]).
    ids: bool,
}

impl Writer {
    fn send(&self, message: &[u8]) -> std::io::Result<()> {
        let mut stream = lock(&self.stream);
        stream.write_all(message).and_then(|()| stream.flush())
    }

    /// Writes one stdin line (`line` ends with its newline) in one write,
    /// so a failed write leaves at most a cut line, which the broker drops.
    /// The broker writes it to the worker unless it already wrote a line
    /// with this `id`, and spools which it did (a [`Message::Written`]).
    pub(super) fn write_input(&self, id: &str, line: &str) -> std::io::Result<()> {
        let mut message = Vec::with_capacity(line.len() + id.len() + 3);
        message.extend_from_slice(b"i ");
        if self.ids {
            message.extend_from_slice(encode_id(id).as_bytes());
            message.push(b' ');
        }
        message.extend_from_slice(line.as_bytes());
        if !message.ends_with(b"\n") {
            message.push(b'\n');
        }
        self.send(&message)
    }

    /// Closes the worker's stdin; it then ends once it read what it was sent.
    pub(super) fn close_input(&self) {
        let _ = self.send(b"c\n");
    }

    /// Cuts the connection for a server that hands its workers over: its
    /// reader ends, and the broker waits for the next server.
    pub(super) fn detach(&self) {
        let _ = lock(&self.stream).shutdown(std::net::Shutdown::Both);
    }
}

/// An input id as a token ([`Writer::write_input`]).
fn encode_id(id: &str) -> String {
    let mut encoded = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_graphic() && byte != b'%' {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The id [`encode_id`] encoded; `None` for what it cannot have written.
fn decode_id(token: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(token.len());
    let mut rest = token.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

/// The records a broker keeps: those not written to the spool file yet
/// (`pending`), and those written and synced that no server acknowledged
/// (`durable`), each in seq order, the seqs contiguous.
#[derive(Default)]
struct Spool {
    /// The seq of the last record queued.
    seq: u64,
    pending: Vec<(u64, Vec<u8>)>,
    pending_bytes: usize,
    durable: VecDeque<(u64, Vec<u8>)>,
    durable_bytes: usize,
    /// The bytes in the spool file, acknowledged records included.
    file_bytes: u64,
    /// The last seq a server acknowledged.
    acked: u64,
    /// Lines dropped since the last `l` record.
    lost: u64,
    /// Why the file could not be written; from then on records are only
    /// kept in memory.
    failed: Option<String>,
    /// The exit is queued: nothing comes after it.
    closed: bool,
}

/// Records taken for one write of the spool file.
struct Batch {
    records: Vec<(u64, Vec<u8>)>,
    /// The unacknowledged records already written, when the file is
    /// rewritten with only them before `records`.
    kept: Option<Vec<u8>>,
    /// False once the file failed: the records are only published.
    write: bool,
}

impl Batch {
    fn is_last(&self) -> bool {
        self.records
            .last()
            .is_some_and(|(_, record)| record.starts_with(b"x "))
    }
}

impl Spool {
    fn bytes(&self) -> usize {
        self.pending_bytes + self.durable_bytes
    }

    fn push(&mut self, tag: u8, payload: &[u8]) {
        self.seq += 1;
        let record = record(tag, self.seq, payload);
        self.pending_bytes += record.len();
        self.pending.push((self.seq, record));
    }

    /// Queues one line unless the spool is full; the count of dropped lines
    /// goes first once there is room again.
    fn queue(&mut self, tag: u8, payload: &[u8], limit: usize) {
        if self.closed {
            return;
        }
        let mut seq = self.seq;
        let note = (self.lost > 0).then(|| {
            seq += 1;
            record(b'l', seq, self.lost.to_string().as_bytes())
        });
        let size = note.as_ref().map_or(0, Vec::len) + record(tag, seq + 1, payload).len();
        if self.bytes() + size > limit {
            self.lost += 1;
            return;
        }
        if note.is_some() {
            self.push(b'l', self.lost.to_string().as_bytes());
            self.lost = 0;
        }
        self.push(tag, payload);
    }

    /// Queues the last record whatever the limit, after the dropped count.
    fn queue_last(&mut self, tag: u8, payload: &[u8]) {
        if self.closed {
            return;
        }
        if self.lost > 0 {
            self.push(b'l', self.lost.to_string().as_bytes());
            self.lost = 0;
        }
        self.push(tag, payload);
        self.closed = true;
    }

    /// Whether the acknowledged records in the file are due to go.
    fn should_compact(&self) -> bool {
        let durable = self.durable_bytes as u64;
        let stale = self.file_bytes.saturating_sub(durable);
        stale >= COMPACT_BYTES && stale >= durable
    }

    /// The next write: every pending record, and the file's rewrite when it
    /// is due; `None` when there is nothing to do.
    fn next_batch(&mut self) -> Option<Batch> {
        let write = self.failed.is_none();
        let rewrite = write && self.should_compact();
        if self.pending.is_empty() && !rewrite {
            return None;
        }
        let kept = rewrite.then(|| {
            self.durable
                .iter()
                .flat_map(|(_, record)| record.iter().copied())
                .collect()
        });
        Some(Batch {
            records: std::mem::take(&mut self.pending),
            kept,
            write,
        })
    }

    /// Publishes a batch once its write ended with `result`. Returns the
    /// error when this write is the one that failed the file.
    fn written(&mut self, batch: Batch, result: std::io::Result<()>) -> Option<String> {
        let mut failed = None;
        if batch.write {
            match result {
                Ok(()) => {
                    if let Some(kept) = &batch.kept {
                        self.file_bytes = kept.len() as u64;
                    }
                    self.file_bytes += batch
                        .records
                        .iter()
                        .map(|(_, record)| record.len() as u64)
                        .sum::<u64>();
                }
                Err(error) => {
                    let error = error.to_string();
                    self.failed = Some(error.clone());
                    failed = Some(error);
                }
            }
        }
        for (seq, record) in batch.records {
            self.pending_bytes -= record.len();
            if seq > self.acked {
                self.durable_bytes += record.len();
                self.durable.push_back((seq, record));
            }
        }
        failed
    }

    /// Drops every record up to `seq`, which a server stored.
    fn ack(&mut self, seq: u64) {
        if seq <= self.acked {
            return;
        }
        self.acked = seq;
        while let Some((front, record)) = self.durable.front() {
            if *front > seq {
                break;
            }
            self.durable_bytes -= record.len();
            self.durable.pop_front();
        }
    }

    /// The first published record after `seq`.
    fn after(&self, seq: u64) -> Option<&(u64, Vec<u8>)> {
        let first = self.durable.front()?.0;
        let index = (seq + 1).saturating_sub(first);
        self.durable.get(usize::try_from(index).ok()?)
    }
}

/// The spool file, written by one thread.
struct SpoolFile {
    path: PathBuf,
    file: File,
}

impl SpoolFile {
    /// Creates the spool at `path`, empty.
    fn create(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            path: path.to_owned(),
            file,
        })
    }

    /// Appends a batch and syncs it; a rewrite goes to a new file that
    /// replaces the old one once it is synced, so a crash leaves one or the
    /// other whole.
    fn write(&mut self, batch: &Batch) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        for (_, record) in &batch.records {
            bytes.extend_from_slice(record);
        }
        match &batch.kept {
            Some(kept) => {
                let temp = self.path.with_extension("spool-new");
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&temp)?;
                file.write_all(kept)?;
                file.write_all(&bytes)?;
                file.sync_data()?;
                std::fs::rename(&temp, &self.path)?;
                self.file = file;
            }
            None => {
                self.file.write_all(&bytes)?;
                self.file.sync_data()?;
            }
        }
        Ok(())
    }
}

/// The broker's state, under one lock.
#[derive(Default)]
struct State {
    spool: Spool,
    /// The attached server, and how many have attached.
    conn: Option<Arc<UnixStream>>,
    generation: u64,
    /// The last seq sent to the attached server.
    cursor: u64,
}

/// The worker's stdin, and the ids of the lines written to it.
#[derive(Default)]
struct Stdin {
    pipe: Option<ChildStdin>,
    written: HashSet<String>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    stdin: Mutex<Stdin>,
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
    // First: the broker outlives whoever started it, so it must not keep
    // their descriptors (a test runner's lock held forever), nor pass them
    // to its guard or its worker.
    crate::platform::close_inherited_descriptors().map_err(context("inherited descriptors"))?;
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
    let spool = SpoolFile::create(&spool_path(&spec.socket)).map_err(context("spool"))?;
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
        state: Mutex::new(State::default()),
        changed: Condvar::new(),
        stdin: Mutex::new(Stdin {
            pipe: Some(stdin),
            written: HashSet::new(),
        }),
        pid,
    });
    let accepting = Arc::clone(&shared);
    crate::thread_spawn::spawn_named("broker-accept", move || accept(&accepting, &listener))?;
    let errors = Arc::clone(&shared);
    crate::thread_spawn::spawn_named("broker-stderr", move || pump(&errors, stderr, b'e'))?;
    let flushing = Arc::clone(&shared);
    let flusher =
        crate::thread_spawn::spawn_named("broker-spool", move || flush(&flushing, spool))?;
    let sending = Arc::clone(&shared);
    let sender = crate::thread_spawn::spawn_named("broker-send", move || send(&sending))?;
    report(&format!("{READY}{pid}"));

    pump(&shared, stdout, b'o');
    // EOF: the worker closed stdout, so it exited or is about to; closing
    // its stdin lets one that waits for it end.
    lock(&shared.stdin).pipe.take();
    let mut exited = match child.wait() {
        Ok(status) => json!({
            "type": "exited",
            "code": status.code(),
            "signal": crate::platform::exit_status_signal(&status),
        }),
        Err(error) => json!({"type": "exited", "code": null, "error": error.to_string()}),
    };
    guard.disarm();
    {
        let mut state = lock(&shared.state);
        if let Some(error) = &state.spool.failed {
            exited["spool_error"] = Value::String(error.clone());
        }
        state.spool.queue_last(b'x', exited.to_string().as_bytes());
    }
    shared.changed.notify_all();
    // Until a server took the exit: one that attaches later still gets it.
    let _ = flusher.join();
    let _ = sender.join();
    // The spool stays: the server that stores the exit removes it, and one
    // that died before that reads it.
    let _ = std::fs::remove_file(&spec.socket);
    if std::fs::metadata(&spec.log).is_ok_and(|log| log.len() == 0) {
        let _ = std::fs::remove_file(&spec.log);
    }
    Ok(())
}

/// Queues each whole line of a worker's stream, tagged; a last line
/// without its newline is queued at the stream's end.
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
        lock(&shared.state).spool.queue(tag, &line, SPOOL_LIMIT);
        shared.changed.notify_all();
    }
}

/// Writes the queued records to the spool file in batches, then publishes
/// them. A write that fails ends the worker; its records, and those after,
/// are published unwritten. Returns once the exit is published.
fn flush(shared: &Shared, mut file: SpoolFile) {
    loop {
        let batch = {
            let mut state = lock(&shared.state);
            loop {
                if let Some(batch) = state.spool.next_batch() {
                    break batch;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let result = if batch.write {
            file.write(&batch)
        } else {
            Ok(())
        };
        let last = batch.is_last();
        let failed = lock(&shared.state).spool.written(batch, result);
        shared.changed.notify_all();
        if let Some(error) = failed {
            eprintln!(
                "herdr worker broker: cannot write the output spool, ending the worker: {error}"
            );
            let _ =
                crate::platform::signal_process_group(shared.pid, crate::platform::Signal::Kill);
        }
        if last {
            return;
        }
    }
}

/// Greets each connecting server with the worker's pid; it attaches with
/// its first message ([`receive`]).
fn accept(shared: &Arc<Shared>, listener: &UnixListener) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if writeln!(stream, "p {} {PROTOCOL}", shared.pid).is_err() {
            continue;
        }
        let receiving = Arc::clone(shared);
        let stream = Arc::new(stream);
        if let Err(error) = crate::thread_spawn::spawn_named("broker-input", move || {
            receive(&receiving, stream);
        }) {
            eprintln!("herdr worker broker: no input thread: {error}");
        }
    }
}

/// Attaches a server once it named the last seq it stored, cutting the
/// previous one, then takes its messages: stdin lines for the worker and
/// acknowledgements.
fn receive(shared: &Shared, stream: Arc<UnixStream>) {
    let mut reader = BufReader::new(&*stream);
    let after = match read_message(&mut reader) {
        Ok(Some(message)) => message.strip_prefix(b"a ").and_then(parse_seq),
        _ => None,
    };
    let Some(after) = after else { return };
    let previous = {
        let mut state = lock(&shared.state);
        // Under the lock, so no record is dropped between the answer and
        // the attach: the server checks it replays everything after `after`.
        let floor = state.spool.acked;
        if writeln!(&*stream, "s {floor}").is_err() {
            return;
        }
        state.spool.ack(after);
        state.generation += 1;
        state.cursor = after;
        state.conn.replace(Arc::clone(&stream))
    };
    shared.changed.notify_all();
    if let Some(previous) = previous {
        let _ = previous.shutdown(std::net::Shutdown::Both);
    }
    while let Ok(Some(message)) = read_message(&mut reader) {
        if let Some(input) = message.strip_prefix(b"i ") {
            write_input(shared, input);
        } else if let Some(seq) = message.strip_prefix(b"k ").and_then(parse_seq) {
            lock(&shared.state).spool.ack(seq);
            // The flusher may rewrite the file now.
            shared.changed.notify_all();
        } else if message == b"c" {
            lock(&shared.stdin).pipe.take();
        }
    }
}

/// Writes one `<id> <line>` to the worker unless a line with that id was
/// written, and spools which happened. The record is queued after the
/// write, so it never claims a line the worker could not read; a line the
/// worker answers quickly may be spooled before it.
fn write_input(shared: &Shared, input: &[u8]) {
    let Some(space) = input.iter().position(|byte| *byte == b' ') else {
        return;
    };
    let (id, line) = (&input[..space], &input[space + 1..]);
    let id = String::from_utf8_lossy(id).into_owned();
    let mut stdin = lock(&shared.stdin);
    let again = stdin.written.contains(&id);
    if !again {
        let Some(pipe) = stdin.pipe.as_mut() else {
            // Closed: the worker ends; nothing was written.
            return;
        };
        let written = pipe
            .write_all(line)
            .and_then(|()| pipe.write_all(b"\n"))
            .and_then(|()| pipe.flush());
        if written.is_err() {
            // The worker is gone; its exit follows.
            stdin.pipe.take();
            return;
        }
        stdin.written.insert(id.clone());
    }
    let mut payload = id.into_bytes();
    if again {
        payload.extend_from_slice(b" again");
    }
    // Under the stdin lock, so two writes' records keep their order.
    lock(&shared.state).spool.queue(b'n', &payload, SPOOL_LIMIT);
    drop(stdin);
    shared.changed.notify_all();
}

/// Sends the published records after the attached server's cursor, oldest
/// first; a record whose write failed is sent again to the next server.
/// Returns once the exit went out.
fn send(shared: &Shared) {
    loop {
        let (conn, generation, seq, message) = {
            let mut state = lock(&shared.state);
            loop {
                if let Some(conn) = state.conn.clone() {
                    if let Some((seq, message)) = state.spool.after(state.cursor) {
                        break (conn, state.generation, *seq, message.clone());
                    }
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let last = message.starts_with(b"x ");
        let written = (&*conn).write_all(&message).and_then(|()| (&*conn).flush());
        let mut state = lock(&shared.state);
        if state.generation != generation {
            continue;
        }
        match written {
            Ok(()) => {
                state.cursor = seq;
                if last {
                    let _ = conn.shutdown(std::net::Shutdown::Write);
                    return;
                }
            }
            Err(_) => state.conn = None,
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

    /// Every broker this test binary started, with its socket, until a
    /// [`Reaper`] ends it.
    static STARTED: Mutex<Vec<(PathBuf, u32)>> = Mutex::new(Vec::new());

    pub(super) fn track(socket: &Path, pid: u32) {
        lock(&STARTED).push((socket.to_owned(), pid));
    }

    /// Ends and reaps, when dropped, every broker started with its socket
    /// under `root`, so a test leaves none behind, also when it fails: a
    /// broker outlives its server by design, and one left running holds
    /// what it holds until someone kills it.
    pub(in crate::workers) struct Reaper {
        root: PathBuf,
    }

    impl Reaper {
        pub(in crate::workers) fn new(root: &Path) -> Self {
            Self {
                root: root.to_owned(),
            }
        }
    }

    impl Drop for Reaper {
        fn drop(&mut self) {
            let mine: Vec<u32> = {
                let mut started = lock(&STARTED);
                let (mine, others) = started
                    .drain(..)
                    .partition(|(socket, _)| socket.starts_with(&self.root));
                *started = others;
                mine.into_iter().map(|(_, pid)| pid).collect()
            };
            for pid in mine {
                reap(pid);
            }
        }
    }

    /// Kills and reaps the broker `pid`, this process's child, unless it was
    /// reaped already (its server waited for it). An unreaped child keeps
    /// its pid, so the kill cannot reach another process.
    fn reap(pid: u32) {
        let pid = pid as libc::pid_t;
        let mut status = 0;
        // 0: still running; its pid, now reaped; -1: reaped by its server.
        if unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } != 0 {
            return;
        }
        // Its death guard ends its worker.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, &mut status, 0);
        }
    }

    /// Whether `pid` is a process at all (a zombie counts).
    fn exists(pid: u32) -> bool {
        let found = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    const INHERIT_ENV: &str = "HERDR_TEST_INHERITED_LOCK";

    /// Takes a `flock` on the file in [`INHERIT_ENV`] through a descriptor
    /// its children inherit, starts a broker (worker `sleep`), closes its own
    /// copy and reports the broker's pid; at the end of its stdin, ends the
    /// broker.
    #[test]
    fn inherit_process_entry() {
        use std::os::fd::AsRawFd;

        let Some(path) = std::env::var_os(INHERIT_ENV) else {
            return;
        };
        let path = PathBuf::from(path);
        let file = File::create(&path).unwrap();
        let fd = file.as_raw_fd();
        assert_eq!(unsafe { libc::flock(fd, libc::LOCK_EX) }, 0);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        let dir = path.parent().unwrap();
        let started = start(
            &test_launcher(),
            &dir.join("w.sock"),
            &dir.join("w.log"),
            Path::new("sleep"),
            &["1000".to_owned()],
            dir,
            |_| {},
        )
        .unwrap();
        drop(file);
        report(&format!("brokered {}", started.process.id()));
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        let mut process = started.process;
        let _ = process.kill();
        let _ = process.wait();
    }

    /// A helper process that ends with its stdin: dropped, it closes its
    /// stdin and waits for it.
    struct Helper(Child);

    impl Drop for Helper {
        fn drop(&mut self) {
            self.0.stdin.take();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_broker_does_not_hold_a_descriptor_its_starter_inherited() {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        // Short: a socket's path has at most 103 bytes.
        let dir = std::env::temp_dir().join(format!("hi{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("lock");
        let mut helper = Helper(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    &format!("{module}::inherit_process_entry"),
                    "--nocapture",
                ])
                .env(INHERIT_ENV, &lock_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = helper.0.stdout.take().unwrap();
        let broker: u32 = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.split_once("brokered ")?.1.trim().parse().ok())
            .expect("the helper started no broker");
        assert!(exists(broker));

        // The helper closed its copy: only a broker that kept the inherited
        // one still holds the lock.
        let file = File::open(&lock_path).unwrap();
        let taken = unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        assert_eq!(taken, 0, "the broker holds its starter's lock");

        drop(helper);
        let _ = std::fs::remove_dir_all(&dir);
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

    /// A [`guard_process_entry`] helper and the group it guards: dropped,
    /// it kills and reaps the helper and kills the group, so a failing test
    /// leaves neither.
    struct Guarded {
        helper: Child,
        group: u32,
    }

    impl Drop for Guarded {
        fn drop(&mut self) {
            let _ = self.helper.kill();
            let _ = self.helper.wait();
            if crate::platform::process_group_alive(self.group) {
                let _ = crate::platform::signal_process_group(
                    self.group,
                    crate::platform::Signal::Kill,
                );
            }
        }
    }

    /// Starts [`guard_process_entry`] and returns it with the guarded pid.
    fn guarded(mode: &str) -> Guarded {
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
        Guarded { helper, group: pid }
    }

    #[test]
    fn a_death_guard_kills_its_group_when_its_process_dies() {
        let mut guarded = guarded("kill");
        let pid = guarded.group;
        assert!(crate::platform::process_group_alive(pid));
        guarded.helper.kill().unwrap();
        guarded.helper.wait().unwrap();
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
        let mut guarded = guarded("disarm");
        let pid = guarded.group;
        guarded.helper.kill().unwrap();
        guarded.helper.wait().unwrap();
        // Disarming reaped the guard, so nothing is left to kill it.
        assert!(crate::platform::process_group_alive(pid));
        assert!(crate::platform::signal_process_group(pid, crate::platform::Signal::Kill).unwrap());
    }

    /// Writes every batch the spool has to `file`, as the flusher does.
    fn flush_all(spool: &mut Spool, file: &mut SpoolFile) -> Option<String> {
        let mut failed = None;
        while let Some(batch) = spool.next_batch() {
            let result = if batch.write {
                file.write(&batch)
            } else {
                Ok(())
            };
            failed = failed.or(spool.written(batch, result));
        }
        failed
    }

    fn sent(spool: &Spool, mut cursor: u64) -> Vec<String> {
        let mut sent = Vec::new();
        while let Some((seq, record)) = spool.after(cursor) {
            cursor = *seq;
            sent.push(String::from_utf8_lossy(record).trim_end().to_owned());
        }
        sent
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-spool-{name}-{}-{}",
            std::process::id(),
            crate::workers::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("w1.spool")
    }

    #[test]
    fn a_full_spool_drops_lines_and_counts_them_before_the_next() {
        let mut spool = Spool::default();
        spool.queue(b'o', b"one", 16);
        spool.queue(b'o', b"two", 16);
        spool.queue(b'o', b"three", 16);
        assert_eq!(spool.lost, 1);
        let batch = spool.next_batch().unwrap();
        spool.written(batch, Ok(()));
        spool.ack(1);
        spool.queue(b'o', b"4", 16);
        // No room for the count and the line together: both wait.
        assert_eq!(spool.lost, 2);
        spool.queue_last(b'x', b"{}");
        spool.queue(b'e', b"after the exit", 1 << 20);
        let batch = spool.next_batch().unwrap();
        assert!(batch.is_last());
        spool.written(batch, Ok(()));
        assert_eq!(sent(&spool, 0), ["o 2 two", "l 3 2", "x 4 {}"]);
    }

    #[test]
    fn records_go_out_after_the_attached_seq_once_written_and_the_file_replays_them() {
        let path = scratch("replay");
        let mut file = SpoolFile::create(&path).unwrap();
        let mut spool = Spool::default();
        spool.queue(b'o', b"{\"type\":\"system\"}", SPOOL_LIMIT);
        spool.queue(b'e', b"warning", SPOOL_LIMIT);
        // Queued, not written: nothing goes out yet.
        assert!(sent(&spool, 0).is_empty());
        assert!(flush_all(&mut spool, &mut file).is_none());
        spool.queue(b'o', b"partial", SPOOL_LIMIT);
        assert_eq!(sent(&spool, 0).len(), 2);
        assert!(flush_all(&mut spool, &mut file).is_none());
        // A server that stored seq 1 attaches: 2 and 3 come again, no gap.
        spool.ack(1);
        assert_eq!(sent(&spool, 1), ["e 2 warning", "o 3 partial"]);
        // One whose store holds 2 (a crash between its commit and its
        // ack) gets only 3.
        assert_eq!(sent(&spool, 2), ["o 3 partial"]);
        // The file holds them all, and a server that finds the broker gone
        // reads them, continuity proven.
        assert_eq!(
            read_spool(&path, 1).unwrap(),
            SpoolRead {
                records: vec![
                    (2, Message::Err("warning".into())),
                    (3, Message::Out("partial".into()))
                ],
                gap: None,
            }
        );
        assert_eq!(read_spool(&path, 0).unwrap().records.len(), 3);
        assert_eq!(read_spool(&path, 3).unwrap().records, []);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    fn spool_with(name: &str, content: &[u8]) -> PathBuf {
        let path = scratch(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn a_spool_proves_continuity_only_without_a_missing_damaged_or_cut_record() {
        // Compacted: it starts after acknowledged records, at most one past
        // the store's seq.
        let path = spool_with("whole", b"o 5 five\no 6 six\n");
        let read = read_spool(&path, 4).unwrap();
        assert_eq!(read.gap, None);
        assert_eq!(read.records.len(), 2);
        assert_eq!(read_spool(&path, 7).unwrap().gap, None);

        // It starts after the store's seq: lines 3 and 4 are gone.
        let gap = read_spool(&path, 2).unwrap();
        assert_eq!(
            gap.gap.as_deref(),
            Some("the spool goes on at seq 5 after seq 2: lines 3..=4 are missing")
        );
        // What is there is still recorded.
        assert_eq!(gap.records.len(), 2);

        let path = spool_with("jump", b"o 1 one\no 3 three\n");
        assert!(read_spool(&path, 0)
            .unwrap()
            .gap
            .unwrap()
            .contains("lines 2..=2"));

        let path = spool_with("damaged", b"o 1 one\n\x00garbage\no 2 two\n");
        assert_eq!(
            read_spool(&path, 0).unwrap().gap.as_deref(),
            Some("the spool holds a damaged record")
        );

        let path = spool_with("cut", b"o 1 one\no 2 tw");
        let read = read_spool(&path, 0).unwrap();
        assert_eq!(
            read.gap.as_deref(),
            Some("the spool ends in a record cut short")
        );
        assert_eq!(read.records, [(1, Message::Out("one".into()))]);

        assert_eq!(
            read_spool(&scratch("missing"), 0).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    /// A broker's state with records 1..=`published` published and those up
    /// to `acked` acknowledged.
    fn shared_with(published: u64, acked: u64) -> Arc<Shared> {
        let mut spool = Spool::default();
        for n in 1..=published {
            spool.queue(b'o', n.to_string().as_bytes(), SPOOL_LIMIT);
        }
        let batch = spool.next_batch().unwrap();
        spool.written(batch, Ok(()));
        spool.ack(acked);
        Arc::new(Shared {
            state: Mutex::new(State {
                spool,
                ..State::default()
            }),
            changed: Condvar::new(),
            stdin: Mutex::new(Stdin::default()),
            pid: 42,
        })
    }

    /// Attaches a server after `after` to a broker in `shared`, over a
    /// socket pair, as [`accept`] would.
    fn attach_to(shared: &Arc<Shared>, after: u64) -> std::io::Result<Link> {
        let (mut broker, server) = UnixStream::pair().unwrap();
        writeln!(broker, "p {} {PROTOCOL}", shared.pid).unwrap();
        let receiving = Arc::clone(shared);
        std::thread::spawn(move || receive(&receiving, Arc::new(broker)));
        attach(server, after)
    }

    #[test]
    fn an_attach_proves_continuity_when_the_broker_kept_every_record_after_it() {
        let shared = shared_with(5, 2);
        let link = attach_to(&shared, 3).unwrap();
        assert_eq!((link.pid, link.floor), (42, 2));
        assert_eq!(attach_gap(link.floor, 3), None);
        // The attach acknowledged what the server stored.
        assert_eq!(lock(&shared.state).spool.acked, 3);
        // Exactly one past the store's seq is continuous too.
        assert_eq!(attach_to(&shared, 3).unwrap().floor, 3);
    }

    #[test]
    fn an_attach_behind_the_brokers_acknowledged_records_names_the_gap() {
        // A store that lost its last commits (an OS crash), or the store of
        // another server: the broker dropped 3..=4 already.
        let shared = shared_with(5, 4);
        let link = attach_to(&shared, 2).unwrap();
        assert_eq!(link.floor, 4);
        assert_eq!(
            attach_gap(link.floor, 2).as_deref(),
            Some("the broker replays from seq 5, but the store holds only up to seq 2: lines 3..=4 are missing")
        );
    }

    #[test]
    fn a_broker_of_the_slice_2_protocol_is_refused_as_unsupported() {
        let (mut broker, server) = UnixStream::pair().unwrap();
        writeln!(broker, "p 42 1").unwrap();
        let error = attach(server, 0).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(
            error.to_string().contains("keeps no output spool"),
            "{error}"
        );
    }

    #[test]
    fn the_spool_file_is_truncated_up_to_the_acked_seq_and_keeps_the_rest() {
        let path = scratch("truncate");
        let mut file = SpoolFile::create(&path).unwrap();
        let mut spool = Spool::default();
        let line = vec![b'a'; 4096];
        let lines = 2 * COMPACT_BYTES as usize / line.len();
        for _ in 0..lines {
            spool.queue(b'o', &line, SPOOL_LIMIT);
        }
        assert!(flush_all(&mut spool, &mut file).is_none());
        let full = std::fs::metadata(&path).unwrap().len();
        // Acknowledged but not yet outweighing the rest: kept.
        spool.ack(lines as u64 / 4);
        assert!(spool.next_batch().is_none());
        // Most of it acknowledged: the file is rewritten with the rest.
        let acked = lines as u64 - 3;
        spool.ack(acked);
        spool.queue(b'o', b"new", SPOOL_LIMIT);
        assert!(flush_all(&mut spool, &mut file).is_none());
        let kept = read_spool(&path, acked).unwrap();
        assert_eq!(kept.gap, None);
        let seqs: Vec<u64> = kept.records.iter().map(|(seq, _)| *seq).collect();
        assert_eq!(seqs, (acked + 1..=lines as u64 + 1).collect::<Vec<_>>());
        assert!(std::fs::metadata(&path).unwrap().len() < full / 100);
        assert_eq!(spool.file_bytes, std::fs::metadata(&path).unwrap().len());
        assert!(!spool.should_compact());
        // Appending goes on in the rewritten file.
        spool.queue(b'o', b"more", SPOOL_LIMIT);
        assert!(flush_all(&mut spool, &mut file).is_none());
        assert_eq!(
            read_spool(&path, lines as u64 + 1).unwrap().records,
            [(lines as u64 + 2, Message::Out("more".into()))]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_spool_that_cannot_be_written_fails_once_and_its_records_still_go_out() {
        let path = scratch("failed");
        SpoolFile::create(&path).unwrap();
        // Read-only: every write fails.
        let mut file = SpoolFile {
            path: path.clone(),
            file: File::open(&path).unwrap(),
        };
        let mut spool = Spool::default();
        spool.queue(b'o', b"one", SPOOL_LIMIT);
        let failed = flush_all(&mut spool, &mut file);
        assert!(failed.is_some());
        assert_eq!(spool.failed, failed);
        spool.queue(b'o', b"two", SPOOL_LIMIT);
        let batch = spool.next_batch().unwrap();
        assert!(!batch.write);
        // Reported once: the caller ends the worker once.
        assert!(spool.written(batch, Ok(())).is_none());
        assert_eq!(sent(&spool, 0), ["o 1 one", "o 2 two"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_greeting_of_another_protocol_is_refused() {
        assert_eq!(
            greeted_pid(format!("p 42 {PROTOCOL}").as_bytes()).unwrap(),
            (42, PROTOCOL)
        );
        // Slice 3's brokers are still re-attached to.
        assert_eq!(greeted_pid(b"p 42 2").unwrap(), (42, PROTOCOL_NO_INPUT_IDS));
        let kind = |greeting: &[u8]| greeted_pid(greeting).unwrap_err().kind();
        assert_eq!(
            kind(format!("p 42 {}", PROTOCOL + 1).as_bytes()),
            std::io::ErrorKind::Unsupported
        );
        assert_eq!(kind(b"p 42"), std::io::ErrorKind::Other);
        assert_eq!(kind(b"o 42 1"), std::io::ErrorKind::Other);
    }

    #[test]
    fn messages_parse_and_a_cut_line_ends_the_stream() {
        let (mut broker, server) = UnixStream::pair().unwrap();
        broker
            .write_all(
                b"o 1 {\"a\":1}\ne 2 oops\no 3 \nl 4 3\nq 5 ignored\no x bad\nn 5 r:perm%201 again\nx 6 {\"type\":\"exited\",\"code\":0}\no 7 cut",
            )
            .unwrap();
        drop(broker);
        let writer = Writer {
            stream: Arc::new(Mutex::new(server.try_clone().unwrap())),
            ids: true,
        };
        let mut messages = Messages {
            reader: BufReader::new(server),
            writer,
        };
        assert_eq!(messages.next(), Some((1, Message::Out("{\"a\":1}".into()))));
        assert_eq!(messages.next(), Some((2, Message::Err("oops".into()))));
        assert_eq!(messages.next(), Some((3, Message::Out(String::new()))));
        assert_eq!(messages.next(), Some((4, Message::Lost(3))));
        assert_eq!(
            messages.next(),
            Some((
                5,
                Message::Written {
                    id: "r:perm 1".into(),
                    again: true
                }
            ))
        );
        assert_eq!(
            messages.next(),
            Some((6, Message::Exit(json!({"type": "exited", "code": 0}))))
        );
        assert_eq!(messages.next(), None);
    }

    #[test]
    fn input_ids_round_trip_as_tokens() {
        for id in ["c:abc", "r:perm 1", "s:12", "100%\n\tü"] {
            let token = encode_id(id);
            assert!(!token.contains(|c: char| c.is_whitespace()), "{token}");
            assert_eq!(decode_id(&token).as_deref(), Some(id));
        }
        assert_eq!(decode_id("bad%2"), None);
    }

    /// A broker over a socket pair whose worker's stdin is a pipe this test
    /// reads, with records 1..=`published` published.
    fn with_stdin(published: u64) -> (Arc<Shared>, std::io::PipeReader) {
        let (reader, writer) = std::io::pipe().unwrap();
        let shared = shared_with(published, 0);
        // A `ChildStdin` from a plain pipe's write end, as the worker's.
        let pipe = ChildStdin::from(std::os::fd::OwnedFd::from(writer));
        lock(&shared.stdin).pipe = Some(pipe);
        (shared, reader)
    }

    #[test]
    fn a_stdin_line_whose_id_was_written_is_acknowledged_without_a_second_write() {
        let (shared, mut stdin) = with_stdin(1);
        write_input(&shared, b"r:perm-1 {\"answer\":1}");
        // The same id again, from a server that does not know whether the
        // first went (a re-attach after a crash), with another line: only
        // acknowledged.
        write_input(&shared, b"r:perm-1 {\"answer\":2}");
        write_input(&shared, b"c:cmd%201 {\"prompt\":1}");
        lock(&shared.stdin).pipe.take();
        let mut written = String::new();
        stdin.read_to_string(&mut written).unwrap();
        assert_eq!(written, "{\"answer\":1}\n{\"prompt\":1}\n");
        let mut spool = std::mem::take(&mut lock(&shared.state).spool);
        let batch = spool.next_batch().unwrap();
        spool.written(batch, Ok(()));
        assert_eq!(
            sent(&spool, 1),
            ["n 2 r:perm-1", "n 3 r:perm-1 again", "n 4 c:cmd%201"]
        );
        // A closed stdin writes nothing and claims nothing.
        write_input(&shared, b"s:9 {}");
        assert!(lock(&shared.state).spool.pending.is_empty());
    }

    #[test]
    fn a_writer_sends_ids_only_to_a_broker_that_takes_them() {
        for (ids, expected) in [(true, "i r:perm%201 {\"a\":1}\n"), (false, "i {\"a\":1}\n")] {
            let (mut broker, server) = UnixStream::pair().unwrap();
            let writer = Writer {
                stream: Arc::new(Mutex::new(server)),
                ids,
            };
            writer.write_input("r:perm 1", "{\"a\":1}\n").unwrap();
            drop(writer);
            let mut sent = String::new();
            broker.read_to_string(&mut sent).unwrap();
            assert_eq!(sent, expected);
        }
    }
}
