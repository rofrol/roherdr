//! Protocol problems coordinators report themselves (`herdr report`): a
//! herdr command, wait, sandbox or skill step that did not give its promised
//! outcome or a clear next step. A durable queue in the worker store, not
//! prompts into a session: each `report.record` is one occurrence of a
//! report, the report being its fingerprint (the kind and the summary with
//! ids, paths, numbers and commit hashes taken out, [`normalize_summary`]).
//! The first occurrence of a fingerprint notifies the herdr repository's
//! coordinator once (the user when no pane coordinates that repository);
//! later ones only count, listed with their sessions and times, except the
//! one that reopens a closed report, which notifies again. Only the herdr
//! repository's coordinator closes a report, naming the TODO item or commit
//! that fixed it, or as not reproducible. No timer acts on a report.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::warn;

use super::coordinators::store_error;
use super::store::{EventRow, Recorded, StoreResult, Tx};
use super::{now_ms, Direction, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    CoordinatorInfo, ReportEvidenceUpload, ReportInfo, ReportOccurrence, ReportRecordParams,
    ReportState,
};

/// The most evidence text an occurrence keeps, in bytes; the CLI sends no
/// more.
pub(crate) const EVIDENCE_MAX_BYTES: usize = 64 * 1024;
const SUMMARY_MAX_BYTES: usize = 500;
const KIND_MAX_CHARS: usize = 40;
const COMMAND_MAX_CHARS: usize = 2000;

/// The summary as it is fingerprinted: lowercase, one space between words,
/// and every path, number, commit hash and id replaced by `<path>`, `<n>`,
/// `<sha>` and `<id>`, so two reports of one problem from other
/// repositories, workers and runs share a fingerprint. A word is a run of
/// letters, digits and `_-./~\`; a path has `/` or `\` or starts with `~`;
/// a number is digits and dots; a hash is 7 or more hex digits with a
/// digit; an id is any other word with a digit, or a herdr id such as
/// `t-pulsqoxl` (one or two letters, `-`, eight base32 characters).
pub(crate) fn normalize_summary(summary: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let mut space = false;
    let flush = |word: &mut String, out: &mut String, space: &mut bool| {
        if word.is_empty() {
            return;
        }
        // A sentence's last dot or dash is not part of its word.
        let trimmed = word.trim_end_matches(['.', '-']);
        let tail = &word[trimmed.len()..];
        if *space && !out.is_empty() {
            out.push(' ');
        }
        *space = false;
        if !trimmed.is_empty() {
            out.push_str(&normalize_word(trimmed));
        }
        out.push_str(tail);
        word.clear();
    };
    for c in summary.chars() {
        if c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | '\\') {
            word.push(c);
            continue;
        }
        flush(&mut word, &mut out, &mut space);
        if c.is_whitespace() || c.is_control() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        }
    }
    flush(&mut word, &mut out, &mut space);
    out
}

fn normalize_word(word: &str) -> String {
    let has_digit = word.chars().any(|c| c.is_ascii_digit());
    if word.contains(['/', '\\']) || word.starts_with('~') {
        return "<path>".into();
    }
    if has_digit && word.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return "<n>".into();
    }
    if has_digit && word.len() >= 7 && word.chars().all(|c| c.is_ascii_hexdigit()) {
        return "<sha>".into();
    }
    if has_digit || is_herdr_id(word) {
        return "<id>".into();
    }
    word.to_lowercase()
}

/// `t-pulsqoxl`, `c-abcdefgh`, `r-yp27jex3`: one or two lowercase letters,
/// `-`, and eight lowercase base32 characters.
fn is_herdr_id(word: &str) -> bool {
    let Some((prefix, rest)) = word.split_once('-') else {
        return false;
    };
    (1..=2).contains(&prefix.len())
        && prefix.chars().all(|c| c.is_ascii_lowercase())
        && rest.len() == 8
        && rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c))
}

