//! The workers' durable state: one SQLite database per session's worker
//! directory (`workers.sqlite3`, WAL). Every event a supervisor records is
//! appended to `events`, which numbers it (`seq`, never reused), and the
//! projections it changes (`workers`, `questions`) are written in the same
//! transaction, so the database never holds an event without its effect or
//! an effect without its event. A supervisor rebuilds its workers from the
//! projections when it opens. Two servers (the old and the new one of a live
//! handoff) may write the same database; each worker's rows are written only
//! by the server that runs it.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{named_params, params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use super::{lock, worker_number, Direction, Pending, Status, RESOLVED_QUESTIONS_KEPT};
use crate::api::schema::{WorkerState, WorkerTurnResult};

/// The database file, inside the session's worker directory.
pub(super) const STORE_FILE: &str = "workers.sqlite3";

/// How long a write waits for another server's write transaction on the
/// same database to end. This is only SQLite's lock wait (`busy_timeout`),
/// not a decision: a write that still finds the database locked fails, and
/// the worker is marked degraded with that error.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Schema migrations, in order; the database's `meta.schema_version` counts
/// how many have run. Never edit one that has shipped: append a new one.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    worker_id TEXT NOT NULL,
    direction TEXT NOT NULL,
    -- The event's `type`; NULL for a line that was not JSON, whose `body` is
    -- then that line as a JSON string.
    type TEXT,
    body TEXT NOT NULL,
    ts_ms INTEGER NOT NULL
);
CREATE INDEX events_by_worker ON events (worker_id, seq);
CREATE TABLE workers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    cwd TEXT NOT NULL,
    workspace_id TEXT,
    model TEXT,
    slot TEXT,
    state TEXT NOT NULL,
    pid INTEGER,
    session_id TEXT,
    turns INTEGER NOT NULL,
    last_result TEXT,
    rate_limit TEXT,
    tool_sessions TEXT NOT NULL,
    exit_code INTEGER,
    exit_signal INTEGER,
    stop_requested_ms INTEGER,
    takeover_ms INTEGER,
    takeover_tab TEXT,
    takeover_error TEXT,
    takeover_unfinished INTEGER NOT NULL,
    refusal TEXT,
    exited INTEGER NOT NULL,
    lost INTEGER NOT NULL,
    end_note TEXT,
    last_seq INTEGER NOT NULL
);
CREATE TABLE questions (
    worker_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    text TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'answered', 'cancelled', 'expired')),
    -- What ended it, as `worker_question_gone` says it.
    how TEXT,
    question TEXT NOT NULL,
    input TEXT NOT NULL,
    asked_seq INTEGER NOT NULL,
    settled_seq INTEGER,
    PRIMARY KEY (worker_id, request_id)
);
"#,
    r#"
-- What `worker.wait` with `until: attention` compares with `after`: the
-- user message that began the last turn, the event that ended a turn, the
-- one that made the worker gone. Filled from the events for workers
-- recorded before.
ALTER TABLE workers ADD COLUMN turn_seq INTEGER;
ALTER TABLE workers ADD COLUMN turn_end_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workers ADD COLUMN gone_seq INTEGER NOT NULL DEFAULT 0;
UPDATE workers SET
    turn_seq = (SELECT max(seq) FROM events
        WHERE worker_id = workers.id AND direction = 'in' AND type = 'user'),
    turn_end_seq = coalesce((SELECT max(seq) FROM events
        WHERE worker_id = workers.id
          AND ((direction = 'out' AND type = 'result')
            OR (direction = 'herdr' AND type IN ('exited', 'lost')))), 0),
    gone_seq = coalesce((SELECT max(seq) FROM events
        WHERE worker_id = workers.id
          AND direction = 'herdr' AND type IN ('exited', 'lost')), 0);
"#,
];

pub(super) type StoreResult<T> = rusqlite::Result<T>;

pub(super) struct Store {
    conn: Mutex<Connection>,
}

