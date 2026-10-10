//! The decision ledger (`decision.*`): the user's open questions and
//! decisions, kept in the worker store so that a coordinator looks an
//! answer up instead of asking the user again. The Claude integration's
//! question gate allows a coordinator's `AskUserQuestion` only when it cites
//! an open record (or a capability only the user has), and refuses one that
//! cites a decided record with its answer; its stop check counts the
//! repository's open records as the user's turn.
//!
//! A record is open, decided or superseded. Only an open record is decided,
//! once; a record that a later one replaces (`supersedes`) is superseded in
//! the same transaction, so at most one record of a chain holds. Records are
//! never deleted.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::runs::store_error;
use super::{now_ms, repository_of, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    DecisionAddParams, DecisionDecideParams, DecisionListParams, DecisionRecord, DecisionSource,
    DecisionStatus,
};

/// The ledger's migration.
pub(super) const DECISIONS_MIGRATION: &str = r#"
-- The decision ledger (`decision.add`, `decision.decide`): one row per
-- question for the user or decision of theirs. `repo` is NULL for a record
-- that holds in every repository; `status` is open, decided or superseded;
-- `source` (user, relayed, menu) and `relayed_by` say where a decision came
-- from; `supersedes` names the record it replaced. Times are Unix
-- milliseconds. Rows are never deleted.
CREATE TABLE decisions (
    id TEXT PRIMARY KEY,
    repo TEXT,
    scope TEXT NOT NULL,
    statement TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'decided', 'superseded')),
    answer TEXT,
    source TEXT CHECK (source IN ('user', 'relayed', 'menu')),
    relayed_by TEXT,
    item TEXT,
    entry TEXT,
    supersedes TEXT REFERENCES decisions (id),
    created_ms INTEGER NOT NULL,
    decided_ms INTEGER
);
CREATE INDEX decisions_by_repo ON decisions (repo, status);
CREATE TRIGGER decisions_no_delete BEFORE DELETE ON decisions
BEGIN SELECT RAISE(ABORT, 'decisions are never deleted'); END;
"#;

/// The scope a record without one gets.
const DEFAULT_SCOPE: &str = "repository";

/// A new record id: `d-` and 8 lowercase base32 characters, from a hash of
/// the time, this process, a counter and the statement.
fn new_decision_id(statement: &str) -> String {
    static RECORDS: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let count = RECORDS.fetch_add(1, Ordering::Relaxed);
    let digest = Sha256::digest(format!(
        "{nanos}:{}:{count}:{statement}",
        std::process::id()
    ));
    let bits = digest[..5]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let id: String = (0..8)
        .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 31) as usize] as char)
        .collect();
    format!("d-{id}")
}

/// Whether `id` has the form of a record id.
fn is_decision_id(id: &str) -> bool {
    id.strip_prefix("d-").is_some_and(|rest| {
        rest.len() == 8
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
    })
}

fn status_name(status: DecisionStatus) -> &'static str {
    match status {
        DecisionStatus::Open => "open",
        DecisionStatus::Decided => "decided",
        DecisionStatus::Superseded => "superseded",
        DecisionStatus::Unknown => "unknown",
    }
}

fn source_name(source: DecisionSource) -> &'static str {
    match source {
        DecisionSource::User => "user",
        DecisionSource::Relayed => "relayed",
        DecisionSource::Menu => "menu",
        DecisionSource::Unknown => "unknown",
    }
}

fn parse_status(name: &str) -> DecisionStatus {
    match name {
        "open" => DecisionStatus::Open,
        "decided" => DecisionStatus::Decided,
        "superseded" => DecisionStatus::Superseded,
        _ => DecisionStatus::Unknown,
    }
}

fn parse_source(name: &str) -> DecisionSource {
    match name {
        "user" => DecisionSource::User,
        "relayed" => DecisionSource::Relayed,
        "menu" => DecisionSource::Menu,
        _ => DecisionSource::Unknown,
    }
}

