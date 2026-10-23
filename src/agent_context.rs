//! The input context of an agent's last completed request, read from the transcript the
//! agent writes: how full its context window was as of that request.
//!
//! The server keeps one cursor per agent session (its transcript, how far it was read, and the
//! last reading), persisted next to its other state, so a restart or a live handoff goes on
//! where the old server stopped. It reconciles a session only at named events: server start,
//! the agent's session start, its turn end and its change to idle or done; never on a timer
//! and never in render. Each reconcile reads only what the agent appended since.
//!
//! A session start (`/clear`, a resume, a compaction, a new session in the pane) and a
//! rewritten transcript make the reading unknown until that session's first completed
//! request: unknown, never 0. Claude only for now.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::api::schema::AgentContext;

/// The file the cursors are kept in.
const STORE_FILE: &str = "agent-context.json";

/// Cursors kept: the most recently reconciled sessions.
const MAX_KEPT: usize = 64;

/// Bytes before a cursor's offset that must still read the same, or the transcript was
/// rewritten.
const FINGERPRINT_BYTES: u64 = 64;

/// The first chunk a backward search for the last reading reads; it doubles while one line
/// does not fit.
const BACKWARD_CHUNK: u64 = 256 * 1024;

/// Whether herdr reads this agent's transcripts for its context.
pub(crate) fn supports(agent: &str) -> bool {
    agent == "claude"
}

/// How far one session's transcript was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    agent: String,
    session_id: String,
    /// Not known yet: the agent creates it with its first message.
    #[serde(default)]
    transcript: Option<PathBuf>,
    /// Where the next read starts: right after the last complete line read.
    offset: u64,
    /// FNV-1a of the bytes before `offset` (at most [`FINGERPRINT_BYTES`]).
    fingerprint: u64,
    /// Unix milliseconds: requests recorded earlier belong to history the session started
    /// with (a resume copies it), not to the session. 0 when every line counts.
    since_ms: u64,
    #[serde(default)]
    reading: Option<AgentContext>,
    /// Unix milliseconds of the last reconcile, for pruning.
    touched_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    cursors: Vec<Cursor>,
}

/// The cursors of the sessions the server reconciled.
#[derive(Debug, Default)]
pub(crate) struct ContextStore {
    /// Where the cursors persist; `None` keeps them in memory (tests, a non-persisting app).
    dir: Option<PathBuf>,
    cursors: BTreeMap<String, Cursor>,
}

/// What made the server reconcile a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextEvent {
    /// The agent reported a session start (Claude's `SessionStart`): a new or cleared
    /// session, a resume or a compaction. The reading is unknown until its first request.
    SessionStart,
    /// Server start, a turn end, a change to idle or done: what the agent appended since.
    Reconcile,
}

fn key(agent: &str, session_id: &str) -> String {
    format!("{agent}:{session_id}")
}