/// 16 hex characters of the SHA-256 of the kind and the normalized summary.
pub(crate) fn fingerprint(kind: &str, normalized: &str) -> String {
    let digest = Sha256::digest(format!("{kind}\n{normalized}"));
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether `repo` is herdr's own repository: its `Cargo.toml` names the
/// package `herdr`. Read only when a report is recorded or closed.
pub(crate) fn is_herdr_repository(repo: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(Path::new(repo).join("Cargo.toml")) else {
        return false;
    };
    text.parse::<toml::Table>().is_ok_and(|manifest| {
        manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            == Some("herdr")
    })
}

/// `r-12` (or `12`) as the report's number.
fn report_number(report_id: &str) -> Option<i64> {
    report_id
        .strip_prefix("r-")
        .unwrap_or(report_id)
        .parse()
        .ok()
        .filter(|number| *number > 0)
}

fn id_of(number: i64) -> String {
    format!("r-{number}")
}

fn event_key(number: i64) -> String {
    format!("report:{}", id_of(number))
}

/// What closed a report, as an occurrence that reopens it names it.
fn closure_text(fix: Option<&str>, not_reproducible: bool) -> String {
    match fix {
        Some(fix) if !not_reproducible => format!("fix {fix}"),
        _ => "not reproducible".into(),
    }
}

/// A TODO item id (`t-` and lowercase letters or digits) or a commit (7 to
/// 40 hex digits).
fn is_fix(fix: &str) -> bool {
    let item = fix.strip_prefix("t-").is_some_and(|rest| {
        rest.len() >= 4
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    let commit = (7..=40).contains(&fix.len()) && fix.chars().all(|c| c.is_ascii_hexdigit());
    item || commit
}

/// One `report.record` as it is written.
struct NewOccurrence<'a> {
    kind: &'a str,
    summary: &'a str,
    normalized: &'a str,
    fingerprint: &'a str,
    uncertain: bool,
    repo: Option<&'a str>,
    pane_id: Option<&'a str>,
    session_id: Option<&'a str>,
    coordinator_id: Option<&'a str>,
    command: Option<&'a str>,
    evidence: Option<&'a str>,
    evidence_source: Option<&'a str>,
    evidence_truncated: bool,
}

/// What `report.record` did: the report as it is now, this occurrence's
/// number, and whether it notifies (a new fingerprint, or a reopened
/// report).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedReport {
    pub(crate) report: ReportInfo,
    pub(crate) occurrence: u32,
    pub(crate) notify: bool,
}

/// The notification a recorded report sends: to the herdr coordinator's
/// pane, else to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportNotice {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) pane_id: Option<String>,
    /// `coordinator <id>` or `user`, as `report.record` answers it.
    pub(crate) notified: String,
}

/// The notification for `recorded`, sent to `coordinator` (the herdr
/// repository's) when one holds a pane; none for a repeat.
pub(crate) fn report_notice(
    recorded: &RecordedReport,
    coordinator: Option<&CoordinatorInfo>,
) -> Option<ReportNotice> {
    if !recorded.notify {
        return None;
    }
    let report = &recorded.report;
    let reopened = if recorded.occurrence > 1 {
        " reopened"
    } else {
        ""
    };
    let uncertain = if report.uncertain { " (uncertain)" } else { "" };
    let from = report
        .occurrences
        .last()
        .and_then(|occurrence| occurrence.repo.as_deref())
        .map(|repo| format!(" — from {repo}"))
        .unwrap_or_default();
    let body = format!("{}: {}{uncertain}{from}", report.kind, report.summary);
    let pane = coordinator
        .filter(|coordinator| !coordinator.headless)
        .and_then(|coordinator| Some((coordinator, coordinator.pane_id.clone()?)));
    Some(match pane {
        Some((coordinator, pane_id)) => ReportNotice {
            title: format!("Herdr report {}{reopened}", report.report_id),
            body,
            pane_id: Some(pane_id),
            notified: format!("coordinator {}", coordinator.coordinator_id),
        },
        None => ReportNotice {
            title: format!(
                "Herdr report {}{reopened} (no herdr coordinator)",
                report.report_id
            ),
            body,
            pane_id: None,
            notified: "user".into(),
        },
    })
}