/// A non-empty text, trimmed, or the refusal naming `what`.
fn text(value: &str, what: &str) -> Result<String, WorkerError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(WorkerError::Invalid(format!("{what} is empty")));
    }
    Ok(value.to_owned())
}

fn optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Checks where a decision came from: a known source, and a named
/// coordinator exactly when it was relayed.
fn checked_source(
    source: Option<DecisionSource>,
    relayed_by: Option<&str>,
) -> Result<(DecisionSource, Option<String>), WorkerError> {
    let relayed_by = optional_text(relayed_by);
    match (source, relayed_by) {
        (None | Some(DecisionSource::Unknown), _) => Err(WorkerError::Invalid(
            "a decision needs its source: user, relayed or menu".into(),
        )),
        (Some(DecisionSource::Relayed), None) => Err(WorkerError::Invalid(
            "a relayed decision names the coordinator that relayed it (relayed_by)".into(),
        )),
        (Some(DecisionSource::Relayed), Some(by)) => Ok((DecisionSource::Relayed, Some(by))),
        (Some(_), Some(_)) => Err(WorkerError::Invalid(
            "relayed_by is taken only with source relayed".into(),
        )),
        (Some(source), None) => Ok((source, None)),
    }
}

const COLUMNS: &str = "id, repo, scope, statement, status, answer, source, relayed_by, item, \
                       entry, supersedes, created_ms, decided_ms, \
                       (SELECT later.id FROM decisions later WHERE later.supersedes = decisions.id)";

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionRecord> {
    Ok(DecisionRecord {
        id: row.get(0)?,
        repo: row.get(1)?,
        scope: row.get(2)?,
        statement: row.get(3)?,
        status: parse_status(&row.get::<_, String>(4)?),
        answer: row.get(5)?,
        source: row
            .get::<_, Option<String>>(6)?
            .as_deref()
            .map(parse_source),
        relayed_by: row.get(7)?,
        item: row.get(8)?,
        entry: row.get(9)?,
        supersedes: row.get(10)?,
        created_ms: row.get::<_, i64>(11)? as u64,
        decided_ms: row.get::<_, Option<i64>>(12)?.map(|ms| ms as u64),
        superseded_by: row.get(13)?,
    })
}

fn record(conn: &Connection, id: &str) -> rusqlite::Result<Option<DecisionRecord>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM decisions WHERE id = ?1"),
        [id],
        record_from_row,
    )
    .optional()
}

fn check_id(id: &str) -> Result<(), WorkerError> {
    if is_decision_id(id) {
        Ok(())
    } else {
        Err(WorkerError::Invalid(format!(
            "{id:?} is not a decision id (d- and 8 characters a-z, 2-7)"
        )))
    }
}

fn not_found(id: &str) -> WorkerError {
    WorkerError::NotFound(format!(
        "decision {id} is not in the ledger (`herdr decision list`)"
    ))
}

/// The open records of `repo` and those that hold in every repository,
/// oldest first: what the repository waits on the user for.
pub(super) fn open_of(conn: &Connection, repo: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT id FROM decisions WHERE status = 'open' AND (repo IS NULL OR repo = ?1)
         ORDER BY created_ms, rowid",
    )?;
    let ids = statement.query_map([repo], |row| row.get::<_, String>(0))?;
    ids.collect()
}

impl WorkerSupervisor {
    /// The repository of `cwd`, as the ledger names it.
    fn decision_repo(cwd: &str) -> Result<String, WorkerError> {
        repository_of(Path::new(cwd))
            .ok_or_else(|| WorkerError::Invalid(format!("{cwd} is not in a git repository")))
    }

