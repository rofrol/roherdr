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
    r#"
-- `answering`: an answer's intent is stored and its control_response not
-- yet confirmed written (`answer_intent` .. `answer_sent`). SQLite cannot
-- change a CHECK in place, so the table is rebuilt.
CREATE TABLE questions_v3 (
    worker_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    text TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK (state IN ('pending', 'answering', 'answered', 'cancelled', 'expired')),
    how TEXT,
    question TEXT NOT NULL,
    input TEXT NOT NULL,
    asked_seq INTEGER NOT NULL,
    settled_seq INTEGER,
    PRIMARY KEY (worker_id, request_id)
);
INSERT INTO questions_v3 SELECT worker_id, request_id, kind, tool_name, text, state, how,
    question, input, asked_seq, settled_seq FROM questions ORDER BY rowid;
DROP TABLE questions;
ALTER TABLE questions_v3 RENAME TO questions;
-- One row per client command id: reserved with the command's first event,
-- settled with its outcome, which a repeated id gets back.
CREATE TABLE receipts (
    command_id TEXT PRIMARY KEY,
    method TEXT NOT NULL,
    -- The command's parameters without the id, as JSON: a reused id with
    -- other parameters is refused.
    params TEXT NOT NULL,
    worker_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'rejected')),
    -- The reply (accepted) or the error's code and message (rejected).
    result TEXT,
    created_ms INTEGER NOT NULL,
    settled_ms INTEGER
);
"#,
    r#"
-- The worker's owner (the pane and agent session that started it) and the
-- highest `seq` that owner acknowledged (`acked`). Workers recorded before
-- have no owner, so no obligations.
ALTER TABLE workers ADD COLUMN owner_pane TEXT;
ALTER TABLE workers ADD COLUMN owner_session TEXT;
ALTER TABLE workers ADD COLUMN acked_seq INTEGER NOT NULL DEFAULT 0;
"#,
    r#"
-- Why a question of an owned worker went to the user (`escalated`,
-- `owner_gone`); NULL while it waits quietly for its owner. And why the
-- owner is gone for good, which makes its later questions go to the user.
ALTER TABLE questions ADD COLUMN escalated TEXT;
ALTER TABLE workers ADD COLUMN owner_gone TEXT;
"#,
    r#"
-- Why the worker's record is incomplete (a store or journal write failed),
-- stored with the first write that succeeds after the failure.
ALTER TABLE workers ADD COLUMN degraded TEXT;
"#,
    r#"
-- The TODO item a worker works on and its repository, so `worker.runs`
-- groups its run under that item; workers recorded before have none and are
-- listed unassigned. The run's start and end times and the questions it
-- asked and whether it ended in a turn are filled from the events of older
-- rows; the commits its turns named stay empty for them.
ALTER TABLE workers ADD COLUMN item TEXT;
ALTER TABLE workers ADD COLUMN repo TEXT;
ALTER TABLE workers ADD COLUMN started_ms INTEGER;
ALTER TABLE workers ADD COLUMN ended_ms INTEGER;
ALTER TABLE workers ADD COLUMN done_commits TEXT NOT NULL DEFAULT '[]';
ALTER TABLE workers ADD COLUMN questions_asked INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workers ADD COLUMN ended_mid_turn INTEGER NOT NULL DEFAULT 0;
UPDATE workers SET
    started_ms = (SELECT ts_ms FROM events
        WHERE worker_id = workers.id AND direction = 'herdr' AND type = 'started'
        ORDER BY seq LIMIT 1),
    ended_ms = (SELECT ts_ms FROM events WHERE seq = workers.gone_seq),
    questions_asked = (SELECT count(*) FROM questions WHERE worker_id = workers.id),
    ended_mid_turn = gone_seq > 0 AND coalesce(turn_seq, 0) > coalesce((SELECT max(seq)
        FROM events WHERE worker_id = workers.id AND direction = 'out' AND type = 'result'), 0);
"#,
    r#"