/// One event as it is appended.
pub(super) struct EventRow<'a> {
    pub(super) worker_id: &'a str,
    pub(super) direction: Direction,
    pub(super) record: &'a Recorded<'a>,
    pub(super) ts_ms: u64,
}

/// What a journal line carries: a JSON event, or a line that was not JSON.
#[derive(Debug, Clone, Copy)]
pub(super) enum Recorded<'a> {
    Event(&'a Value),
    Raw(&'a str),
}

impl Store {
    pub(super) fn open(path: &Path) -> StoreResult<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(LOCK_WAIT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // In WAL mode a server crash loses nothing committed; an OS crash
        // may lose the last commits, never tear one.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Runs `write` in one write transaction: everything it wrote is
    /// committed, or nothing when it (or the commit) fails.
    pub(super) fn transaction<T>(
        &self,
        write: impl FnOnce(&Tx<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = lock(&self.conn);
        // Immediate: the write lock is taken at the start, so the lock wait
        // applies; a deferred transaction upgraded later fails at once.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = write(&Tx { tx: &tx })?;
        tx.commit()?;
        Ok(value)
    }

    /// Every worker the projections hold, by number.
    pub(super) fn load_all(&self) -> StoreResult<BTreeMap<u64, Status>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&format!("SELECT {WORKER_COLUMNS} FROM workers"))?;
        let mut workers = BTreeMap::new();
        for status in statement.query_map([], status_from_row)? {
            let mut status = status?;
            let Some(number) = worker_number(&status.worker_id) else {
                continue;
            };
            load_questions(&conn, &mut status)?;
            workers.insert(number, status);
        }
        Ok(workers)
    }

    /// One worker from the projections, if they hold it.
    pub(super) fn load(&self, worker_id: &str) -> StoreResult<Option<Status>> {
        let conn = lock(&self.conn);
        let status = conn
            .query_row(
                &format!("SELECT {WORKER_COLUMNS} FROM workers WHERE id = ?1"),
                [worker_id],
                status_from_row,
            )
            .optional()?;
        match status {
            Some(mut status) => {
                load_questions(&conn, &mut status)?;
                Ok(Some(status))
            }
            None => Ok(None),
        }
    }

    #[cfg(test)]
    pub(super) fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        lock(&self.conn)
    }
}

fn migrate(conn: &mut Connection) -> StoreResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    let version: usize = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse().unwrap_or(usize::MAX))
        .unwrap_or(0);
    if version > MIGRATIONS.len() {
        // Written by a newer herdr: refused rather than misread; the
        // supervisor then runs from the JSONL journals, degraded.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "worker store schema version {version} is newer than this herdr's {}",
                MIGRATIONS.len()
            )),
        ));
    }
    for migration in &MIGRATIONS[version..] {
        tx.execute_batch(migration)?;
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [MIGRATIONS.len().to_string()],
    )?;
    tx.commit()
}

/// A write transaction ([`Store::transaction`]).
pub(super) struct Tx<'a> {
    tx: &'a rusqlite::Transaction<'a>,
}

