//! A coordinator's inbox over all of its workers (`worker.events`).
//!
//! Every event that changes what a coordination tenure has to look at (a
//! question asked, a turn's end, the worker's exit, a worker joining it, its
//! end acknowledged, the worker moving to another tenure) adds an `inbox` row
//! for that tenure in the same transaction as the event, under the event's
//! `seq`: one server-wide sequence, which the store never reuses and which a
//! live handoff and a restart keep, since the new server opens the same
//! store. A reader passes the cursor of its last reply and gets every later
//! row of its tenure, in order, as a bounded batch; with `wait` it blocks
//! while there is none, woken by the commits ([`Shared::changed`]), never by
//! a timer.
//!
//! A cursor names the store's incarnation (a random id the migration wrote)
//! and a `seq`. One the store cannot serve (another incarnation, older than
//! the inbox, or past the latest event) is answered with `resync_required`
//! and a snapshot, never with a silent skip. Reading resolves nothing: a
//! pending question stays pending, and the snapshot carries it however old
//! the event that asked it.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::coordinators::{info, store_error};
use super::store::{Recorded, StoreResult, Tx};
use super::{lock, Before, Direction, Pending, Registry, Status, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    CoordinatorInfo, WorkerEvent, WorkerEventKind, WorkerEventsParams, WorkerEventsSnapshot,
    WorkerQuestion, WorkerState,
};
use std::sync::MutexGuard;
use std::time::Duration;

/// The inbox table and the store's incarnation.
pub(super) const INBOX_MIGRATION: &str = r#"
-- Each coordination tenure's inbox: what an event (`seq`) of one of its
-- workers means for it (`kind`: question, turn_end, exit, joined, left,
-- reowned), with what the reader needs of it (`detail`, JSON). Written in
-- the event's transaction; one event may add rows for two tenures (a move)
-- or two kinds (an exit its owner asked for, acknowledged at once).
CREATE TABLE inbox (
    seq INTEGER NOT NULL,
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    worker_id TEXT NOT NULL,
    detail TEXT NOT NULL,
    PRIMARY KEY (owner, seq, kind)
);
-- The store's identity in every cursor: a cursor of another store (one
-- recreated since) is refused with `resync_required`.
INSERT INTO meta (key, value) VALUES ('incarnation', lower(hex(randomblob(8))));
-- Events up to this `seq` came before the inbox and have no rows: a cursor
-- older than it cannot be served.
INSERT INTO meta (key, value)
    VALUES ('inbox_from', (SELECT coalesce(max(seq), 0) FROM events));
"#;

/// How many events one reply carries when the caller does not say.
const DEFAULT_LIMIT: u32 = 100;
/// The most events one reply carries.
const MAX_LIMIT: u32 = 1000;

fn kind_name(kind: WorkerEventKind) -> &'static str {
    match kind {
        WorkerEventKind::Question => "question",
        WorkerEventKind::TurnEnd => "turn_end",
        WorkerEventKind::Exit => "exit",
        WorkerEventKind::Joined => "joined",
        WorkerEventKind::Left => "left",
        WorkerEventKind::Reowned => "reowned",
        WorkerEventKind::HeldOutput => "held_output",
        WorkerEventKind::Unknown => "unknown",
    }
}

fn parse_kind(name: &str) -> WorkerEventKind {
    serde_json::from_value(Value::String(name.to_owned())).unwrap_or(WorkerEventKind::Unknown)
}