-- The item's title in TODO.md when the worker started (`started`'s
-- `item_title`), since a finished item leaves the file. Filled from the
-- `started` event where it has one; older workers have none, and
-- `worker.runs` reads TODO.md for them.
ALTER TABLE workers ADD COLUMN item_title TEXT;
UPDATE workers SET item_title = (SELECT json_extract(body, '$.item_title') FROM events
    WHERE worker_id = workers.id AND direction = 'herdr' AND type = 'started'
    ORDER BY seq LIMIT 1);
"#,
    r#"
-- The latest `worker.verify` verdict with its evidence (`verification`'s
-- `verification`), so `worker.runs` shows it. Older workers have none.
ALTER TABLE workers ADD COLUMN verification TEXT;
"#,
    r#"
-- The takeover claim's id (`takeover`'s `takeover_id`), which its tab
-- carries, so a restart can find the tab. Filled from the last `takeover`
-- event of a claim still held; older claims have none.
ALTER TABLE workers ADD COLUMN takeover_id TEXT;
UPDATE workers SET takeover_id = (SELECT json_extract(body, '$.takeover_id') FROM events
    WHERE worker_id = workers.id AND direction = 'herdr' AND type = 'takeover'
    ORDER BY seq DESC LIMIT 1)
    WHERE takeover_ms IS NOT NULL;
"#,
    r#"
-- Coordination tenures: who coordinates a repository's TODO, from when to
-- when, projected from the `coordinator_started` and `coordinator_ended`
-- events (whose `worker_id` is the tenure's id) in the same transaction.
-- Times are Unix milliseconds. `epoch` grows by one per tenure of the repo.
CREATE TABLE coordinators (
    id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    item TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    end_reason TEXT,
    epoch INTEGER NOT NULL
);
-- One active coordinator per repository: this index is the claim.
CREATE UNIQUE INDEX coordinators_one_active_per_repo ON coordinators (repo)
    WHERE ended_at IS NULL;
-- The panes and agent sessions a tenure ran in; `to_at` is NULL while bound.
CREATE TABLE coordinator_bindings (
    coordinator_id TEXT NOT NULL REFERENCES coordinators (id),
    pane_id TEXT NOT NULL,
    session_id TEXT,
    from_at INTEGER NOT NULL,
    to_at INTEGER
);
CREATE INDEX coordinator_bindings_by_pane ON coordinator_bindings (pane_id)
    WHERE to_at IS NULL;
-- The tenure of the coordinator whose pane started the worker
-- (`started`'s `owner.coordinator_id`); older workers have none.
ALTER TABLE workers ADD COLUMN owner_coordinator_id TEXT;
"#,
    r#"
-- The broker that owns the worker's pipes (`started`'s `broker`: its pid,
-- socket and the worker's directory as asked for), as JSON, so a server
-- that starts while the worker runs re-attaches to it. Workers started
-- before the broker, or on a platform without one, have none.
ALTER TABLE workers ADD COLUMN broker TEXT;
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

/// A client command's receipt as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredReceipt {
    pub(super) method: String,
    pub(super) params: String,
    pub(super) worker_id: Option<String>,
    pub(super) state: ReceiptState,
    pub(super) result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReceiptState {
    /// Reserved, outcome not stored: the command runs, or its server ended
    /// before it finished.
    Pending,
    Accepted,
    Rejected,
}

impl ReceiptState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            _ => Self::Pending,
        }
    }
}