impl ContextStore {
    /// The cursors persisted in `dir`, or none when it has no readable file.
    pub(crate) fn load(dir: Option<PathBuf>) -> Self {
        let cursors = dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(STORE_FILE)).ok())
            .and_then(|text| serde_json::from_str::<StoreFile>(&text).ok())
            .map(|file| {
                file.cursors
                    .into_iter()
                    .map(|cursor| (key(&cursor.agent, &cursor.session_id), cursor))
                    .collect()
            })
            .unwrap_or_default();
        Self { dir, cursors }
    }

    /// Where a server keeps them: the state directory, per named session.
    pub(crate) fn default_dir() -> PathBuf {
        let state_dir = crate::config::state_dir();
        match crate::session::active_name() {
            Some(name) => state_dir.join("sessions").join(name),
            None => state_dir,
        }
    }

    /// Reads what `agent`'s session `session_id` appended to its transcript since the last
    /// reconcile and returns its reading: the last completed request's input context, or
    /// `None` while unknown. `reported` is the transcript the agent's integration named;
    /// `locate` finds it otherwise, called only while the cursor has none.
    pub(crate) fn reconcile(
        &mut self,
        agent: &str,
        session_id: &str,
        reported: Option<&Path>,
        locate: impl FnOnce() -> Option<PathBuf>,
        event: ContextEvent,
        now_ms: u64,
    ) -> Option<AgentContext> {
        if !supports(agent) || session_id.is_empty() {
            return None;
        }
        let key = key(agent, session_id);
        let before = self.cursors.get(&key).cloned();
        let cursor = match (event, self.cursors.remove(&key)) {
            (ContextEvent::Reconcile, Some(mut cursor)) => {
                if let Some(reported) = reported {
                    if cursor.transcript.as_deref() != Some(reported) {
                        if cursor.transcript.is_some() {
                            // Another file now: what it holds is not what was read.
                            cursor = anchored(agent, session_id, Some(reported.into()), now_ms);
                        } else {
                            cursor.transcript = Some(reported.into());
                        }
                    }
                }
                if cursor.transcript.is_none() {
                    cursor.transcript = locate();
                }
                advance(cursor, now_ms)
            }
            (ContextEvent::SessionStart, _) => {
                let transcript = reported.map(Path::to_path_buf).or_else(locate);
                anchored(agent, session_id, transcript, now_ms)
            }
            (ContextEvent::Reconcile, None) => {
                // First sight outside a session start (a server start, an agent whose start
                // herdr missed): the transcript's last request is this session's.
                let transcript = reported.map(Path::to_path_buf).or_else(locate);
                first_sight(agent, session_id, transcript, now_ms)
            }
        };
        let reading = cursor.reading.clone();
        let changed = before.as_ref().is_none_or(|before| {
            before.offset != cursor.offset
                || before.transcript != cursor.transcript
                || before.reading != cursor.reading
                || before.since_ms != cursor.since_ms
        });
        self.cursors.insert(key, cursor);
        if changed {
            self.prune();
            self.save();
        }
        reading
    }

    fn prune(&mut self) {
        while self.cursors.len() > MAX_KEPT {
            let Some(oldest) = self
                .cursors
                .iter()
                .min_by_key(|(_, cursor)| cursor.touched_ms)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.cursors.remove(&oldest);
        }
    }

    /// Writes the cursors atomically (a temporary file renamed over the old one); a failure
    /// is logged and they stay in memory.
    fn save(&self) {
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        let file = StoreFile {
            cursors: self.cursors.values().cloned().collect(),
        };
        let path = dir.join(STORE_FILE);
        let result = (|| {
            std::fs::create_dir_all(dir)?;
            let text = serde_json::to_string(&file).map_err(std::io::Error::other)?;
            let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(error) = result {
            tracing::warn!(%error, path = %path.display(), "cannot store the agent context cursors");
        }
    }
}

/// A cursor at the transcript's end now: the reading is unknown until a request completes.
fn anchored(agent: &str, session_id: &str, transcript: Option<PathBuf>, now_ms: u64) -> Cursor {
    let (offset, fingerprint) = transcript
        .as_deref()
        .and_then(|path| File::open(path).ok())
        .and_then(|mut file| {
            let len = file.metadata().ok()?.len();
            let end = complete_end(&mut file, len).ok()?;
            Some((end, fingerprint(&mut file, end).ok()?))
        })
        .unwrap_or((0, fingerprint_of(&[])));
    Cursor {
        agent: agent.into(),
        session_id: session_id.into(),
        transcript,
        offset,
        fingerprint,
        since_ms: now_ms,
        reading: None,
        touched_ms: now_ms,
    }
}

/// A cursor for a session first seen with its transcript already written: its last request
/// is found by reading backwards from the end, without reading the whole file.
fn first_sight(agent: &str, session_id: &str, transcript: Option<PathBuf>, now_ms: u64) -> Cursor {
    let mut cursor = Cursor {
        agent: agent.into(),
        session_id: session_id.into(),
        transcript,
        offset: 0,
        fingerprint: fingerprint_of(&[]),
        since_ms: 0,
        reading: None,
        touched_ms: now_ms,
    };
    let Some(mut file) = cursor
        .transcript
        .as_deref()
        .and_then(|path| File::open(path).ok())
    else {
        return cursor;
    };
    let result = (|| -> std::io::Result<()> {
        let len = file.metadata()?.len();
        let end = complete_end(&mut file, len)?;
        cursor.reading = last_reading_before(&mut file, end, session_id)?;
        cursor.offset = end;
        cursor.fingerprint = fingerprint(&mut file, end)?;
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%error, session = session_id, "cannot read the agent's transcript");
    }
    cursor
}

/// Reads the complete lines appended since the cursor's offset. A transcript shorter than
/// the offset, or whose bytes before it changed, was rewritten: the cursor starts again at
/// its end, with the reading unknown.
fn advance(mut cursor: Cursor, now_ms: u64) -> Cursor {
    cursor.touched_ms = now_ms;
    let Some(path) = cursor.transcript.clone() else {
        return cursor;
    };
    let Ok(mut file) = File::open(&path) else {
        // Not written yet, or gone for now: nothing new to read.
        return cursor;
    };
    let result = (|| -> std::io::Result<Cursor> {
        let len = file.metadata()?.len();
        if len < cursor.offset || fingerprint(&mut file, cursor.offset)? != cursor.fingerprint {
            return Ok(anchored(
                &cursor.agent,
                &cursor.session_id,
                Some(path.clone()),
                now_ms,
            ));
        }
        if len == cursor.offset {
            return Ok(cursor.clone());
        }
        file.seek(SeekFrom::Start(cursor.offset))?;
        let mut appended = Vec::new();
        file.by_ref()
            .take(len - cursor.offset)
            .read_to_end(&mut appended)?;
        let Some(last_newline) = appended.iter().rposition(|byte| *byte == b'\n') else {
            // Only a line still being written.
            return Ok(cursor.clone());
        };
        let complete = &appended[..=last_newline];
        let mut next = cursor.clone();
        for line in complete.split(|byte| *byte == b'\n') {
            if let Some(reading) = claude_reading(line, &cursor.session_id, cursor.since_ms) {
                next.reading = Some(reading);
            }
        }
        next.offset = cursor.offset + complete.len() as u64;
        next.fingerprint = fingerprint(&mut file, next.offset)?;
        Ok(next)
    })();
    match result {
        Ok(next) => next,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "cannot read the agent's transcript");
            cursor
        }
    }
}