/// The inbox rows the event just folded into `status` adds: who gets it,
/// what it is and its detail. `before` is the status before the event.
fn rows(
    before: &Before,
    status: &Status,
    direction: Direction,
    record: &Recorded<'_>,
) -> Vec<(String, WorkerEventKind, Value)> {
    let mut rows = Vec::new();
    let owner = status.owner_coordinator.as_deref();
    if before.owner.as_deref() != owner {
        if let Some(old) = &before.owner {
            rows.push((
                old.clone(),
                WorkerEventKind::Reowned,
                json!({"to_coordinator_id": owner}),
            ));
        }
        if let Some(new) = owner {
            rows.push((
                new.to_owned(),
                WorkerEventKind::Joined,
                json!({"from_coordinator_id": before.owner}),
            ));
        }
    }
    let Some(owner) = owner else {
        return rows;
    };
    let state = || json!({"state": status.state});
    if status.is_gone() && (status.exited, status.lost) != before.gone {
        rows.push((owner.to_owned(), WorkerEventKind::Exit, state()));
    } else {
        let asked: Vec<WorkerQuestion> = status
            .questions
            .iter()
            .filter(|pending| {
                !before
                    .pending
                    .iter()
                    .any(|(id, _)| *id == pending.question.request_id)
            })
            .map(Pending::shown)
            .collect();
        let result = matches!(record, Recorded::Event(event)
            if direction == Direction::Out && event["type"].as_str() == Some("result"));
        if !asked.is_empty() {
            rows.push((
                owner.to_owned(),
                WorkerEventKind::Question,
                json!({"questions": asked}),
            ));
        } else if !status.is_gone() && status.turn_ended() && (!before.turn_ended || result) {
            rows.push((owner.to_owned(), WorkerEventKind::TurnEnd, state()));
        }
        let held = matches!(record, Recorded::Event(event)
            if direction == Direction::Herdr && event["type"].as_str() == Some("tool_output_held"));
        if held {
            rows.push((
                owner.to_owned(),
                WorkerEventKind::HeldOutput,
                json!({"held_output": status.held_output}),
            ));
        }
    }
    if before.listed && !status.listed() {
        rows.push((owner.to_owned(), WorkerEventKind::Left, json!({})));
    }
    rows
}

/// Writes the inbox rows of event `seq` in its transaction.
pub(super) fn write(
    tx: &Tx<'_>,
    seq: i64,
    before: &Before,
    status: &Status,
    direction: Direction,
    record: &Recorded<'_>,
) -> StoreResult<()> {
    for (owner, kind, detail) in rows(before, status, direction, record) {
        tx.connection().execute(
            "INSERT OR IGNORE INTO inbox (seq, owner, kind, worker_id, detail)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                seq,
                owner,
                kind_name(kind),
                status.worker_id,
                detail.to_string()
            ],
        )?;
    }
    Ok(())
}

fn meta(conn: &Connection, key: &str) -> StoreResult<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .optional()
}

fn event_from_row(row: &rusqlite::Row<'_>) -> StoreResult<(i64, i64, WorkerEvent)> {
    let seq: i64 = row.get(0)?;
    let kind: String = row.get(1)?;
    let detail: String = row.get(3)?;
    let detail: Value = serde_json::from_str(&detail).unwrap_or(Value::Null);
    let text = |key: &str| detail[key].as_str().map(str::to_owned);
    Ok((
        row.get(4)?,
        seq,
        WorkerEvent {
            seq,
            worker_id: row.get(2)?,
            kind: parse_kind(&kind),
            questions: serde_json::from_value(detail["questions"].clone()).unwrap_or_default(),
            state: serde_json::from_value::<Option<WorkerState>>(detail["state"].clone())
                .unwrap_or(None),
            from_coordinator_id: text("from_coordinator_id"),
            to_coordinator_id: text("to_coordinator_id"),
            held_output: serde_json::from_value(detail["held_output"].clone()).unwrap_or(None),
        },
    ))
}