impl Tx<'_> {
    /// Appends one event and returns its `seq`.
    pub(super) fn event(&self, event: &EventRow<'_>) -> StoreResult<i64> {
        let (kind, body) = match event.record {
            Recorded::Event(value) => {
                (value.get("type").and_then(Value::as_str), value.to_string())
            }
            Recorded::Raw(line) => (None, Value::String((*line).to_owned()).to_string()),
        };
        self.tx.execute(
            "INSERT INTO events (worker_id, direction, type, body, ts_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.worker_id,
                event.direction.as_str(),
                kind,
                body,
                event.ts_ms as i64
            ],
        )?;
        Ok(self.tx.last_insert_rowid())
    }

    /// Writes the questions an event asked or settled: `before` are the ids
    /// pending before it, `status` the state after it.
    pub(super) fn questions(
        &self,
        seq: i64,
        before: &[String],
        status: &Status,
    ) -> StoreResult<()> {
        for pending in &status.questions {
            if before.contains(&pending.question.request_id) {
                continue;
            }
            let question = &pending.question;
            self.tx.execute(
                "INSERT INTO questions (worker_id, request_id, kind, tool_name, text, state,
                     how, question, input, asked_seq, settled_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, ?6, ?7, ?8, NULL)
                 ON CONFLICT (worker_id, request_id) DO UPDATE SET
                     kind = excluded.kind, tool_name = excluded.tool_name,
                     text = excluded.text, state = 'pending', how = NULL,
                     question = excluded.question, input = excluded.input,
                     asked_seq = excluded.asked_seq, settled_seq = NULL",
                params![
                    status.worker_id,
                    question.request_id,
                    enum_text(&question.kind),
                    question.tool_name,
                    question.text,
                    serde_json::to_string(question).unwrap_or_else(|_| "{}".into()),
                    pending.input.to_string(),
                    seq,
                ],
            )?;
        }
        for request_id in before {
            if status
                .questions
                .iter()
                .any(|pending| pending.question.request_id == *request_id)
            {
                continue;
            }
            let how = status.resolution(request_id).unwrap_or("cancelled");
            self.tx.execute(
                "UPDATE questions SET state = ?3, how = ?4, settled_seq = ?5
                 WHERE worker_id = ?1 AND request_id = ?2",
                params![status.worker_id, request_id, question_state(how), how, seq],
            )?;
        }
        Ok(())
    }

    /// Writes the worker's projection row as of event `seq`.
    pub(super) fn worker(&self, status: &Status, seq: i64) -> StoreResult<()> {
        let tool_sessions: Vec<(u32, Option<u64>)> = status
            .tool_sessions
            .iter()
            .map(|(session, start)| (*session, *start))
            .collect();
        self.tx.execute(
            &format!(
                "INSERT INTO workers ({WORKER_COLUMNS}) VALUES (
                    :id, :name, :cwd, :workspace_id, :model, :slot, :state, :pid,
                    :session_id, :turns, :last_result, :rate_limit, :tool_sessions,
                    :exit_code, :exit_signal, :stop_requested_ms, :takeover_ms,
                    :takeover_tab, :takeover_error, :takeover_unfinished, :refusal,
                    :exited, :lost, :end_note, :last_seq, :turn_seq, :turn_end_seq,
                    :gone_seq)
                 ON CONFLICT (id) DO UPDATE SET {}",
                WORKER_COLUMNS
                    .split(", ")
                    .filter(|column| *column != "id")
                    .map(|column| format!("{column} = excluded.{column}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            named_params! {
                ":id": status.worker_id,
                ":name": status.name,
                ":cwd": status.cwd,
                ":workspace_id": status.workspace_id,
                ":model": status.model,
                ":slot": status.slot,
                ":state": enum_text(&status.state),
                ":pid": status.pid,
                ":session_id": status.session_id,
                ":turns": status.turns,
                ":last_result": status
                    .last_result
                    .as_ref()
                    .and_then(|result| serde_json::to_string(result).ok()),
                ":rate_limit": status.rate_limit.as_ref().map(Value::to_string),
                ":tool_sessions": serde_json::to_string(&tool_sessions)
                    .unwrap_or_else(|_| "[]".into()),
                ":exit_code": status.exit_code,
                ":exit_signal": status.exit_signal,
                ":stop_requested_ms": status.stop_requested_ms.map(|ms| ms as i64),
                ":takeover_ms": status.takeover_ms.map(|ms| ms as i64),
                ":takeover_tab": status.takeover_tab,
                ":takeover_error": status.takeover_error,
                ":takeover_unfinished": status.takeover_unfinished,
                ":refusal": status.refusal,
                ":exited": status.exited,
                ":lost": status.lost,
                ":end_note": status.end_note,
                ":last_seq": seq,
                ":turn_seq": status.turn_seq,
                ":turn_end_seq": status.turn_end_seq,
                ":gone_seq": status.gone_seq,
            },
        )?;
        Ok(())
    }
}