fn one_line(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

impl WorkerSupervisor {
    fn report_store(&self) -> Result<&super::store::Store, WorkerError> {
        self.shared
            .store
            .as_ref()
            .map_err(|error| WorkerError::Io(std::io::Error::other(error.clone())))
    }

    /// Records one occurrence of `params`' problem: from pane `pane_id` (its
    /// public id; its active tenure is recorded with it), whose agent
    /// session is `pane_session` and directory `pane_cwd` when the params
    /// do not name them. Its evidence is copied into the worker directory's
    /// `reports/` first, so the occurrence never names a file that is not
    /// there.
    pub(crate) fn report_record(
        &self,
        params: &ReportRecordParams,
        pane_id: Option<&str>,
        pane_session: Option<&str>,
        pane_cwd: Option<&str>,
    ) -> Result<RecordedReport, WorkerError> {
        let kind = params.kind.trim();
        if kind.is_empty()
            || kind.chars().count() > KIND_MAX_CHARS
            || !kind
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(WorkerError::Invalid(format!(
                "kind must be a slug of lowercase letters, digits and -, at most \
                 {KIND_MAX_CHARS} characters, not {kind:?}"
            )));
        }
        let summary = params.summary.trim();
        if summary.is_empty() || summary.len() > SUMMARY_MAX_BYTES || !one_line(summary) {
            return Err(WorkerError::Invalid(format!(
                "summary must be one nonempty line of at most {SUMMARY_MAX_BYTES} bytes"
            )));
        }
        let normalized = normalize_summary(summary);
        let fingerprint = fingerprint(kind, &normalized);
        let tenure = pane_id.and_then(|pane| self.coordinator_of_pane(pane));
        let repo = params
            .cwd
            .as_deref()
            .or(pane_cwd)
            .and_then(super::repository_of_dir)
            .or_else(|| tenure.as_ref().map(|tenure| tenure.repo.clone()));
        let session = params
            .session_id
            .as_deref()
            .filter(|session| !session.trim().is_empty())
            .or(pane_session);
        let command: Option<String> = params
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(|command| command.chars().take(COMMAND_MAX_CHARS).collect());
        let evidence = params
            .evidence
            .as_ref()
            .map(|evidence| self.keep_evidence(evidence))
            .transpose()?;
        let occurrence = NewOccurrence {
            kind,
            summary,
            normalized: &normalized,
            fingerprint: &fingerprint,
            uncertain: params.uncertain,
            repo: repo.as_deref(),
            pane_id,
            session_id: session,
            coordinator_id: tenure.as_ref().map(|tenure| tenure.coordinator_id.as_str()),
            command: command.as_deref(),
            evidence: evidence
                .as_ref()
                .map(|path| path.to_str().unwrap_or_default()),
            evidence_source: params
                .evidence
                .as_ref()
                .map(|evidence| evidence.source.as_str()),
            evidence_truncated: params
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.truncated),
        };
        let store = self.report_store()?;
        let (number, count, notify) = store
            .transaction(|tx| record_occurrence(tx, &occurrence, now_ms()))
            .map_err(store_error)?;
        let report = store
            .read(|conn| report_by_number(conn, number))
            .map_err(store_error)?
            .ok_or_else(|| WorkerError::ReportNotFound(id_of(number)))?;
        Ok(RecordedReport {
            report,
            occurrence: count,
            notify,
        })
    }

    /// Writes `evidence` to `reports/<hash>-<name>` in the worker directory
    /// (published by rename, so a reader never sees a half-written copy;
    /// the same text is kept once) and returns its path.
    fn keep_evidence(&self, evidence: &ReportEvidenceUpload) -> Result<PathBuf, WorkerError> {
        let mut content = evidence.content.as_str();
        let mut truncated = evidence.truncated;
        if content.len() > EVIDENCE_MAX_BYTES {
            let mut end = EVIDENCE_MAX_BYTES;
            while !content.is_char_boundary(end) {
                end -= 1;
            }
            content = &content[..end];
            truncated = true;
        }
        let name: String = Path::new(&evidence.source)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("evidence")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .take(60)
            .collect();
        let digest = Sha256::digest(content.as_bytes());
        let hash: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let dir = self.shared.dir.join("reports");
        let path = dir.join(format!("{hash}-{name}"));
        if path.exists() {
            return Ok(path);
        }
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let staged = dir.join(format!(".{hash}-{name}.{}.tmp", std::process::id()));
            let mut text = content.to_owned();
            if truncated {
                text.push_str(&format!(
                    "\n[herdr: cut to the first {} of {} bytes]\n",
                    content.len(),
                    evidence.bytes
                ));
            }
            std::fs::write(&staged, text)?;
            std::fs::rename(&staged, &path)
        };
        write().map_err(|error| {
            WorkerError::Io(std::io::Error::other(format!(
                "cannot keep the evidence {} in {}: {error}",
                evidence.source,
                dir.display()
            )))
        })?;
        Ok(path)
    }

    /// The reports, the latest occurrence first: the open ones, or all.
    pub(crate) fn reports(&self, all: bool) -> Result<Vec<ReportInfo>, WorkerError> {
        self.report_store()?
            .read(|conn| list_reports(conn, all))
            .map_err(store_error)
    }

    /// The active tenure of the herdr repository, if one is active.
    pub(crate) fn herdr_coordinator(&self) -> Option<CoordinatorInfo> {
        match self.coordinator_status(None) {
            Ok(tenures) => tenures
                .into_iter()
                .find(|tenure| is_herdr_repository(&tenure.repo)),
            Err(error) => {
                warn!(%error, "cannot read the coordination tenures for a report");
                None
            }
        }
    }

    /// Closes report `report_id` with `fix` (a TODO item or commit) or as
    /// not reproducible: only from `pane_id` (its public id) while it holds
    /// the herdr repository's coordination tenure. Closing a closed report
    /// is refused, naming its closure.
    pub(crate) fn report_close(
        &self,
        report_id: &str,
        pane_id: Option<&str>,
        fix: Option<&str>,
        not_reproducible: bool,
    ) -> Result<ReportInfo, WorkerError> {
        let fix = fix.map(str::trim).filter(|fix| !fix.is_empty());
        match (fix, not_reproducible) {
            (Some(_), true) | (None, false) => {
                return Err(WorkerError::Invalid(
                    "close takes either a fix (a TODO item or commit) or not_reproducible".into(),
                ))
            }
            (Some(fix), false) if !is_fix(fix) => {
                return Err(WorkerError::Invalid(format!(
                    "fix must be a TODO item id (t-...) or a commit (7 to 40 hex digits), not \
                     {fix:?}"
                )))
            }
            _ => {}
        }
        let number = report_number(report_id)
            .ok_or_else(|| WorkerError::ReportNotFound(format!("no report {report_id}")))?;
        let Some(pane) = pane_id else {
            return Err(WorkerError::ReportCloseRefused(
                "report close runs in the herdr coordinator's pane: HERDR_PANE_ID is not set"
                    .into(),
            ));
        };
        let tenure = self.coordinator_of_pane(pane).ok_or_else(|| {
            WorkerError::ReportCloseRefused(format!(
                "only the herdr repository's coordinator closes reports; pane {pane} holds no \
                 coordination tenure"
            ))
        })?;
        if !is_herdr_repository(&tenure.repo) {
            return Err(WorkerError::ReportCloseRefused(format!(
                "only the herdr repository's coordinator closes reports; pane {pane} \
                 coordinates {} ({})",
                tenure.repo, tenure.coordinator_id
            )));
        }
        let store = self.report_store()?;
        let closed = store
            .transaction(|tx| {
                close_report(
                    tx,
                    number,
                    fix,
                    not_reproducible,
                    &tenure.coordinator_id,
                    now_ms(),
                )
            })
            .map_err(store_error)?;
        match closed {
            Closed::NotFound => Err(WorkerError::ReportNotFound(format!(
                "no report {}",
                id_of(number)
            ))),
            Closed::Already(closure) => Err(WorkerError::Invalid(format!(
                "report {} is closed already ({closure})",
                id_of(number)
            ))),
            Closed::Done => store
                .read(|conn| report_by_number(conn, number))
                .map_err(store_error)?
                .ok_or_else(|| WorkerError::ReportNotFound(id_of(number))),
        }
    }
}

