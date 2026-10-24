//! The server's persistent usage ledger: the tokens each agent session's responses used, read
//! from the transcripts the agents write (`super::transcript`), one row per response.
//!
//! A row's key is its session, agent kind, transcript and the offset of its line in that
//! transcript, and a cursor per transcript keeps how far it was read: each booking reads only
//! the complete lines appended since and writes their rows and the moved cursor in one
//! transaction, so reading the same lines again, a restart or a live handoff never counts a
//! response twice and never loses one. A transcript that got shorter, or whose bytes before
//! the cursor changed, was rewritten: it is read again from its start under a new epoch, and a
//! response the agent names (Claude's message and request ids, pi's entry id, Codex's running
//! total at its time) that is already booked is not booked again. Rows stay after their panes
//! close and their sessions are forgotten.
//!
//! The server books only at the named events that produce usage: server start (every stored
//! cursor whose transcript changed, then every pane's session), an agent's turn end or change
//! to idle, a session start (the previous session's last responses) and an agent's or pane's
//! exit. Never on a timer and never in render. A write that finds the database locked by
//! another server (a live handoff overlaps two) fails at once and is logged; its cursor did
//! not move, so the next of these events books the same lines.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::transcript::{self, CodexTotals, LineUsage, Reader};
use crate::api::schema::{
    UsageLedgerAgentTokens, UsageLedgerDay, UsageLedgerModel, UsageLedgerReport, UsageLedgerSpace,
    UsageLedgerTokens, UsageLedgerWindow,
};

/// The database file, next to the server's other state.
pub(crate) const LEDGER_FILE: &str = "usage-ledger.sqlite3";

/// What every row records as its source.
const SOURCE: &str = "transcript";

/// Bytes before a cursor's offset that must still read the same, or the transcript was
/// rewritten.
const FINGERPRINT_BYTES: u64 = 64;

/// The rolling windows the aggregates cover, by id and length in seconds.
pub(crate) const WINDOWS: &[(&str, u64)] = &[
    ("1h", 3_600),
    ("6h", 6 * 3_600),
    ("24h", 24 * 3_600),
    ("7d", 7 * 24 * 3_600),
];

/// Schema migrations, in order; `meta.schema_version` counts how many have run. Never edit
/// one that has shipped: append a new one.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE cursors (
    agent TEXT NOT NULL,
    session_id TEXT NOT NULL,
    transcript TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    fingerprint INTEGER NOT NULL,
    codex_model TEXT,
    codex_totals TEXT,
    workspace_id TEXT,
    touched_ms INTEGER NOT NULL,
    PRIMARY KEY (agent, session_id, transcript)
);
CREATE TABLE entries (
    agent TEXT NOT NULL,
    session_id TEXT NOT NULL,
    transcript TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    response_key TEXT,
    model TEXT NOT NULL,
    workspace_id TEXT,
    day TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    source TEXT NOT NULL,
    booked_ms INTEGER NOT NULL,
    PRIMARY KEY (agent, session_id, transcript, epoch, offset)
);
CREATE UNIQUE INDEX entries_response ON entries (agent, response_key)
    WHERE response_key IS NOT NULL;
CREATE INDEX entries_space_day ON entries (workspace_id, day);
CREATE INDEX entries_at ON entries (at_ms);
"#];

/// How far one transcript of a session was read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cursor {
    agent: String,
    session_id: String,
    transcript: PathBuf,
    /// Counts the rewrites seen: a row's offset belongs to one epoch of the file.
    epoch: i64,
    /// Where the next read starts: right after the last complete line read.
    offset: u64,
    /// FNV-1a of the bytes before `offset` (at most [`FINGERPRINT_BYTES`]).
    fingerprint: u64,
    /// Codex's running total as of `offset`.
    codex: CodexTotals,
    workspace_id: Option<String>,
    touched_ms: u64,
}

impl Cursor {
    fn new(agent: &str, session_id: &str, transcript: &Path) -> Self {
        Self {
            agent: agent.into(),
            session_id: session_id.into(),
            transcript: transcript.into(),
            epoch: 0,
            offset: 0,
            fingerprint: fingerprint_of(&[]),
            codex: CodexTotals::default(),
            workspace_id: None,
            touched_ms: 0,
        }
    }
}