    /// `decision.add`: a new record, open, or decided when `answer` is
    /// given. The record it supersedes, which must exist and not be
    /// superseded already, becomes superseded in the same transaction.
    pub(crate) fn decision_add(
        &self,
        params: DecisionAddParams,
    ) -> Result<DecisionRecord, WorkerError> {
        let statement = text(&params.statement, "statement")?;
        let repo = params.cwd.as_deref().map(Self::decision_repo).transpose()?;
        let scope = optional_text(params.scope.as_deref()).unwrap_or_else(|| DEFAULT_SCOPE.into());
        let item = optional_text(params.item.as_deref());
        if let Some(item) = &item {
            super::check_item_id(item)?;
        }
        let answer = optional_text(params.answer.as_deref());
        let (status, source, relayed_by) = match &answer {
            Some(_) => {
                let (source, by) = checked_source(params.source, params.relayed_by.as_deref())?;
                (DecisionStatus::Decided, Some(source), by)
            }
            None if params.source.is_some() || params.relayed_by.is_some() => {
                return Err(WorkerError::Invalid(
                    "source and relayed_by go with an answer; an open question has neither".into(),
                ))
            }
            None => (DecisionStatus::Open, None, None),
        };
        let supersedes = optional_text(params.supersedes.as_deref());
        if let Some(old) = &supersedes {
            check_id(old)?;
        }
        let id = new_decision_id(&statement);
        let at = now_ms();
        let store = self.run_store()?;
        let outcome = store
            .transaction(|tx| {
                let conn = tx.connection();
                if let Some(old) = &supersedes {
                    match record(conn, old)? {
                        None => return Ok(Err(not_found(old))),
                        Some(old) if old.status == DecisionStatus::Superseded => {
                            return Ok(Err(WorkerError::Invalid(format!(
                                "decision {} is superseded already, by {}",
                                old.id,
                                old.superseded_by.as_deref().unwrap_or("?")
                            ))))
                        }
                        Some(_) => {}
                    }
                    conn.execute(
                        "UPDATE decisions SET status = 'superseded' WHERE id = ?1",
                        [old],
                    )?;
                }
                conn.execute(
                    "INSERT INTO decisions (id, repo, scope, statement, status, answer, source,
                     relayed_by, item, entry, supersedes, created_ms, decided_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        id,
                        repo,
                        scope,
                        statement,
                        status_name(status),
                        answer,
                        source.map(source_name),
                        relayed_by,
                        item,
                        optional_text(params.entry.as_deref()),
                        supersedes,
                        at as i64,
                        answer.as_ref().map(|_| at as i64),
                    ],
                )?;
                Ok(Ok(record(conn, &id)?))
            })
            .map_err(store_error)?;
        outcome?.ok_or_else(|| not_found(&id))
    }

    /// `decision.decide`: the user's answer to an open record. A decided or
    /// superseded record is refused with what it holds: a decision is
    /// changed by a new record that supersedes it.
    pub(crate) fn decision_decide(
        &self,
        params: DecisionDecideParams,
    ) -> Result<DecisionRecord, WorkerError> {
        check_id(&params.id)?;
        let answer = text(&params.answer, "answer")?;
        let (source, relayed_by) =
            checked_source(Some(params.source), params.relayed_by.as_deref())?;
        let at = now_ms();
        let store = self.run_store()?;
        let outcome = store
            .transaction(|tx| {
                let conn = tx.connection();
                let Some(current) = record(conn, &params.id)? else {
                    return Ok(Err(not_found(&params.id)));
                };
                match current.status {
                    DecisionStatus::Open => {}
                    DecisionStatus::Decided => {
                        return Ok(Err(WorkerError::Invalid(format!(
                            "decision {} is decided already: {}; record a change with \
                             `herdr decision add --supersedes {}`",
                            current.id,
                            current.answer.as_deref().unwrap_or(""),
                            current.id
                        ))))
                    }
                    _ => {
                        return Ok(Err(WorkerError::Invalid(format!(
                            "decision {} is superseded by {}",
                            current.id,
                            current.superseded_by.as_deref().unwrap_or("?")
                        ))))
                    }
                }
                conn.execute(
                    "UPDATE decisions SET status = 'decided', answer = ?2, source = ?3,
                     relayed_by = ?4, decided_ms = ?5 WHERE id = ?1 AND status = 'open'",
                    params![
                        current.id,
                        answer,
                        source_name(source),
                        relayed_by,
                        at as i64
                    ],
                )?;
                Ok(Ok(record(conn, &current.id)?))
            })
            .map_err(store_error)?;
        outcome?.ok_or_else(|| not_found(&params.id))
    }

    /// `decision.list`: the records, oldest first; of one repository (with
    /// the records that hold everywhere) and one status when asked.
    pub(crate) fn decision_list(
        &self,
        params: DecisionListParams,
    ) -> Result<Vec<DecisionRecord>, WorkerError> {
        let repo = params.cwd.as_deref().map(Self::decision_repo).transpose()?;
        if params.status == Some(DecisionStatus::Unknown) {
            return Err(WorkerError::Invalid(
                "status is open, decided or superseded".into(),
            ));
        }
        let status = params.status.map(status_name);
        self.run_store()?
            .read(|conn| {
                let mut statement = conn.prepare(&format!(
                    "SELECT {COLUMNS} FROM decisions
                     WHERE (?1 IS NULL OR repo IS NULL OR repo = ?1) AND (?2 IS NULL OR status = ?2)
                     ORDER BY created_ms, rowid"
                ))?;
                let rows = statement.query_map(params![repo, status], record_from_row)?;
                rows.collect()
            })
            .map_err(store_error)
    }

    /// `decision.get`: one record.
    pub(crate) fn decision_get(&self, id: &str) -> Result<DecisionRecord, WorkerError> {
        check_id(id)?;
        self.run_store()?
            .read(|conn| record(conn, id))
            .map_err(store_error)?
            .ok_or_else(|| not_found(id))
    }

    /// The open records of `repo`'s ledger (and those that hold in every
    /// repository): an empty list when the store cannot be read.
    pub(super) fn open_decisions(&self, repo: &str) -> Vec<String> {
        let Ok(store) = self.run_store() else {
            return Vec::new();
        };
        store.read(|conn| open_of(conn, repo)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_id_is_d_and_eight_base32_characters() {
        let id = new_decision_id("x");
        assert!(is_decision_id(&id), "{id}");
        for bad in [
            "d-abc",
            "d-ABCDEFGH",
            "t-abcdefgh",
            "d-abcdefg1",
            "d-abcdefghi",
        ] {
            assert!(!is_decision_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_decision_names_its_source() {
        assert!(checked_source(None, None).is_err());
        assert!(checked_source(Some(DecisionSource::Unknown), None).is_err());
        assert!(checked_source(Some(DecisionSource::Relayed), None).is_err());
        assert!(checked_source(Some(DecisionSource::User), Some("c-1")).is_err());
        assert_eq!(
            checked_source(Some(DecisionSource::Relayed), Some(" todo-herdr ")).unwrap(),
            (DecisionSource::Relayed, Some("todo-herdr".into()))
        );
        assert_eq!(
            checked_source(Some(DecisionSource::Menu), Some(" ")).unwrap(),
            (DecisionSource::Menu, None)
        );
    }

    #[test]
    fn every_status_and_source_has_its_wire_name() {
        for status in [
            DecisionStatus::Open,
            DecisionStatus::Decided,
            DecisionStatus::Superseded,
        ] {
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(status_name(status).into())
            );
            assert_eq!(parse_status(status_name(status)), status);
        }
        for source in [
            DecisionSource::User,
            DecisionSource::Relayed,
            DecisionSource::Menu,
        ] {
            assert_eq!(
                serde_json::to_value(source).unwrap(),
                serde_json::Value::String(source_name(source).into())
            );
            assert_eq!(parse_source(source_name(source)), source);
        }
    }
}