/// Where the transcript's last complete line ends: right after its last newline, or 0.
fn complete_end(file: &mut File, len: u64) -> std::io::Result<u64> {
    let mut end = len;
    while end > 0 {
        let start = end.saturating_sub(BACKWARD_CHUNK);
        let chunk = read_range(file, start, end)?;
        if let Some(at) = chunk.iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + at as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// The last reading in the complete lines before `end`, searched from the end backwards.
fn last_reading_before(
    file: &mut File,
    end: u64,
    session_id: &str,
) -> std::io::Result<Option<AgentContext>> {
    let mut end = end;
    let mut size = BACKWARD_CHUNK;
    while end > 0 {
        let start = end.saturating_sub(size);
        let chunk = read_range(file, start, end)?;
        // A line that starts before the chunk is read with the next one.
        let first = if start == 0 {
            0
        } else {
            match chunk.iter().position(|byte| *byte == b'\n') {
                Some(at) if at + 1 < chunk.len() => at + 1,
                _ => {
                    size = size.saturating_mul(2);
                    continue;
                }
            }
        };
        for line in chunk[first..].rsplit(|byte| *byte == b'\n') {
            if let Some(reading) = claude_reading(line, session_id, 0) {
                return Ok(Some(reading));
            }
        }
        end = start + first as u64;
    }
    Ok(None)
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

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The reading one Claude transcript line holds: an assistant message of the main agent with
/// its request's usage, whose input context is the sum of `input_tokens`,
/// `cache_creation_input_tokens` and `cache_read_input_tokens` (Claude's `input_tokens`
/// excludes the cache). Claude writes a
/// line per content block of a response, each with the response's usage, so the last one
/// stands for it. Lines without input usage (user lines, tool results, an output-only usage),
/// subagents' lines, synthetic messages and requests recorded before `since_ms` hold none.
fn claude_reading(line: &[u8], session_id: &str, since_ms: u64) -> Option<AgentContext> {
    // Most lines are tool results and text: skip them before parsing.
    if !line.windows(7).any(|window| window == b"\"usage\"") {
        return None;
    }
    let line: Value = serde_json::from_slice(line).ok()?;
    if line.get("type").and_then(Value::as_str) != Some("assistant")
        || line.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let message = line.get("message")?;
    let usage = message.get("usage")?;
    if message.get("model").and_then(Value::as_str) == Some("<synthetic>") {
        return None;
    }
    let tokens = number(usage, "input_tokens")
        + number(usage, "cache_creation_input_tokens")
        + number(usage, "cache_read_input_tokens");
    if tokens == 0 {
        return None;
    }
    let at = line
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    if since_ms > 0 {
        let recorded_ms = at
            .as_deref()
            .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok())
            .map(|time| (time.unix_timestamp_nanos() / 1_000_000).max(0) as u64);
        if recorded_ms.is_some_and(|recorded| recorded < since_ms) {
            return None;
        }
    }
    Some(AgentContext {
        last_request_input_tokens: tokens,
        session_id: session_id.to_string(),
        message_id: message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string),
        at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 2026-10-23T12:00:00Z.
    const NOON_MS: u64 = 1_792_756_800_000;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-agent-context-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assistant(id: &str, timestamp: &str, input: u64, write: u64, read: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": timestamp,
            "requestId": format!("req-{id}"),
            "message": {
                "id": id,
                "model": "claude-opus-5-5",
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": write,
                    "cache_read_input_tokens": read,
                    "output_tokens": 900,
                },
            },
        })
        .to_string()
    }

    fn tool_result(timestamp: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": timestamp,
            "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]},
        })
        .to_string()
    }

    fn output_only(id: &str, timestamp: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": timestamp,
            "message": {"id": id, "model": "claude-opus-5-5", "usage": {"output_tokens": 50}},
        })
        .to_string()
    }

    fn append(path: &Path, lines: &[String]) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn reconcile(
        store: &mut ContextStore,
        session: &str,
        path: &Path,
        event: ContextEvent,
    ) -> Option<AgentContext> {
        store.reconcile("claude", session, Some(path), || None, event, NOON_MS)
    }

    #[test]
    fn the_reading_sums_input_and_both_caches_of_the_last_request() {
        let dir = temp_dir("sum");
        let path = dir.join("s1.jsonl");
        append(
            &path,
            &[
                assistant("m1", "2026-10-23T11:00:00Z", 10, 1_000, 100_000),
                tool_result("2026-10-23T11:00:01Z"),
                assistant("m2", "2026-10-23T11:00:02Z", 3, 2_519, 418_000),
            ],
        );
        let mut store = ContextStore::load(None);
        let reading = reconcile(&mut store, "s1", &path, ContextEvent::Reconcile).unwrap();
        assert_eq!(reading.last_request_input_tokens, 420_522);
        assert_eq!(reading.session_id, "s1");
        assert_eq!(reading.message_id.as_deref(), Some("m2"));
        assert_eq!(reading.at.as_deref(), Some("2026-10-23T11:00:02Z"));
    }

    #[test]
    fn output_only_and_tool_result_lines_after_the_last_usage_keep_it() {
        let dir = temp_dir("tail");
        let path = dir.join("s1.jsonl");
        append(&path, &[assistant("m1", "2026-10-23T11:00:00Z", 5, 5, 90)]);
        let mut store = ContextStore::load(None);
        reconcile(&mut store, "s1", &path, ContextEvent::Reconcile);
        append(
            &path,
            &[
                output_only("m2", "2026-10-23T13:00:00Z"),
                tool_result("2026-10-23T13:00:01Z"),
                serde_json::json!({
                    "type": "assistant", "isSidechain": true, "timestamp": "2026-10-23T13:00:02Z",
                    "message": {"id": "sub", "model": "claude-haiku-5-5",
                        "usage": {"input_tokens": 7, "cache_read_input_tokens": 7}},
                })
                .to_string(),
            ],
        );
        let reading = reconcile(&mut store, "s1", &path, ContextEvent::Reconcile).unwrap();
        assert_eq!(reading.last_request_input_tokens, 100);
        assert_eq!(reading.message_id.as_deref(), Some("m1"));
        // On first sight too, read backwards.
        let mut fresh = ContextStore::load(None);
        let reading = reconcile(&mut fresh, "s1", &path, ContextEvent::Reconcile).unwrap();
        assert_eq!(reading.message_id.as_deref(), Some("m1"));
    }

    #[test]
    fn a_session_start_is_unknown_until_its_first_request() {
        let dir = temp_dir("clear");
        let old = dir.join("s1.jsonl");
        append(&old, &[assistant("m1", "2026-10-23T11:00:00Z", 1, 1, 98)]);
        let mut store = ContextStore::load(None);
        assert!(reconcile(&mut store, "s1", &old, ContextEvent::Reconcile).is_some());

        // `/clear`: a new session, a new transcript.
        let new = dir.join("s2.jsonl");
        assert_eq!(
            reconcile(&mut store, "s2", &new, ContextEvent::SessionStart),
            None
        );
        // A resume copies older requests into it: still not this session's.
        append(&new, &[assistant("m1", "2026-10-23T11:00:00Z", 1, 1, 98)]);
        assert_eq!(
            reconcile(&mut store, "s2", &new, ContextEvent::Reconcile),
            None
        );
        append(&new, &[assistant("m3", "2026-10-23T12:05:00Z", 2, 3, 5)]);
        let reading = reconcile(&mut store, "s2", &new, ContextEvent::Reconcile).unwrap();
        assert_eq!(reading.last_request_input_tokens, 10);

        // A session start in the same transcript (a resume, a compaction).
        assert_eq!(
            reconcile(&mut store, "s2", &new, ContextEvent::SessionStart),
            None
        );
        assert_eq!(
            reconcile(&mut store, "s2", &new, ContextEvent::Reconcile),
            None
        );
    }

    #[test]
    fn a_rewritten_transcript_is_unknown_until_the_next_request() {
        let dir = temp_dir("rewrite");
        let path = dir.join("s1.jsonl");
        append(&path, &[assistant("m1", "2026-10-23T11:00:00Z", 1, 1, 98)]);
        let mut store = ContextStore::load(None);
        assert!(reconcile(&mut store, "s1", &path, ContextEvent::Reconcile).is_some());
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                tool_result("2026-10-23T11:30:00Z"),
                assistant("m9", "2026-10-23T11:31:00Z", 1, 1, 1)
            ),
        )
        .unwrap();
        assert_eq!(
            reconcile(&mut store, "s1", &path, ContextEvent::Reconcile),
            None
        );
        append(&path, &[assistant("m10", "2026-10-23T12:10:00Z", 1, 1, 2)]);
        let reading = reconcile(&mut store, "s1", &path, ContextEvent::Reconcile).unwrap();
        assert_eq!(reading.message_id.as_deref(), Some("m10"));
    }

    #[test]
    fn the_cursor_resumes_after_a_restart() {
        let dir = temp_dir("restart");
        let path = dir.join("s1.jsonl");
        let state = dir.join("state");
        let mut store = ContextStore::load(Some(state.clone()));
        assert_eq!(
            reconcile(&mut store, "s1", &path, ContextEvent::SessionStart),
            None
        );
        append(&path, &[assistant("m1", "2026-10-23T12:01:00Z", 1, 2, 3)]);
        assert_eq!(
            reconcile(&mut store, "s1", &path, ContextEvent::Reconcile)
                .map(|reading| reading.last_request_input_tokens),
            Some(6)
        );
        drop(store);

        // A new server reads the persisted cursor: the reading is there at once.
        let mut restarted = ContextStore::load(Some(state));
        let cursor = restarted.cursors.get("claude:s1").cloned().unwrap();
        assert_eq!(
            reconcile(&mut restarted, "s1", &path, ContextEvent::Reconcile)
                .map(|reading| reading.last_request_input_tokens),
            Some(6)
        );
        // And only what was appended after it is read: a partial line waits for its end.
        let partial = assistant("m2", "2026-10-23T12:02:00Z", 1, 1, 1);
        let (head, tail) = partial.split_at(20);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(file, "{head}").unwrap();
        assert_eq!(
            reconcile(&mut restarted, "s1", &path, ContextEvent::Reconcile)
                .map(|reading| reading.last_request_input_tokens),
            Some(6)
        );
        assert_eq!(restarted.cursors["claude:s1"].offset, cursor.offset);
        writeln!(file, "{tail}").unwrap();
        assert_eq!(
            reconcile(&mut restarted, "s1", &path, ContextEvent::Reconcile)
                .map(|reading| reading.last_request_input_tokens),
            Some(3)
        );
    }

    #[test]
    fn a_long_line_before_the_last_request_is_read_backwards() {
        let dir = temp_dir("long");
        let path = dir.join("s1.jsonl");
        let big = serde_json::json!({
            "type": "user",
            "message": {"content": "x".repeat((BACKWARD_CHUNK * 3) as usize)},
        })
        .to_string();
        append(
            &path,
            &[assistant("m1", "2026-10-23T11:00:00Z", 1, 1, 1), big],
        );
        let mut store = ContextStore::load(None);
        let reading = reconcile(&mut store, "s1", &path, ContextEvent::Reconcile).unwrap();
        assert_eq!(reading.message_id.as_deref(), Some("m1"));
    }

    #[test]
    fn other_agents_and_unknown_transcripts_are_unknown() {
        let mut store = ContextStore::load(None);
        assert_eq!(
            store.reconcile(
                "codex",
                "s1",
                None,
                || None,
                ContextEvent::Reconcile,
                NOON_MS
            ),
            None
        );
        assert_eq!(
            store.reconcile(
                "claude",
                "s1",
                None,
                || None,
                ContextEvent::Reconcile,
                NOON_MS
            ),
            None
        );
    }
}