/// One response read from a transcript, not yet booked.
#[derive(Debug, Clone)]
struct Entry {
    offset: u64,
    usage: LineUsage,
}

/// The ledger, kept in SQLite; in memory when the server persists nothing (tests) or the
/// file cannot be opened.
pub(crate) struct UsageLedger {
    conn: Option<Connection>,
    /// Seconds east of UTC at a Unix time, for the local day of a response.
    local_offset: fn(i64) -> i64,
}

impl std::fmt::Debug for UsageLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageLedger")
            .field("open", &self.conn.is_some())
            .finish()
    }
}

fn platform_offset(at: i64) -> i64 {
    crate::platform::local_utc_offset_at(at).unwrap_or(0)
}

impl UsageLedger {
    /// The ledger in `path`, created (with its coverage start `now_ms`) when it has none; in
    /// memory without a path.
    pub(crate) fn open(path: Option<&Path>, now_ms: u64) -> Self {
        Self::open_with(path, now_ms, platform_offset)
    }

    fn open_with(path: Option<&Path>, now_ms: u64, local_offset: fn(i64) -> i64) -> Self {
        let opened = match path {
            Some(path) => open_file(path, now_ms).or_else(|error| {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "cannot open the usage ledger; recording in memory"
                );
                open_memory(now_ms)
            }),
            None => open_memory(now_ms),
        };
        let conn = match opened {
            Ok(conn) => Some(conn),
            Err(error) => {
                tracing::warn!(%error, "cannot open an in-memory usage ledger");
                None
            }
        };
        Self { conn, local_offset }
    }

    /// Unix milliseconds since which the ledger records.
    pub(crate) fn coverage_start_ms(&self) -> Option<u64> {
        let conn = self.conn.as_ref()?;
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'coverage_start_ms'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|value| value.parse().ok())
    }

    /// The transcripts already read for `agent`'s session, main one first.
    pub(crate) fn stored_transcripts(&self, agent: &str, session_id: &str) -> Vec<PathBuf> {
        let Some(conn) = self.conn.as_ref() else {
            return Vec::new();
        };
        let result = (|| -> rusqlite::Result<Vec<PathBuf>> {
            let mut statement = conn.prepare(
                "SELECT transcript FROM cursors WHERE agent = ?1 AND session_id = ?2
                 ORDER BY rowid",
            )?;
            let rows =
                statement.query_map(params![agent, session_id], |row| row.get::<_, String>(0))?;
            rows.map(|row| row.map(PathBuf::from)).collect()
        })();
        result.unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot read the usage ledger's cursors");
            Vec::new()
        })
    }

    /// Books what `agent`'s session appended to each of `transcripts` since its cursor, under
    /// `workspace_id`. Returns how many responses were booked.
    pub(crate) fn book_session(
        &mut self,
        agent: &str,
        session_id: &str,
        workspace_id: Option<&str>,
        transcripts: &[PathBuf],
        now_ms: u64,
    ) -> usize {
        let Some(reader) = transcript::reader_for(agent) else {
            return 0;
        };
        if session_id.is_empty() {
            return 0;
        }
        let mut booked = 0;
        for path in transcripts {
            let cursor = match self.cursor(agent, session_id, path) {
                Ok(cursor) => cursor.unwrap_or_else(|| Cursor::new(agent, session_id, path)),
                Err(error) => {
                    tracing::warn!(%error, "cannot read the usage ledger's cursor");
                    continue;
                }
            };
            booked += self.advance(reader, cursor, workspace_id.map(str::to_string), now_ms);
        }
        booked
    }

    /// Server start: every stored cursor whose transcript changed since it was read goes on
    /// from its offset, under the space it was last booked in, so a session no pane shows any
    /// more still has its last responses booked. A transcript of the length read and not
    /// modified since costs one `stat`.
    pub(crate) fn catch_up(&mut self, now_ms: u64) -> usize {
        let cursors = match self.all_cursors() {
            Ok(cursors) => cursors,
            Err(error) => {
                tracing::warn!(%error, "cannot read the usage ledger's cursors");
                return 0;
            }
        };
        let mut booked = 0;
        for cursor in cursors {
            let Some(reader) = transcript::reader_for(&cursor.agent) else {
                continue;
            };
            let Ok(metadata) = std::fs::metadata(&cursor.transcript) else {
                continue;
            };
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_millis() as u64);
            if metadata.len() == cursor.offset
                && modified_ms.is_some_and(|modified| modified <= cursor.touched_ms)
            {
                continue;
            }
            let workspace_id = cursor.workspace_id.clone();
            booked += self.advance(reader, cursor, workspace_id, now_ms);
        }
        booked
    }

    fn cursor(
        &self,
        agent: &str,
        session_id: &str,
        transcript: &Path,
    ) -> rusqlite::Result<Option<Cursor>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(None);
        };
        conn.query_row(
            "SELECT agent, session_id, transcript, epoch, offset, fingerprint, codex_model,
                    codex_totals, workspace_id, touched_ms
             FROM cursors WHERE agent = ?1 AND session_id = ?2 AND transcript = ?3",
            params![agent, session_id, transcript.to_string_lossy()],
            cursor_from_row,
        )
        .optional()
    }

    fn all_cursors(&self) -> rusqlite::Result<Vec<Cursor>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(Vec::new());
        };
        let mut statement = conn.prepare(
            "SELECT agent, session_id, transcript, epoch, offset, fingerprint, codex_model,
                    codex_totals, workspace_id, touched_ms
             FROM cursors ORDER BY rowid",
        )?;
        let rows = statement.query_map([], cursor_from_row)?;
        rows.collect()
    }

    /// Reads the complete lines appended since `cursor` and books them with the moved cursor
    /// in one transaction. A failure is logged and leaves the cursor where it was.
    fn advance(
        &mut self,
        reader: Reader,
        cursor: Cursor,
        workspace_id: Option<String>,
        now_ms: u64,
    ) -> usize {
        let path = cursor.transcript.clone();
        let Ok(mut file) = File::open(&path) else {
            // Not written yet, or gone for now: nothing new to read.
            return 0;
        };
        let read = (|| -> std::io::Result<(Cursor, Vec<Entry>)> {
            let len = file.metadata()?.len();
            let mut cursor = cursor;
            if len < cursor.offset || fingerprint(&mut file, cursor.offset)? != cursor.fingerprint {
                // Rewritten: read again from the start, as another epoch of the file.
                cursor.epoch += 1;
                cursor.offset = 0;
                cursor.fingerprint = fingerprint_of(&[]);
                cursor.codex = CodexTotals::default();
            }
            let entries = read_lines(reader, &mut file, &mut cursor)?;
            cursor.fingerprint = fingerprint(&mut file, cursor.offset)?;
            Ok((cursor, entries))
        })();
        let (mut next, entries) = match read {
            Ok(read) => read,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot read the agent's transcript");
                return 0;
            }
        };
        next.workspace_id = workspace_id.or(next.workspace_id);
        next.touched_ms = now_ms;
        match self.commit(reader, &next, &entries, now_ms) {
            Ok(booked) => booked,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot write the usage ledger");
                0
            }
        }
    }

    fn commit(
        &mut self,
        reader: Reader,
        cursor: &Cursor,
        entries: &[Entry],
        now_ms: u64,
    ) -> rusqlite::Result<usize> {
        let local_offset = self.local_offset;
        let Some(conn) = self.conn.as_mut() else {
            return Ok(0);
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transcript = cursor.transcript.to_string_lossy().into_owned();
        let mut booked = 0;
        for entry in entries {
            let usage = &entry.usage;
            let tokens = &usage.tokens;
            let at_ms = usage
                .timestamp
                .as_deref()
                .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok())
                .map(|time| (time.unix_timestamp_nanos() / 1_000_000).max(0) as u64)
                .unwrap_or(now_ms);
            let day = local_day(at_ms, local_offset);
            // pi names an entry within its session only.
            let response_key = usage.response_key.as_ref().map(|key| match reader {
                Reader::Pi => format!("{}:{key}", cursor.session_id),
                Reader::Claude | Reader::Codex => key.clone(),
            });
            if let Some(key) = response_key.as_deref() {
                // The response is booked already: from another line of it (Claude writes one
                // per content block), an earlier epoch of the file or a session it was copied
                // from. Its counts only ever grow to the largest a line reports.
                let merged = tx.execute(
                    "UPDATE entries SET input = max(input, ?3), output = max(output, ?4),
                         cache_read = max(cache_read, ?5), cache_write = max(cache_write, ?6)
                     WHERE agent = ?1 AND response_key = ?2",
                    params![
                        cursor.agent,
                        key,
                        tokens.input as i64,
                        tokens.output as i64,
                        tokens.cache_read as i64,
                        tokens.cache_write() as i64,
                    ],
                )?;
                if merged > 0 {
                    continue;
                }
            }
            booked += tx.execute(
                "INSERT OR IGNORE INTO entries (agent, session_id, transcript, epoch, offset,
                     response_key, model, workspace_id, day, at_ms, input, output, cache_read,
                     cache_write, source, booked_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    cursor.agent,
                    cursor.session_id,
                    transcript,
                    cursor.epoch,
                    entry.offset as i64,
                    response_key,
                    usage.model,
                    cursor.workspace_id,
                    day,
                    at_ms as i64,
                    tokens.input as i64,
                    tokens.output as i64,
                    tokens.cache_read as i64,
                    tokens.cache_write() as i64,
                    SOURCE,
                    now_ms as i64,
                ],
            )?;
        }
        let codex_totals = cursor
            .codex
            .previous
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        tx.execute(
            "INSERT INTO cursors (agent, session_id, transcript, epoch, offset, fingerprint,
                 codex_model, codex_totals, workspace_id, touched_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (agent, session_id, transcript) DO UPDATE SET
                 epoch = excluded.epoch, offset = excluded.offset,
                 fingerprint = excluded.fingerprint, codex_model = excluded.codex_model,
                 codex_totals = excluded.codex_totals, workspace_id = excluded.workspace_id,
                 touched_ms = excluded.touched_ms",
            params![
                cursor.agent,
                cursor.session_id,
                transcript,
                cursor.epoch,
                cursor.offset as i64,
                cursor.fingerprint as i64,
                cursor.codex.model,
                codex_totals,
                cursor.workspace_id,
                cursor.touched_ms as i64,
            ],
        )?;
        tx.commit()?;
        Ok(booked)
    }

    /// The aggregates per space (only `only`'s, when given; `labels` names the spaces the
    /// server lists, in their order) and, over every space, per rolling window ending at
    /// `now_ms`.
    pub(crate) fn report(
        &self,
        only: Option<&str>,
        labels: &[(String, String)],
        now_ms: u64,
    ) -> UsageLedgerReport {
        let mut report = UsageLedgerReport {
            coverage_start: self.coverage_start_ms().map(|ms| ms / 1_000),
            as_of: Some(now_ms / 1_000),
            spaces: Vec::new(),
            windows: Vec::new(),
        };
        let Some(conn) = self.conn.as_ref() else {
            return report;
        };
        match aggregate(conn, only, labels, now_ms) {
            Ok((spaces, windows)) => {
                report.spaces = spaces;
                report.windows = windows;
            }
            Err(error) => tracing::warn!(%error, "cannot read the usage ledger"),
        }
        report
    }
}