const WORKER_COLUMNS: &str = "id, name, cwd, workspace_id, model, slot, state, pid, \
session_id, turns, last_result, rate_limit, tool_sessions, exit_code, exit_signal, \
stop_requested_ms, takeover_ms, takeover_tab, takeover_error, takeover_unfinished, refusal, \
exited, lost, end_note, last_seq, turn_seq, turn_end_seq, gone_seq";

fn status_from_row(row: &rusqlite::Row<'_>) -> StoreResult<Status> {
    let json = |index: usize| -> StoreResult<Option<Value>> {
        Ok(row
            .get::<_, Option<String>>(index)?
            .and_then(|text| serde_json::from_str(&text).ok()))
    };
    let mut status = Status::new(row.get(0)?);
    status.name = row.get(1)?;
    status.cwd = row.get(2)?;
    status.workspace_id = row.get(3)?;
    status.model = row.get(4)?;
    status.slot = row.get(5)?;
    status.state =
        serde_json::from_value(Value::String(row.get(6)?)).unwrap_or(WorkerState::Unknown);
    status.pid = row.get(7)?;
    status.session_id = row.get(8)?;
    status.turns = row.get(9)?;
    status.last_result =
        json(10)?.and_then(|value| serde_json::from_value::<WorkerTurnResult>(value).ok());
    status.rate_limit = json(11)?;
    status.tool_sessions = json(12)?
        .and_then(|value| serde_json::from_value::<Vec<(u32, Option<u64>)>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .collect();
    status.exit_code = row.get(13)?;
    status.exit_signal = row.get(14)?;
    status.stop_requested_ms = row.get::<_, Option<i64>>(15)?.map(|ms| ms as u64);
    status.takeover_ms = row.get::<_, Option<i64>>(16)?.map(|ms| ms as u64);
    status.takeover_tab = row.get(17)?;
    status.takeover_error = row.get(18)?;
    status.takeover_unfinished = row.get(19)?;
    status.refusal = row.get(20)?;
    status.exited = row.get(21)?;
    status.lost = row.get(22)?;
    status.end_note = row.get(23)?;
    status.last_seq = row.get(24)?;
    status.turn_seq = row.get(25)?;
    status.turn_end_seq = row.get(26)?;
    status.gone_seq = row.get(27)?;
    Ok(status)
}

/// Fills a status's pending questions, oldest first, and the most recently
/// settled ones.
fn load_questions(conn: &Connection, status: &mut Status) -> StoreResult<()> {
    let mut pending = conn.prepare(
        "SELECT question, input, asked_seq FROM questions
         WHERE worker_id = ?1 AND state = 'pending' ORDER BY asked_seq, rowid",
    )?;
    let rows = pending.query_map([&status.worker_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (question, input, asked_seq) = row?;
        let Ok(question) = serde_json::from_str(&question) else {
            continue;
        };
        status.questions.push(Pending {
            question,
            input: serde_json::from_str(&input)
                .unwrap_or_else(|_| Value::Object(Default::default())),
            asked_seq,
        });
    }
    let mut settled = conn.prepare(
        "SELECT request_id, how FROM questions
         WHERE worker_id = ?1 AND state != 'pending'
         ORDER BY settled_seq DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = settled.query_map(
        params![status.worker_id, RESOLVED_QUESTIONS_KEPT as i64],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let mut resolved = VecDeque::new();
    for row in rows {
        let (request_id, how) = row?;
        resolved.push_front((request_id, how.unwrap_or_else(|| "cancelled".into())));
    }
    status.resolved = resolved;
    Ok(())
}

/// A serde enum's wire name (`waiting_approval`), as the API spells it.
fn enum_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The `questions.state` of a question that ended `how`.
fn question_state(how: &str) -> &'static str {
    match how {
        "answered" => "answered",
        "cancelled" => "cancelled",
        _ => "expired",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::workers::{import_journal, now_ms, question_from_request, replay_journal};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-worker-store-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn append(store: &Store, worker_id: &str, event: &Value) -> StoreResult<i64> {
        let mut status = Status::new(worker_id.to_owned());
        status.apply(Direction::Herdr, event);
        store.transaction(|tx| {
            let seq = tx.event(&EventRow {
                worker_id,
                direction: Direction::Herdr,
                record: &Recorded::Event(event),
                ts_ms: 1,
            })?;
            tx.questions(seq, &[], &status)?;
            tx.worker(&status, seq)?;
            Ok(seq)
        })
    }

    #[test]
    fn a_fresh_store_has_the_schema_and_no_workers() {
        let dir = scratch("fresh");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        assert!(store.load_all().unwrap().is_empty());
        let version: String = store
            .connection()
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, MIGRATIONS.len().to_string());
        let mode: String = store
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(store);
        // Opening again runs no migration twice.
        let store = Store::open(&path).unwrap();
        assert_eq!(count(&store, "events"), 0);

        // A database from a newer herdr is refused, not misread.
        store
            .connection()
            .execute(
                "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(store);
        let refused = Store::open(&path).err().unwrap().to_string();
        assert!(refused.contains("newer"), "{refused}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_event_and_its_projection_commit_together_or_not_at_all() {
        let dir = scratch("atomic");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let started = json!({"type": "started", "cwd": "/repo", "name": "one"});
        let seq = append(&store, "w1", &started).unwrap();
        assert_eq!((count(&store, "events"), count(&store, "workers")), (1, 1));
        assert_eq!(store.load("w1").unwrap().unwrap().last_seq, seq);

        // The projection write fails after the event was inserted: neither
        // stays.
        store
            .connection()
            .execute_batch(
                "CREATE TRIGGER forced BEFORE UPDATE ON workers
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();
        let error = append(
            &store,
            "w1",
            &json!({"type": "signal", "signal": "SIGTERM"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("forced failure"), "{error}");
        assert_eq!((count(&store, "events"), count(&store, "workers")), (1, 1));
        assert_eq!(store.load("w1").unwrap().unwrap().last_seq, seq);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_attention_columns_are_filled_from_the_events_of_older_rows() {
        let dir = scratch("attention-columns");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let events = [
            (Direction::Herdr, json!({"type": "started", "cwd": "/repo"})),
            (
                Direction::In,
                json!({"type": "user", "message": {"content": "go"}}),
            ),
            (
                Direction::Out,
                json!({"type": "result", "subtype": "success", "is_error": false}),
            ),
            (Direction::Herdr, json!({"type": "exited", "code": 0})),
        ];
        let mut status = Status::new("w1".to_owned());
        let mut seqs = Vec::new();
        for (direction, event) in &events {
            status.apply(*direction, event);
            seqs.push(
                store
                    .transaction(|tx| {
                        let seq = tx.event(&EventRow {
                            worker_id: "w1",
                            direction: *direction,
                            record: &Recorded::Event(event),
                            ts_ms: 1,
                        })?;
                        tx.worker(&status, seq)?;
                        Ok(seq)
                    })
                    .unwrap(),
            );
        }
        // A database from before the columns: written as the first
        // migration left it.
        store
            .connection()
            .execute_batch(
                "ALTER TABLE workers DROP COLUMN turn_seq;
                 ALTER TABLE workers DROP COLUMN turn_end_seq;
                 ALTER TABLE workers DROP COLUMN gone_seq;
                 UPDATE meta SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(store);
        let loaded = Store::open(&path).unwrap().load("w1").unwrap().unwrap();
        assert_eq!(
            (loaded.turn_seq, loaded.turn_end_seq, loaded.gone_seq),
            (Some(seqs[1]), seqs[3], seqs[3])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seq_is_never_reused() {
        let dir = scratch("seq");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let event = json!({"type": "started", "cwd": "/repo"});
        let first = append(&store, "w1", &event).unwrap();
        let second = append(&store, "w2", &event).unwrap();
        assert!(second > first);
        store
            .connection()
            .execute("DELETE FROM events WHERE seq = ?1", [second])
            .unwrap();
        assert!(append(&store, "w1", &event).unwrap() > second);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_servers_append_concurrently_without_losing_or_reusing_a_seq() {
        const PER_THREAD: usize = 200;
        let dir = scratch("concurrent");
        let path = dir.join(STORE_FILE);
        Store::open(&path).unwrap();
        // One connection per thread, as two servers of a handoff have.
        let threads: Vec<_> = ["w1", "w2"]
            .into_iter()
            .map(|worker_id| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    let event = json!({"type": "tool_sessions", "sessions": []});
                    (0..PER_THREAD)
                        .map(|_| append(&store, worker_id, &event).unwrap())
                        .collect::<Vec<i64>>()
                })
            })
            .collect();
        let mut all = Vec::new();
        for thread in threads {
            let seqs = thread.join().unwrap();
            assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
            all.extend(seqs);
        }
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 2 * PER_THREAD);
        let store = Store::open(&path).unwrap();
        assert_eq!(count(&store, "events"), (2 * PER_THREAD) as i64);
        assert_eq!(count(&store, "workers"), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_projections_rebuild_the_folded_state() {
        let dir = scratch("rebuild");
        let journal = dir.join("w1.jsonl");
        let question = |id: &str| {
            json!({"type": "question", "input": {"command": "ls"},
                "question": question_from_request(
                    id, &json!({"tool_name": "Bash", "input": {"command": "ls"}}), "asked")})
        };
        let lines = [
            (
                "herdr",
                json!({"type": "started", "cwd": "/repo", "name": "task",
                "workspace_id": "ws_1", "model": "opus", "pid": 4242,
                "folder_slot": {"name": "worker", "branch": "b"}}),
            ),
            (
                "out",
                json!({"type": "system", "subtype": "init", "session_id": "s-1"}),
            ),
            (
                "out",
                json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}),
            ),
            (
                "herdr",
                json!({"type": "tool_sessions",
                "sessions": [{"session": 7, "leader_start": 99}, 8]}),
            ),
            (
                "out",
                json!({"type": "result", "subtype": "success", "is_error": false,
                "terminal_reason": "completed", "result": "first"}),
            ),
            (
                "in",
                json!({"type": "user", "message": {"content": "next"}}),
            ),
            (
                "out",
                json!({"type": "control_request", "request_id": "q1",
                "request": {"subtype": "can_use_tool"}}),
            ),
            ("herdr", question("q1")),
            ("herdr", question("q2")),
            ("herdr", question("q3")),
            ("herdr", json!({"type": "answer", "request_id": "q2"})),
            (
                "out",
                json!({"type": "control_cancel_request", "request_id": "q3"}),
            ),
            ("herdr", json!({"type": "takeover", "at_ms": 5})),
            (
                "herdr",
                json!({"type": "signal", "signal": "SIGTERM", "at_ms": 6}),
            ),
        ];
        let text: String = lines
            .iter()
            .map(|(dir, event)| format!("{}\n", json!({"ts_ms": 1, "dir": dir, "event": event})))
            .collect();
        std::fs::write(&journal, text + "{torn\n").unwrap();

        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let imported = import_journal(&store, "w1", &journal).unwrap();
        assert_eq!(imported.questions.len(), 1);
        assert_eq!(imported.resolution("q2"), Some("answered"));
        assert_eq!(imported.resolution("q3"), Some("cancelled"));
        assert_eq!(count(&store, "events"), lines.len() as i64);

        let mut loaded = store.load("w1").unwrap().unwrap();
        loaded.mark_unfinished_takeover();
        assert_eq!(format!("{loaded:?}"), format!("{imported:?}"));
        // The import folds as the in-memory replay does.
        let mut replayed = replay_journal("w1", &journal).unwrap();
        replayed.last_seq = imported.last_seq;
        assert_eq!(format!("{replayed:?}"), format!("{imported:?}"));
        let states: Vec<(String, String)> = store
            .connection()
            .prepare("SELECT request_id, state FROM questions ORDER BY request_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<StoreResult<_>>()
            .unwrap();
        assert_eq!(
            states,
            [("q1", "pending"), ("q2", "answered"), ("q3", "cancelled")]
                .map(|(id, state)| (id.to_owned(), state.to_owned()))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