/// `owner`'s rows after `after`, in order, at most `limit` events, but never
/// a part of one `seq`'s rows: a cursor past it would skip the rest. Returns
/// them and whether more follow.
fn read_after(
    conn: &Connection,
    owner: &str,
    after: i64,
    limit: u32,
) -> StoreResult<(Vec<WorkerEvent>, bool)> {
    const COLUMNS: &str = "SELECT seq, kind, worker_id, detail, rowid FROM inbox";
    let mut statement = conn.prepare(&format!(
        "{COLUMNS} WHERE owner = ?1 AND seq > ?2 ORDER BY seq, rowid LIMIT ?3"
    ))?;
    let mut rows: Vec<(i64, i64, WorkerEvent)> = statement
        .query_map(params![owner, after, limit], event_from_row)?
        .collect::<StoreResult<_>>()?;
    let Some(&(last_rowid, last_seq, _)) = rows.last() else {
        return Ok((Vec::new(), false));
    };
    let mut rest = conn.prepare(&format!(
        "{COLUMNS} WHERE owner = ?1 AND seq = ?2 AND rowid > ?3 ORDER BY rowid"
    ))?;
    let same_seq: Vec<(i64, i64, WorkerEvent)> = rest
        .query_map(params![owner, last_seq, last_rowid], event_from_row)?
        .collect::<StoreResult<_>>()?;
    rows.extend(same_seq);
    let more = conn
        .query_row(
            "SELECT 1 FROM inbox WHERE owner = ?1 AND seq > ?2 LIMIT 1",
            params![owner, last_seq],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok((rows.into_iter().map(|(_, _, event)| event).collect(), more))
}

fn cursor(incarnation: &str, seq: i64) -> String {
    format!("{incarnation}-{seq}")
}

fn parse_cursor(text: &str) -> Result<(String, i64), WorkerError> {
    text.rsplit_once('-')
        .and_then(|(incarnation, seq)| Some((incarnation.to_owned(), seq.parse().ok()?)))
        .filter(|(incarnation, seq)| {
            !incarnation.is_empty()
                && incarnation.chars().all(|c| c.is_ascii_alphanumeric())
                && *seq >= 0
        })
        .ok_or_else(|| {
            WorkerError::Invalid(format!(
                "after {text:?} is not a worker.events cursor (`<incarnation>-<seq>`, a reply's \
                 next_cursor)"
            ))
        })
}

/// A `worker.events` reply.
pub(crate) struct Events {
    pub(crate) owner: CoordinatorInfo,
    pub(crate) events: Vec<WorkerEvent>,
    pub(crate) next_cursor: String,
    pub(crate) more: bool,
    pub(crate) resync_required: bool,
    pub(crate) snapshot: Option<WorkerEventsSnapshot>,
}

impl WorkerSupervisor {
    /// `worker.events`: the owner's inbox after `params.after`. With
    /// `params.wait` it blocks while that is empty, woken by the commits
    /// that write the inbox (they notify `changed` once the registry lock,
    /// held across the check and the block, lets them in), so no event
    /// lands between the check and the block unseen. `keep_waiting` runs at
    /// least every `liveness_check`, only so that a caller whose client went
    /// away or whose server stops or hands off can give up; that interval
    /// never decides the outcome. Returns `None` when the caller gave up.
    pub(crate) fn events(
        &self,
        params: &WorkerEventsParams,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<Events>, WorkerError> {
        let store = self.tenure_store()?;
        let owner = match (&params.owner, &params.owner_pane_id) {
            (Some(owner), _) => owner.clone(),
            (None, Some(pane)) => store
                .coordinator_of_pane(pane)
                .map_err(store_error)?
                .map(|tenure| tenure.id)
                .ok_or_else(|| {
                    WorkerError::CoordinatorNotFound(format!(
                        "pane {pane} is bound to no active coordinator"
                    ))
                })?,
            (None, None) => {
                return Err(WorkerError::Invalid(
                    "worker.events needs owner or owner_pane_id".into(),
                ))
            }
        };
        let after = params.after.as_deref().map(parse_cursor).transpose()?;
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut registry = lock(&self.shared.registry);
        loop {
            if let Some(reply) =
                self.events_now(&registry, &owner, after.as_ref(), limit, params)?
            {
                return Ok(Some(reply));
            }
            drop(registry);
            if !keep_waiting() {
                return Ok(None);
            }
            registry = lock(&self.shared.registry);
            // Looks again after `keep_waiting`, which ran without the lock,
            // then blocks with the lock taken for that look: a commit after
            // it needs the lock, so its notification finds this wait
            // blocked.
            if let Some(reply) =
                self.events_now(&registry, &owner, after.as_ref(), limit, params)?
            {
                return Ok(Some(reply));
            }
            registry = self
                .shared
                .changed
                .wait_timeout(registry, liveness_check)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// One look at the inbox under the registry lock: the reply, or `None`
    /// when the caller waits and there is nothing to answer yet.
    fn events_now(
        &self,
        registry: &MutexGuard<'_, Registry>,
        owner: &str,
        after: Option<&(String, i64)>,
        limit: u32,
        params: &WorkerEventsParams,
    ) -> Result<Option<Events>, WorkerError> {
        let store = self.tenure_store()?;
        let tenure = store
            .coordinator(owner)
            .map_err(store_error)?
            .ok_or_else(|| {
                WorkerError::CoordinatorNotFound(format!("coordinator {owner} not found"))
            })?;
        let ended = tenure.ended_at.is_some();
        let read = store.read(|conn| {
            let incarnation = meta(conn, "incarnation")?.unwrap_or_default();
            let floor: i64 = meta(conn, "inbox_from")?
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let latest: i64 =
                conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |row| {
                    row.get(0)
                })?;
            let from = match after {
                Some((of, seq)) if *of == incarnation && *seq >= floor && *seq <= latest => {
                    Some(*seq)
                }
                _ => None,
            };
            let (events, more) = match from {
                Some(seq) => read_after(conn, owner, seq, limit)?,
                None => (Vec::new(), false),
            };
            Ok((incarnation, latest, from, events, more))
        });
        let (incarnation, latest, from, events, more) = read.map_err(store_error)?;
        let resync_required = after.is_some() && from.is_none();
        let snapshot = from.is_none() || params.snapshot;
        if events.is_empty() && !snapshot && params.wait && !ended {
            return Ok(None);
        }
        let next = events
            .last()
            .map(|event| event.seq)
            .or(from)
            .unwrap_or(latest);
        Ok(Some(Events {
            owner: info(tenure),
            events,
            next_cursor: cursor(&incarnation, next),
            more,
            resync_required,
            snapshot: snapshot.then(|| snapshot_of(registry, owner)),
        }))
    }
}

/// The owner's listed workers as they are now, oldest first.
fn snapshot_of(registry: &Registry, owner: &str) -> WorkerEventsSnapshot {
    WorkerEventsSnapshot {
        workers: registry
            .workers
            .values()
            .filter(|entry| {
                entry.status.owner_coordinator.as_deref() == Some(owner) && entry.status.listed()
            })
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_names_the_incarnation_and_the_seq() {
        assert_eq!(
            parse_cursor(&cursor("ab12", 42)).unwrap(),
            ("ab12".to_owned(), 42)
        );
        for bad in ["", "42", "-42", "ab12-", "ab12-x", "ab12--1"] {
            assert_eq!(
                parse_cursor(bad).unwrap_err().code(),
                "invalid_request",
                "{bad}"
            );
        }
    }

    #[test]
    fn every_kind_has_its_wire_name() {
        for kind in [
            WorkerEventKind::Question,
            WorkerEventKind::TurnEnd,
            WorkerEventKind::Exit,
            WorkerEventKind::Joined,
            WorkerEventKind::Left,
            WorkerEventKind::Reowned,
        ] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                Value::String(kind_name(kind).to_owned())
            );
            assert_eq!(parse_kind(kind_name(kind)), kind);
        }
        assert_eq!(parse_kind("later"), WorkerEventKind::Unknown);
    }
}