fn open_file(path: &Path, now_ms: u64) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(%error, dir = %dir.display(), "cannot create the usage ledger's directory");
        }
    }
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // In WAL mode a server crash loses nothing committed; an OS crash may lose the last
    // commits, never tear one, and their cursors go back with them.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    migrate(&mut conn, now_ms)?;
    Ok(conn)
}

fn open_memory(now_ms: u64) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    migrate(&mut conn, now_ms)?;
    Ok(conn)
}

fn migrate(conn: &mut Connection, now_ms: u64) -> rusqlite::Result<()> {
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
        // Written by a newer herdr: refused rather than misread.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "usage ledger schema version {version} is newer than this herdr's {}",
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
    // The coverage starts when the ledger is created and never moves.
    tx.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES ('coverage_start_ms', ?1)",
        [now_ms.to_string()],
    )?;
    tx.commit()
}

fn cursor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Cursor> {
    let totals: Option<String> = row.get(7)?;
    let mut previous = [0u64; 4];
    if let Some(totals) = totals {
        for (slot, value) in previous.iter_mut().zip(totals.split(',')) {
            *slot = value.parse().unwrap_or(0);
        }
    }
    let codex_model: Option<String> = row.get(6)?;
    Ok(Cursor {
        agent: row.get(0)?,
        session_id: row.get(1)?,
        transcript: PathBuf::from(row.get::<_, String>(2)?),
        epoch: row.get(3)?,
        offset: row.get::<_, i64>(4)?.max(0) as u64,
        fingerprint: row.get::<_, i64>(5)? as u64,
        codex: CodexTotals {
            model: codex_model.unwrap_or_else(|| CodexTotals::default().model),
            previous,
        },
        workspace_id: row.get(8)?,
        touched_ms: row.get::<_, i64>(9)?.max(0) as u64,
    })
}