fn report_event(
    tx: &Tx<'_>,
    number: i64,
    event: &serde_json::Value,
    at_ms: u64,
) -> StoreResult<i64> {
    tx.event(&EventRow {
        worker_id: &event_key(number),
        direction: Direction::Herdr,
        record: &Recorded::Event(event),
        ts_ms: at_ms,
    })
}

/// Adds the occurrence to its fingerprint's report (a new one for a new
/// fingerprint), reopening a closed report, with its `report_recorded`
/// event. Returns the report's number, the occurrence's number and whether
/// it notifies.
fn record_occurrence(
    tx: &Tx<'_>,
    occurrence: &NewOccurrence<'_>,
    at_ms: u64,
) -> StoreResult<(i64, u32, bool)> {
    let conn = tx.connection();
    let existing: Option<(i64, String, u32, Option<String>, bool)> = conn
        .query_row(
            "SELECT id, state, count, fix, not_reproducible FROM reports WHERE fingerprint = ?1",
            [occurrence.fingerprint],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (number, count, reopened) = match existing {
        None => {
            conn.execute(
                "INSERT INTO reports (fingerprint, kind, summary, normalized, state, uncertain,
                     first_ms, last_ms, count)
                 VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6, ?6, 1)",
                params![
                    occurrence.fingerprint,
                    occurrence.kind,
                    occurrence.summary,
                    occurrence.normalized,
                    occurrence.uncertain,
                    at_ms as i64
                ],
            )?;
            (conn.last_insert_rowid(), 1, None)
        }
        Some((number, state, count, fix, not_reproducible)) => {
            let reopened =
                (state == "closed").then(|| closure_text(fix.as_deref(), not_reproducible));
            conn.execute(
                "UPDATE reports SET count = count + 1, last_ms = ?2,
                     uncertain = uncertain AND ?3, state = 'open',
                     reopened = reopened + ?4,
                     closed_ms = CASE WHEN ?4 THEN NULL ELSE closed_ms END,
                     closed_by = CASE WHEN ?4 THEN NULL ELSE closed_by END,
                     fix = CASE WHEN ?4 THEN NULL ELSE fix END,
                     not_reproducible = CASE WHEN ?4 THEN 0 ELSE not_reproducible END
                 WHERE id = ?1",
                params![
                    number,
                    at_ms as i64,
                    occurrence.uncertain,
                    reopened.is_some()
                ],
            )?;
            (number, count + 1, reopened)
        }
    };
    conn.execute(
        "INSERT INTO report_occurrences (report_id, number, ts, summary, uncertain, repo, pane_id,
             session_id, coordinator_id, command, evidence, evidence_source, evidence_truncated,
             reopened)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            number,
            count,
            at_ms as i64,
            occurrence.summary,
            occurrence.uncertain,
            occurrence.repo,
            occurrence.pane_id,
            occurrence.session_id,
            occurrence.coordinator_id,
            occurrence.command,
            occurrence.evidence,
            occurrence.evidence_source,
            occurrence.evidence_truncated,
            reopened
        ],
    )?;
    let event = json!({
        "type": "report_recorded",
        "report_id": id_of(number),
        "occurrence": count,
        "kind": occurrence.kind,
        "summary": occurrence.summary,
        "fingerprint": occurrence.fingerprint,
        "uncertain": occurrence.uncertain,
        "repo": occurrence.repo,
        "pane_id": occurrence.pane_id,
        "session_id": occurrence.session_id,
        "coordinator_id": occurrence.coordinator_id,
        "command": occurrence.command,
        "evidence": occurrence.evidence,
        "reopened": reopened,
    });
    report_event(tx, number, &event, at_ms)?;
    let notify = count == 1 || reopened.is_some();
    Ok((number, count, notify))
}

