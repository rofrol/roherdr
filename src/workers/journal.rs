//! A worker's append-only JSONL journal (`<id>.jsonl`): its records,
//! written after each store commit ([`Journal::export`]), read back
//! ([`read_journal`]), replayed into a state without the store
//! ([`replay_journal`]) or imported into it ([`import_journal`]), and the
//! lock file beside it that names the server running the worker.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};
use tracing::warn;

use super::status::{Direction, Status};
use super::{lock, store, transcript, WorkerError};

/// One worker's append-only JSONL journal: an export of its events for
/// `herdr worker log` and debugging, written after each event's store
/// commit. The store, not this file, is the worker's record.
pub(super) struct Journal {
    file: Mutex<File>,
}

impl Journal {
    pub(super) fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Appends one record with the `seq` the store gave it, if any.
    pub(super) fn export(
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
        lock(&self.file).write_all(line.as_bytes())?;
        transcript::note_appended();
        Ok(())
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
pub(super) fn note_journal_gap(store: &store::Store, status: &mut Status, path: &Path) {
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
pub(super) fn replay_journal(worker_id: &str, path: &Path) -> std::io::Result<Status> {
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
pub(super) fn import_journal(
    store: &store::Store,
    worker_id: &str,
    path: &Path,
) -> Result<Status, String> {
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
pub(super) fn own_journal(journal_path: &Path, worker_id: &str) -> Result<File, WorkerError> {
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
pub(super) enum Ownership {
    /// No server runs the worker; the lock, if the journal has one, is
    /// held while the journal is replayed.
    Unowned { _held: Option<File> },
    /// Another server holds the lock: its worker may still run.
    Owned(File),
}

pub(super) fn journal_ownership(journal_path: &Path) -> std::io::Result<Ownership> {
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