/// Whether a line can hold usage, checked before it is parsed: most lines are tool results
/// and text.
fn may_hold_usage(reader: Reader, line: &[u8]) -> bool {
    let has = |needle: &[u8]| line.windows(needle.len()).any(|window| window == needle);
    match reader {
        Reader::Claude | Reader::Pi => has(b"\"usage\""),
        Reader::Codex => has(b"token_count") || has(b"turn_context"),
    }
}

/// Reads the complete lines from the cursor's offset to the end and moves the cursor past
/// them; a line still being written (no newline yet) is left for the next read.
fn read_lines(reader: Reader, file: &mut File, cursor: &mut Cursor) -> std::io::Result<Vec<Entry>> {
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut lines = BufReader::new(file);
    let mut entries = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = lines.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let offset = cursor.offset;
        cursor.offset += read as u64;
        if !may_hold_usage(reader, &line) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let usage = match reader {
            Reader::Claude => transcript::claude_line(&value),
            Reader::Pi => transcript::pi_line(&value),
            Reader::Codex => cursor.codex.line(&value),
        };
        if let Some(usage) = usage.filter(|usage| !usage.tokens.is_zero()) {
            entries.push(Entry { offset, usage });
        }
    }
    Ok(entries)
}

fn read_range(file: &mut File, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Vec::with_capacity((end - start) as usize);
    file.by_ref().take(end - start).read_to_end(&mut buffer)?;
    Ok(buffer)
}