/// A client command id with what it asked, as reserved and settled.
#[derive(Debug, Clone, Copy)]
pub(super) struct NewReceipt<'a> {
    pub(super) command_id: &'a str,
    pub(super) method: &'a str,
    pub(super) params: &'a str,
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

    /// The receipt of a client command id, if one was stored.
    pub(super) fn receipt(&self, command_id: &str) -> StoreResult<Option<StoredReceipt>> {
        let conn = lock(&self.conn);
        conn.query_row(
            "SELECT method, params, worker_id, state, result FROM receipts WHERE command_id = ?1",
            [command_id],
            |row| {
                Ok(StoredReceipt {
                    method: row.get(0)?,
                    params: row.get(1)?,
                    worker_id: row.get(2)?,
                    state: ReceiptState::parse(&row.get::<_, String>(3)?),
                    result: row.get(4)?,
                })
            },
        )
        .optional()
    }

    /// How many events the store holds of one worker.
    pub(super) fn event_count(&self, worker_id: &str) -> StoreResult<i64> {
        let conn = lock(&self.conn);
        conn.query_row(
            "SELECT count(*) FROM events WHERE worker_id = ?1",
            [worker_id],
            |row| row.get(0),
        )
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

    /// Writes the questions an event asked, began answering or settled:
    /// `before` are the ids open before it, each with whether its answer was
    /// in flight, `status` the state after it.
    ///
    /// Every change is guarded by the state the row had (T3 Code's guard):
    /// a cleanup that cancels open questions (a turn's end, the exit)
    /// settles only rows still `pending`, so it never overwrites an answer
    /// that is `answering` or `answered`, whatever this server's memory
    /// holds; only the answer's own outcome settles an `answering` row.
    pub(super) fn questions(
        &self,
        seq: i64,
        before: &[(String, bool)],
        status: &Status,
    ) -> StoreResult<()> {
        let state_of = |answering: bool| if answering { "answering" } else { "pending" };
        for pending in &status.questions {
            let question = &pending.question;
            let known = before.iter().find(|(id, _)| *id == question.request_id);
            if let (Some(_), Some(escalated)) = (known, &pending.escalated) {
                self.tx.execute(
                    "UPDATE questions SET escalated = ?3
                     WHERE worker_id = ?1 AND request_id = ?2 AND escalated IS NULL",
                    params![status.worker_id, question.request_id, escalated],
                )?;
            }
            match known {
                Some((_, was)) if *was == pending.answering => {}
                Some((_, was)) => {
                    self.tx.execute(
                        "UPDATE questions SET state = ?3
                         WHERE worker_id = ?1 AND request_id = ?2 AND state = ?4",
                        params![
                            status.worker_id,
                            question.request_id,
                            state_of(pending.answering),
                            state_of(*was),
                        ],
                    )?;
                }
                None => {
                    self.tx.execute(
                        "INSERT INTO questions (worker_id, request_id, kind, tool_name, text,
                             state, how, question, input, asked_seq, settled_seq, escalated)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?9, NULL, ?6, ?7, ?8, NULL, ?10)
                         ON CONFLICT (worker_id, request_id) DO UPDATE SET
                             kind = excluded.kind, tool_name = excluded.tool_name,
                             text = excluded.text, state = excluded.state, how = NULL,
                             question = excluded.question, input = excluded.input,
                             asked_seq = excluded.asked_seq, settled_seq = NULL,
                             escalated = excluded.escalated",
                        params![
                            status.worker_id,
                            question.request_id,
                            enum_text(&question.kind),
                            question.tool_name,
                            question.text,
                            serde_json::to_string(question).unwrap_or_else(|_| "{}".into()),
                            pending.input.to_string(),
                            seq,
                            state_of(pending.answering),
                            pending.escalated,
                        ],
                    )?;
                }
            }
        }
        for (request_id, was_answering) in before {
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
                 WHERE worker_id = ?1 AND request_id = ?2 AND state = ?6",
                params![
                    status.worker_id,
                    request_id,
                    question_state(how),
                    how,
                    seq,
                    state_of(*was_answering),
                ],
            )?;
        }
        Ok(())
    }

    /// Reserves a client command's id with its first event: inserted only
    /// if absent. The caller has checked under the registry lock that the
    /// id is new; a row another server wrote meanwhile stays as it is.
    pub(super) fn reserve_receipt(
        &self,
        receipt: &NewReceipt<'_>,
        worker_id: &str,
        ts_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO receipts (command_id, method, params, worker_id, state, created_ms)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)
             ON CONFLICT (command_id) DO NOTHING",
            params![
                receipt.command_id,
                receipt.method,
                receipt.params,
                worker_id,
                ts_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Stores a command's outcome, reserving its id first when no event of
    /// the command did.
    pub(super) fn settle_receipt(
        &self,
        receipt: &NewReceipt<'_>,
        worker_id: Option<&str>,
        state: ReceiptState,
        result: &str,
        ts_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO receipts (command_id, method, params, worker_id, state, result,
                 created_ms, settled_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT (command_id) DO UPDATE SET
                 worker_id = coalesce(receipts.worker_id, excluded.worker_id),
                 state = excluded.state, result = excluded.result,
                 settled_ms = excluded.settled_ms
             WHERE receipts.state = 'pending'",
            params![
                receipt.command_id,
                receipt.method,
                receipt.params,
                worker_id,
                state.as_str(),
                result,
                ts_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Records `coordinator_started` and its projection: the tenure's row,
    /// with the repository's next epoch, and its first binding. The unique
    /// index refuses a second active tenure of the repository, also one
    /// another server wrote; the caller checks first with
    /// [`Self::active_coordinator`] to refuse it by name.
    pub(super) fn coordinator_started(
        &self,
        tenure: &NewTenure<'_>,
        at_ms: u64,
    ) -> StoreResult<StoredTenure> {
        let epoch: i64 = self.tx.query_row(
            "SELECT coalesce(max(epoch), 0) + 1 FROM coordinators WHERE repo = ?1",
            [tenure.repo],
            |row| row.get(0),
        )?;
        let event = serde_json::json!({
            "type": "coordinator_started",
            "coordinator_id": tenure.id,
            "repo": tenure.repo,
            "pane_id": tenure.pane_id,
            "session_id": tenure.session_id,
            "epoch": epoch,
        });
        self.coordinator_event(tenure.id, &event, at_ms)?;
        self.tx.execute(
            "INSERT INTO coordinators (id, repo, item, started_at, ended_at, end_reason, epoch)
             VALUES (?1, ?2, NULL, ?3, NULL, NULL, ?4)",
            params![tenure.id, tenure.repo, at_ms as i64, epoch],
        )?;
        self.tx.execute(
            "INSERT INTO coordinator_bindings (coordinator_id, pane_id, session_id, from_at, to_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![tenure.id, tenure.pane_id, tenure.session_id, at_ms as i64],
        )?;
        tenure_by_id(self.tx, tenure.id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Records `coordinator_ended` with its reason (and `cause`, what herdr
    /// saw) and ends the tenure and its binding; nothing when it has ended
    /// already. Returns the tenure as it is now.
    pub(super) fn coordinator_ended(
        &self,
        id: &str,
        reason: &str,
        cause: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<Option<StoredTenure>> {
        let Some(tenure) = tenure_by_id(self.tx, id)? else {
            return Ok(None);
        };
        if tenure.ended_at.is_some() {
            return Ok(Some(tenure));
        }
        let event = serde_json::json!({
            "type": "coordinator_ended",
            "coordinator_id": id,
            "reason": reason,
            "cause": cause,
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE coordinators SET ended_at = ?2, end_reason = ?3
             WHERE id = ?1 AND ended_at IS NULL",
            params![id, at_ms as i64, reason],
        )?;
        self.tx.execute(
            "UPDATE coordinator_bindings SET to_at = ?2
             WHERE coordinator_id = ?1 AND to_at IS NULL",
            params![id, at_ms as i64],
        )?;
        tenure_by_id(self.tx, id)
    }

    /// The repository's active tenure, as this transaction sees it.
    pub(super) fn active_coordinator(&self, repo: &str) -> StoreResult<Option<StoredTenure>> {
        Ok(tenures(self.tx, "t.repo = ?1 AND t.ended_at IS NULL", [repo])?.pop())
    }

    /// The active tenure bound to `pane_id`, as this transaction sees it.
    pub(super) fn coordinator_of_pane(&self, pane_id: &str) -> StoreResult<Option<StoredTenure>> {
        Ok(tenures(self.tx, ACTIVE_OF_PANE, [pane_id])?.pop())
    }

    fn coordinator_event(&self, id: &str, event: &Value, at_ms: u64) -> StoreResult<i64> {
        self.event(&EventRow {
            worker_id: id,
            direction: super::Direction::Herdr,
            record: &Recorded::Event(event),
            ts_ms: at_ms,
        })
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
                    :gone_seq, :owner_pane, :owner_session, :acked_seq, :owner_gone, :degraded,
                    :item, :repo, :started_ms, :ended_ms, :done_commits, :questions_asked,
                    :ended_mid_turn, :item_title, :verification, :takeover_id,
                    :owner_coordinator_id, :broker)
                 ON CONFLICT (id) DO UPDATE SET {}",
                WORKER_COLUMNS
                    .split(", ")
                    .filter(|column| *column != "id")
                    .map(|column| match column {
                        // Only grows, whichever server writes the row.
                        "acked_seq" =>
                            "acked_seq = max(workers.acked_seq, excluded.acked_seq)".to_owned(),
                        _ => format!("{column} = excluded.{column}"),
                    })
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
                ":owner_pane": status.owner_pane,
                ":owner_session": status.owner_session,
                ":acked_seq": status.acked_seq,
                ":owner_gone": status.owner_gone,
                ":degraded": status.degraded,
                ":item": status.item,
                ":repo": status.repo,
                ":started_ms": status.started_ms.map(|ms| ms as i64),
                ":ended_ms": status.ended_ms.map(|ms| ms as i64),
                ":done_commits": serde_json::to_string(&status.done_commits)
                    .unwrap_or_else(|_| "[]".into()),
                ":questions_asked": status.questions_asked,
                ":ended_mid_turn": status.ended_mid_turn,
                ":item_title": status.item_title,
                ":verification": status
                    .verification
                    .as_ref()
                    .and_then(|verification| serde_json::to_string(verification).ok()),
                ":takeover_id": status.takeover_id,
                ":owner_coordinator_id": status.owner_coordinator,
                ":broker": status
                    .broker
                    .as_ref()
                    .and_then(|broker| serde_json::to_string(broker).ok()),
            },
        )?;
        Ok(())
    }
}

/// A tenure as `coordinator.start` asks for it.
pub(super) struct NewTenure<'a> {
    pub(super) id: &'a str,
    pub(super) repo: &'a str,
    pub(super) pane_id: &'a str,
    pub(super) session_id: Option<&'a str>,
}

/// One coordination tenure with its latest binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredTenure {
    pub(super) id: String,
    pub(super) repo: String,
    pub(super) item: Option<String>,
    pub(super) started_at: u64,
    pub(super) ended_at: Option<u64>,
    pub(super) end_reason: Option<String>,
    pub(super) epoch: i64,
    pub(super) pane_id: Option<String>,
    pub(super) session_id: Option<String>,
}

const ACTIVE_OF_PANE: &str = "t.ended_at IS NULL AND t.id IN (SELECT coordinator_id \
    FROM coordinator_bindings WHERE pane_id = ?1 AND to_at IS NULL)";

/// The tenures `filter` (over `coordinators t`) selects, oldest first, each
/// with its latest binding.
fn tenures(
    conn: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> StoreResult<Vec<StoredTenure>> {
    let mut statement = conn.prepare(&format!(
        "SELECT t.id, t.repo, t.item, t.started_at, t.ended_at, t.end_reason, t.epoch,
             b.pane_id, b.session_id
         FROM coordinators t
         LEFT JOIN coordinator_bindings b ON b.rowid = (SELECT rowid FROM coordinator_bindings
             WHERE coordinator_id = t.id ORDER BY from_at DESC, rowid DESC LIMIT 1)
         WHERE {filter}
         ORDER BY t.started_at, t.rowid"
    ))?;
    let rows = statement.query_map(params, |row| {
        Ok(StoredTenure {
            id: row.get(0)?,
            repo: row.get(1)?,
            item: row.get(2)?,
            started_at: row.get::<_, i64>(3)? as u64,
            ended_at: row.get::<_, Option<i64>>(4)?.map(|ms| ms as u64),
            end_reason: row.get(5)?,
            epoch: row.get(6)?,
            pane_id: row.get(7)?,
            session_id: row.get(8)?,
        })
    })?;
    rows.collect()
}

fn tenure_by_id(conn: &Connection, id: &str) -> StoreResult<Option<StoredTenure>> {
    Ok(tenures(conn, "t.id = ?1", [id])?.pop())
}

impl Store {
    /// The active tenures, of one repository when given.
    pub(super) fn active_coordinators(&self, repo: Option<&str>) -> StoreResult<Vec<StoredTenure>> {
        let conn = lock(&self.conn);
        match repo {
            Some(repo) => tenures(&conn, "t.ended_at IS NULL AND t.repo = ?1", [repo]),
            None => tenures(&conn, "t.ended_at IS NULL", []),
        }
    }

    #[cfg(test)]
    pub(super) fn coordinator(&self, id: &str) -> StoreResult<Option<StoredTenure>> {
        tenure_by_id(&lock(&self.conn), id)
    }

    /// The active tenure bound to `pane_id`.
    pub(super) fn coordinator_of_pane(&self, pane_id: &str) -> StoreResult<Option<StoredTenure>> {
        Ok(tenures(&lock(&self.conn), ACTIVE_OF_PANE, [pane_id])?.pop())
    }
}

const WORKER_COLUMNS: &str = "id, name, cwd, workspace_id, model, slot, state, pid, \
session_id, turns, last_result, rate_limit, tool_sessions, exit_code, exit_signal, \
stop_requested_ms, takeover_ms, takeover_tab, takeover_error, takeover_unfinished, refusal, \
exited, lost, end_note, last_seq, turn_seq, turn_end_seq, gone_seq, owner_pane, owner_session, \
acked_seq, owner_gone, degraded, item, repo, started_ms, ended_ms, done_commits, questions_asked, \
ended_mid_turn, item_title, verification, takeover_id, owner_coordinator_id, broker";

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
    status.owner_pane = row.get(28)?;
    status.owner_session = row.get(29)?;
    status.acked_seq = row.get(30)?;
    status.owner_gone = row.get(31)?;
    status.degraded = row.get(32)?;
    status.item = row.get(33)?;
    status.repo = row.get(34)?;
    status.started_ms = row.get::<_, Option<i64>>(35)?.map(|ms| ms as u64);
    status.ended_ms = row.get::<_, Option<i64>>(36)?.map(|ms| ms as u64);
    status.done_commits = json(37)?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    status.questions_asked = row.get(38)?;
    status.ended_mid_turn = row.get(39)?;
    status.item_title = row.get(40)?;
    status.verification = json(41)?.and_then(|value| serde_json::from_value(value).ok());
    status.takeover_id = row.get(42)?;
    status.owner_coordinator = row.get(43)?;
    status.broker = json(44)?.and_then(|value| serde_json::from_value(value).ok());
    Ok(status)
}

/// Fills a status's open questions (pending or answering), oldest first,
/// and the most recently settled ones.
fn load_questions(conn: &Connection, status: &mut Status) -> StoreResult<()> {
    let mut pending = conn.prepare(
        "SELECT question, input, asked_seq, state, escalated FROM questions
         WHERE worker_id = ?1 AND state IN ('pending', 'answering')
         ORDER BY asked_seq, rowid",
    )?;
    let rows = pending.query_map([&status.worker_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (question, input, asked_seq, state, escalated) = row?;
        let Ok(question) = serde_json::from_str(&question) else {
            continue;
        };
        status.questions.push(Pending {
            question,
            input: serde_json::from_str(&input)
                .unwrap_or_else(|_| Value::Object(Default::default())),
            asked_seq,
            answering: state == "answering",
            cleared: None,
            escalated,
        });
    }
    let mut settled = conn.prepare(
        "SELECT request_id, how FROM questions
         WHERE worker_id = ?1 AND state NOT IN ('pending', 'answering')
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
pub(super) fn question_state(how: &str) -> &'static str {
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
        for (index, (direction, event)) in events.iter().enumerate() {
            status.apply(*direction, event);
            seqs.push(
                store
                    .transaction(|tx| {
                        let seq = tx.event(&EventRow {
                            worker_id: "w1",
                            direction: *direction,
                            record: &Recorded::Event(event),
                            ts_ms: 10 + index as u64,
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
                 DROP TABLE receipts;
                 ALTER TABLE workers DROP COLUMN owner_pane;
                 ALTER TABLE workers DROP COLUMN owner_session;
                 ALTER TABLE workers DROP COLUMN acked_seq;
                 ALTER TABLE workers DROP COLUMN owner_gone;
                 ALTER TABLE workers DROP COLUMN degraded;
                 ALTER TABLE questions DROP COLUMN escalated;
                 ALTER TABLE workers DROP COLUMN item;
                 ALTER TABLE workers DROP COLUMN repo;
                 ALTER TABLE workers DROP COLUMN started_ms;
                 ALTER TABLE workers DROP COLUMN ended_ms;
                 ALTER TABLE workers DROP COLUMN done_commits;
                 ALTER TABLE workers DROP COLUMN questions_asked;
                 ALTER TABLE workers DROP COLUMN ended_mid_turn;
                 ALTER TABLE workers DROP COLUMN item_title;
                 ALTER TABLE workers DROP COLUMN verification;
                 ALTER TABLE workers DROP COLUMN takeover_id;
                 DROP TABLE coordinator_bindings;
                 DROP TABLE coordinators;
                 ALTER TABLE workers DROP COLUMN owner_coordinator_id;
                 ALTER TABLE workers DROP COLUMN broker;
                 UPDATE meta SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(store);
        let loaded = Store::open(&path).unwrap().load("w1").unwrap().unwrap();
        assert_eq!(
            (loaded.turn_seq, loaded.turn_end_seq, loaded.gone_seq),
            (Some(seqs[1]), seqs[3], seqs[3])
        );
        // The run's times come from its events; an older worker has no
        // item, so `worker.runs` lists it unassigned.
        assert_eq!(
            (loaded.started_ms, loaded.ended_ms, loaded.ended_mid_turn),
            (Some(10), Some(13), false)
        );
        assert_eq!((loaded.item, loaded.repo), (None, None));
        assert_eq!(loaded.item_title, None);
        assert!(loaded.done_commits.is_empty());
        assert_eq!(loaded.verification, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_item_title_is_kept_and_filled_from_the_started_event_of_older_rows() {
        let dir = scratch("item-title");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let started = json!({"type": "started", "cwd": "/repo", "item": "t-abcd2345",
            "item_title": "Items popup"});
        append(&store, "w1", &started).unwrap();
        append(&store, "w2", &json!({"type": "started", "cwd": "/repo"})).unwrap();
        assert_eq!(
            store.load("w1").unwrap().unwrap().item_title.as_deref(),
            Some("Items popup")
        );
        // A database from before the column: the migration fills it from
        // the `started` event.
        store
            .connection()
            .execute_batch(&format!(
                "ALTER TABLE workers DROP COLUMN item_title;
                 ALTER TABLE workers DROP COLUMN verification;
                 ALTER TABLE workers DROP COLUMN takeover_id;
                 DROP TABLE coordinator_bindings;
                 DROP TABLE coordinators;
                 ALTER TABLE workers DROP COLUMN owner_coordinator_id;
                 ALTER TABLE workers DROP COLUMN broker;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 5
            ))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.load("w1").unwrap().unwrap().item_title.as_deref(),
            Some("Items popup")
        );
        assert_eq!(store.load("w2").unwrap().unwrap().item_title, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cleanup_settles_only_rows_still_pending() {
        let dir = scratch("cleanup-guard");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let question = json!({"type": "question", "input": {},
            "question": question_from_request(
                "q1", &json!({"tool_name": "Bash", "input": {"command": "ls"}}), "asked")});
        append(&store, "w1", &question).unwrap();
        // Another writer began answering it; this status has not seen that.
        store
            .connection()
            .execute("UPDATE questions SET state = 'answering'", [])
            .unwrap();
        let mut status = Status::new("w1".to_owned());
        status.apply(Direction::Herdr, &question);
        let before = status.open_questions();
        let ended = json!({"type": "result", "subtype": "success", "is_error": false});
        status.apply(Direction::Out, &ended);
        assert!(status.questions.is_empty());
        store
            .transaction(|tx| {
                let seq = tx.event(&EventRow {
                    worker_id: "w1",
                    direction: Direction::Out,
                    record: &Recorded::Event(&ended),
                    ts_ms: 1,
                })?;
                tx.questions(seq, &before, &status)
            })
            .unwrap();
        let state: String = store
            .connection()
            .query_row("SELECT state FROM questions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(state, "answering");
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
                "broker": {"pid": 4241, "socket": "/state/w1.sock", "cwd": "/link"},
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
            (
                "herdr",
                json!({"type": "takeover", "at_ms": 5, "takeover_id": "tk-w1-5"}),
            ),
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