enum Closed {
    Done,
    NotFound,
    /// It was closed already: with what.
    Already(String),
}

fn close_report(
    tx: &Tx<'_>,
    number: i64,
    fix: Option<&str>,
    not_reproducible: bool,
    closed_by: &str,
    at_ms: u64,
) -> StoreResult<Closed> {
    let conn = tx.connection();
    let Some((state, old_fix, old_not_reproducible)) = conn
        .query_row(
            "SELECT state, fix, not_reproducible FROM reports WHERE id = ?1",
            [number],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(Closed::NotFound);
    };
    if state == "closed" {
        return Ok(Closed::Already(closure_text(
            old_fix.as_deref(),
            old_not_reproducible,
        )));
    }
    conn.execute(
        "UPDATE reports SET state = 'closed', closed_ms = ?2, closed_by = ?3, fix = ?4,
             not_reproducible = ?5
         WHERE id = ?1",
        params![number, at_ms as i64, closed_by, fix, not_reproducible],
    )?;
    let event = json!({
        "type": "report_closed",
        "report_id": id_of(number),
        "closed_by": closed_by,
        "fix": fix,
        "not_reproducible": not_reproducible,
    });
    report_event(tx, number, &event, at_ms)?;
    Ok(Closed::Done)
}

const REPORT_COLUMNS: &str = "id, kind, summary, normalized, fingerprint, state, uncertain, \
    first_ms, last_ms, count, reopened, closed_ms, closed_by, fix, not_reproducible";

fn report_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReportInfo> {
    Ok(ReportInfo {
        report_id: id_of(row.get(0)?),
        kind: row.get(1)?,
        summary: row.get(2)?,
        normalized: row.get(3)?,
        fingerprint: row.get(4)?,
        state: match row.get::<_, String>(5)?.as_str() {
            "open" => ReportState::Open,
            "closed" => ReportState::Closed,
            _ => ReportState::Unknown,
        },
        uncertain: row.get(6)?,
        first_ms: row.get::<_, i64>(7)? as u64,
        last_ms: row.get::<_, i64>(8)? as u64,
        count: row.get(9)?,
        reopened: row.get(10)?,
        closed_ms: row.get::<_, Option<i64>>(11)?.map(|ms| ms as u64),
        closed_by: row.get(12)?,
        fix: row.get(13)?,
        not_reproducible: row.get(14)?,
        occurrences: Vec::new(),
    })
}