fn fingerprint(file: &mut File, offset: u64) -> std::io::Result<u64> {
    let start = offset.saturating_sub(FINGERPRINT_BYTES);
    Ok(fingerprint_of(&read_range(file, start, offset)?))
}

/// FNV-1a: a cheap identity of a few bytes, not a security check.
fn fingerprint_of(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// `YYYY-MM-DD` of `at_ms` in the local time `local_offset` gives for it.
fn local_day(at_ms: u64, local_offset: fn(i64) -> i64) -> String {
    let secs = (at_ms / 1_000) as i64;
    let local = OffsetDateTime::from_unix_timestamp(secs.saturating_add(local_offset(secs)))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}",
        local.year(),
        u8::from(local.month()),
        local.day()
    )
}

fn add(sum: &mut UsageLedgerTokens, tokens: &UsageLedgerTokens) {
    sum.input += tokens.input;
    sum.output += tokens.output;
    sum.cache_read += tokens.cache_read;
    sum.cache_write += tokens.cache_write;
}

fn tokens_from(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<UsageLedgerTokens> {
    let column = |index: usize| -> rusqlite::Result<u64> {
        Ok(row.get::<_, Option<i64>>(index)?.unwrap_or(0).max(0) as u64)
    };
    Ok(UsageLedgerTokens {
        input: column(first)?,
        output: column(first + 1)?,
        cache_read: column(first + 2)?,
        cache_write: column(first + 3)?,
    })
}

fn add_model(
    models: &mut Vec<UsageLedgerModel>,
    agent: &str,
    model: &str,
    tokens: &UsageLedgerTokens,
) {
    match models
        .iter_mut()
        .find(|entry| entry.agent == agent && entry.model == model)
    {
        Some(entry) => add(&mut entry.tokens, tokens),
        None => models.push(UsageLedgerModel {
            agent: agent.into(),
            model: model.into(),
            tokens: tokens.clone(),
        }),
    }
}

fn add_agent(agents: &mut Vec<UsageLedgerAgentTokens>, agent: &str, tokens: &UsageLedgerTokens) {
    match agents.iter_mut().find(|entry| entry.agent == agent) {
        Some(entry) => add(&mut entry.tokens, tokens),
        None => agents.push(UsageLedgerAgentTokens {
            agent: agent.into(),
            tokens: tokens.clone(),
        }),
    }
}

fn empty_windows() -> Vec<UsageLedgerWindow> {
    WINDOWS
        .iter()
        .map(|(id, seconds)| UsageLedgerWindow {
            id: (*id).into(),
            seconds: *seconds,
            tokens: UsageLedgerTokens::default(),
            agents: Vec::new(),
        })
        .collect()
}

type Aggregates = (Vec<UsageLedgerSpace>, Vec<UsageLedgerWindow>);

fn aggregate(
    conn: &Connection,
    only: Option<&str>,
    labels: &[(String, String)],
    now_ms: u64,
) -> rusqlite::Result<Aggregates> {
    let mut spaces: BTreeMap<Option<String>, UsageLedgerSpace> = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT workspace_id, agent, model, day, sum(input), sum(output), sum(cache_read),
                sum(cache_write), min(at_ms)
         FROM entries WHERE ?1 IS NULL OR workspace_id = ?1
         GROUP BY workspace_id, agent, model, day ORDER BY day, agent, model",
    )?;
    let mut rows = statement.query(params![only])?;
    while let Some(row) = rows.next()? {
        let workspace_id: Option<String> = row.get(0)?;
        let agent: String = row.get(1)?;
        let model: String = row.get(2)?;
        let day: String = row.get(3)?;
        let tokens = tokens_from(row, 4)?;
        let first_at = row
            .get::<_, Option<i64>>(8)?
            .map(|ms| ms.max(0) as u64 / 1_000);
        let space = spaces
            .entry(workspace_id.clone())
            .or_insert_with(|| UsageLedgerSpace {
                workspace_id,
                windows: empty_windows(),
                ..UsageLedgerSpace::default()
            });
        add(&mut space.tokens, &tokens);
        space.first_at = match (space.first_at, first_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        add_model(&mut space.models, &agent, &model, &tokens);
        if space.days.last().map(|last| last.day.as_str()) != Some(day.as_str()) {
            space.days.push(UsageLedgerDay {
                day,
                ..UsageLedgerDay::default()
            });
        }
        if let Some(entry) = space.days.last_mut() {
            add(&mut entry.tokens, &tokens);
            add_model(&mut entry.models, &agent, &model, &tokens);
        }
    }
    drop(rows);

    let mut all = empty_windows();
    let mut statement = conn.prepare(
        "SELECT workspace_id, agent, sum(input), sum(output), sum(cache_read), sum(cache_write)
         FROM entries WHERE at_ms >= ?1 GROUP BY workspace_id, agent ORDER BY agent",
    )?;
    for (index, (_, seconds)) in WINDOWS.iter().enumerate() {
        let from = now_ms.saturating_sub(seconds * 1_000) as i64;
        let mut rows = statement.query(params![from])?;
        while let Some(row) = rows.next()? {
            let workspace_id: Option<String> = row.get(0)?;
            let agent: String = row.get(1)?;
            let tokens = tokens_from(row, 2)?;
            if let Some(window) = all.get_mut(index) {
                add(&mut window.tokens, &tokens);
                add_agent(&mut window.agents, &agent, &tokens);
            }
            if let Some(window) = spaces
                .get_mut(&workspace_id)
                .and_then(|space| space.windows.get_mut(index))
            {
                add(&mut window.tokens, &tokens);
                add_agent(&mut window.agents, &agent, &tokens);
            }
        }
    }

    // The spaces the server lists, in their order, then the others, then usage without one.
    let mut ordered = Vec::with_capacity(spaces.len());
    for (id, label) in labels {
        if let Some(mut space) = spaces.remove(&Some(id.clone())) {
            space.label = Some(label.clone());
            ordered.push(space);
        }
    }
    let without = spaces.remove(&None);
    ordered.extend(spaces.into_values());
    ordered.extend(without);
    Ok((ordered, all))
}

#[cfg(test)]
mod tests;