fn occurrences_of(conn: &Connection, number: i64) -> StoreResult<Vec<ReportOccurrence>> {
    let mut statement = conn.prepare(
        "SELECT number, ts, summary, uncertain, repo, pane_id, session_id, coordinator_id,
             command, evidence, evidence_source, evidence_truncated, reopened
         FROM report_occurrences WHERE report_id = ?1 ORDER BY number",
    )?;
    let rows = statement.query_map([number], |row| {
        Ok(ReportOccurrence {
            number: row.get(0)?,
            ts_ms: row.get::<_, i64>(1)? as u64,
            summary: row.get(2)?,
            uncertain: row.get(3)?,
            repo: row.get(4)?,
            pane_id: row.get(5)?,
            session_id: row.get(6)?,
            coordinator_id: row.get(7)?,
            command: row.get(8)?,
            evidence: row.get(9)?,
            evidence_source: row.get(10)?,
            evidence_truncated: row.get(11)?,
            reopened: row.get(12)?,
        })
    })?;
    rows.collect()
}

fn report_by_number(conn: &Connection, number: i64) -> StoreResult<Option<ReportInfo>> {
    let report = conn
        .query_row(
            &format!("SELECT {REPORT_COLUMNS} FROM reports WHERE id = ?1"),
            [number],
            report_from_row,
        )
        .optional()?;
    report
        .map(|mut report| {
            report.occurrences = occurrences_of(conn, number)?;
            Ok(report)
        })
        .transpose()
}

fn list_reports(conn: &Connection, all: bool) -> StoreResult<Vec<ReportInfo>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {REPORT_COLUMNS} FROM reports WHERE ?1 OR state = 'open'
         ORDER BY last_ms DESC, id DESC"
    ))?;
    let reports: Vec<ReportInfo> = statement
        .query_map([all], report_from_row)?
        .collect::<StoreResult<_>>()?;
    reports
        .into_iter()
        .map(|mut report| {
            let number = report_number(&report.report_id).unwrap_or_default();
            report.occurrences = occurrences_of(conn, number)?;
            Ok(report)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, WorkerSupervisor) {
        let root = std::env::temp_dir().join(format!(
            "herdr-reports-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        (root, supervisor)
    }

    /// A directory whose `Cargo.toml` names the package `herdr`.
    fn herdr_checkout(root: &Path) -> String {
        let dir = root.join("herdr");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"herdr\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        dir.display().to_string()
    }

    fn params(kind: &str, summary: &str) -> ReportRecordParams {
        ReportRecordParams {
            kind: kind.into(),
            summary: summary.into(),
            ..ReportRecordParams::default()
        }
    }

    #[test]
    fn the_normalizer_strips_ids_paths_numbers_and_hashes() {
        assert_eq!(
            normalize_summary(
                "  todo wait r-yp27jex3 gave up after 3 tries in /Users/me/repo/TODO.md, \
                 commit 1a2b3c4d5e.  Worker w12 (t-pulsqoxl) EXITED 8."
            ),
            "todo wait <id> gave up after <n> tries in <path>, commit <sha>. worker <id> \
             (<id>) exited <n>."
        );
        // Two reports of one problem from other runs, repositories and
        // workers normalize alike; another problem does not.
        let one = normalize_summary("`herdr worker wait w3` returned lost for ~/work/a");
        let two = normalize_summary("`herdr worker wait w41` returned lost for ./b/c");
        assert_eq!(one, two);
        assert_eq!(one, "`herdr worker wait <id>` returned lost for <path>");
        assert_ne!(
            one,
            normalize_summary("`herdr worker wait w3` returned done")
        );
        // Words without digits stay, lowercased; versions are numbers.
        assert_eq!(
            normalize_summary("Skill step needs v0.5.1 of claude-code"),
            "skill step needs <id> of claude-code"
        );
        assert_eq!(normalize_summary("exit 0.5.1"), "exit <n>");
        assert_eq!(
            fingerprint("wait", &normalize_summary("w1 lost")),
            fingerprint("wait", &normalize_summary("W9  lost"))
        );
        assert_ne!(
            fingerprint("wait", "x"),
            fingerprint("sandbox", "x"),
            "the kind is part of the fingerprint"
        );
        assert_eq!(fingerprint("wait", "x").len(), 16);
    }

    #[test]
    fn repeats_of_a_fingerprint_add_occurrences_and_notify_once() {
        let (root, supervisor) = scratch("dedup");
        let first = supervisor
            .report_record(
                &ReportRecordParams {
                    command: Some("herdr todo wait r-abcd2345 --after 7".into()),
                    session_id: Some("s-1".into()),
                    cwd: Some("/nowhere".into()),
                    ..params("todo-wait", "todo wait r-abcd2345 gave up after 3 tries")
                },
                Some("p1"),
                None,
                None,
            )
            .unwrap();
        assert_eq!((first.occurrence, first.notify), (1, true));
        assert_eq!(first.report.report_id, "r-1");
        assert_eq!(first.report.state, ReportState::Open);
        let second = supervisor
            .report_record(
                &params("todo-wait", "todo wait r-zzzz7777 gave up after 5 tries"),
                Some("p2"),
                Some("s-2"),
                None,
            )
            .unwrap();
        assert_eq!(
            (
                second.report.report_id.as_str(),
                second.occurrence,
                second.notify
            ),
            ("r-1", 2, false)
        );
        assert_eq!(second.report.count, 2);
        // The report keeps its first summary; each occurrence its own, with
        // its pane, session and command.
        assert_eq!(
            second.report.summary,
            "todo wait r-abcd2345 gave up after 3 tries"
        );
        let occurrences = &second.report.occurrences;
        assert_eq!(occurrences.len(), 2);
        assert_eq!(
            occurrences[0].command.as_deref(),
            Some("herdr todo wait r-abcd2345 --after 7")
        );
        assert_eq!(
            (
                occurrences[1].pane_id.as_deref(),
                occurrences[1].session_id.as_deref()
            ),
            (Some("p2"), Some("s-2"))
        );
        // Another kind is another report, notified again.
        let other = supervisor
            .report_record(
                &params("sandbox", "todo wait r-abcd2345 gave up after 3 tries"),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            (other.report.report_id.as_str(), other.notify),
            ("r-2", true)
        );
        assert_eq!(supervisor.reports(false).unwrap().len(), 2);
        // The events are in the store's event log.
        let types: Vec<String> = supervisor
            .report_store()
            .unwrap()
            .connection()
            .prepare("SELECT type FROM events WHERE worker_id = 'report:r-1' ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(types, ["report_recorded", "report_recorded"]);
        for bad in [
            params("Bad Kind", "x"),
            params("ok", " "),
            params("ok", "two\nlines"),
        ] {
            assert_eq!(
                supervisor
                    .report_record(&bad, None, None, None)
                    .unwrap_err()
                    .code(),
                "invalid_request"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn evidence_is_copied_and_bounded() {
        let (root, supervisor) = scratch("evidence");
        let source = root.join("tmp-output.log");
        let recorded = supervisor
            .report_record(
                &ReportRecordParams {
                    evidence: Some(ReportEvidenceUpload {
                        source: source.display().to_string(),
                        content: "é".repeat(EVIDENCE_MAX_BYTES),
                        bytes: 2 * EVIDENCE_MAX_BYTES as u64,
                        truncated: false,
                    }),
                    ..params("wait", "worker wait ended without a verdict")
                },
                None,
                None,
                None,
            )
            .unwrap();
        let occurrence = &recorded.report.occurrences[0];
        assert!(occurrence.evidence_truncated || occurrence.evidence.is_some());
        let kept = occurrence.evidence.as_deref().unwrap();
        assert!(kept.starts_with(&root.join("workers/reports").display().to_string()));
        assert!(kept.ends_with("-tmp-output.log"), "{kept}");
        let text = std::fs::read_to_string(kept).unwrap();
        assert!(text.len() <= EVIDENCE_MAX_BYTES + 100, "{}", text.len());
        assert!(
            text.contains("[herdr: cut to the first"),
            "the cut is marked"
        );
        assert_eq!(
            occurrence.evidence_source.as_deref(),
            Some(source.display().to_string().as_str())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn only_the_herdr_coordinator_closes_and_a_later_occurrence_reopens() {
        let (root, supervisor) = scratch("close");
        let herdr = herdr_checkout(&root);
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let other = other.display().to_string();
        let recorded = supervisor
            .report_record(&params("wait", "w1 lost"), None, None, None)
            .unwrap();
        let id = recorded.report.report_id.clone();

        // Without a pane, from a pane without a tenure, and from another
        // repository's coordinator: refused.
        for pane in [None, Some("p9")] {
            let refused = supervisor
                .report_close(&id, pane, Some("t-abcd2345"), false)
                .unwrap_err();
            assert_eq!(refused.code(), "report_close_refused", "{pane:?}");
        }
        supervisor.coordinator_start(&other, "p2", None).unwrap();
        let refused = supervisor
            .report_close(&id, Some("p2"), Some("t-abcd2345"), false)
            .unwrap_err();
        assert_eq!(refused.code(), "report_close_refused");
        assert!(refused.to_string().contains(&other), "{refused}");
        assert_eq!(supervisor.herdr_coordinator(), None);

        // The herdr repository's coordinator closes it.
        let tenure = supervisor.coordinator_start(&herdr, "p1", None).unwrap();
        assert_eq!(
            supervisor
                .herdr_coordinator()
                .map(|coordinator| coordinator.coordinator_id),
            Some(tenure.coordinator_id.clone())
        );
        for (fix, not_reproducible) in [
            (None, false),
            (Some("t-abcd2345"), true),
            (Some("not a fix"), false),
        ] {
            assert_eq!(
                supervisor
                    .report_close(&id, Some("p1"), fix, not_reproducible)
                    .unwrap_err()
                    .code(),
                "invalid_request",
                "{fix:?} {not_reproducible}"
            );
        }
        assert_eq!(
            supervisor
                .report_close("r-99", Some("p1"), Some("abc1234"), false)
                .unwrap_err()
                .code(),
            "report_not_found"
        );
        let closed = supervisor
            .report_close(&id, Some("p1"), Some("abc1234"), false)
            .unwrap();
        assert_eq!(closed.state, ReportState::Closed);
        assert_eq!(closed.fix.as_deref(), Some("abc1234"));
        assert_eq!(
            closed.closed_by.as_deref(),
            Some(tenure.coordinator_id.as_str())
        );
        assert!(supervisor.reports(false).unwrap().is_empty());
        assert_eq!(supervisor.reports(true).unwrap().len(), 1);
        assert_eq!(
            supervisor
                .report_close(&id, Some("p1"), None, true)
                .unwrap_err()
                .code(),
            "invalid_request",
            "a closed report is not closed again"
        );

        // A later occurrence reopens it and notifies again, naming the fix
        // it reopened.
        let again = supervisor
            .report_record(&params("wait", "w7 lost"), None, None, None)
            .unwrap();
        assert_eq!((again.occurrence, again.notify), (2, true));
        assert_eq!(again.report.state, ReportState::Open);
        assert_eq!(again.report.reopened, 1);
        assert_eq!(again.report.fix, None);
        assert_eq!(
            again.report.occurrences[1].reopened.as_deref(),
            Some("fix abc1234")
        );
        let notice = report_notice(&again, Some(&tenure)).unwrap();
        assert_eq!(notice.title, format!("Herdr report {id} reopened"));
        // A repeat of the open report does not.
        let repeat = supervisor
            .report_record(&params("wait", "w8 lost"), None, None, None)
            .unwrap();
        assert!(!repeat.notify);
        assert_eq!(report_notice(&repeat, Some(&tenure)), None);
        // Closed as not reproducible.
        let closed = supervisor
            .report_close(&id, Some("p1"), None, true)
            .unwrap();
        assert!(closed.not_reproducible);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_report_notifies_the_herdr_coordinators_pane_else_the_user() {
        let (root, supervisor) = scratch("notice");
        let recorded = supervisor
            .report_record(
                &ReportRecordParams {
                    uncertain: true,
                    ..params("skill", "the todo skill names a herdr-only path")
                },
                None,
                None,
                None,
            )
            .unwrap();
        assert!(recorded.report.uncertain);
        let notice = report_notice(&recorded, None).unwrap();
        assert_eq!(notice.notified, "user");
        assert_eq!(notice.pane_id, None);
        assert_eq!(notice.title, "Herdr report r-1 (no herdr coordinator)");
        assert_eq!(
            notice.body,
            "skill: the todo skill names a herdr-only path (uncertain)"
        );
        let herdr = herdr_checkout(&root);
        let tenure = supervisor.coordinator_start(&herdr, "p1", None).unwrap();
        let notice = report_notice(&recorded, Some(&tenure)).unwrap();
        assert_eq!(notice.pane_id.as_deref(), Some("p1"));
        assert_eq!(
            notice.notified,
            format!("coordinator {}", tenure.coordinator_id)
        );
        assert_eq!(notice.title, "Herdr report r-1");
        assert!(is_herdr_repository(&herdr));
        assert!(!is_herdr_repository(&root.display().to_string()));
        let _ = std::fs::remove_dir_all(root);
    }
}
