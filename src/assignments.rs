//! Assignments: the prompts herdr typed into a pane's agent (`agent.prompt` and its variants),
//! each kept with the verdict its work ended on, so a coordinator learns a pane worker's
//! `WORKER-DONE`/`WORKER-BLOCKED` line even when nothing waited for it when it came, and after
//! the server restarted or handed off.
//!
//! An assignment is registered when herdr accepts the prompt, with its anchor: the length of the
//! agent's transcript at that moment, so only records written after the prompt can start it.
//! The server records the verdict itself when the agent reports a turn end (Claude's `Stop`):
//! it reads the transcript from the oldest open assignment's anchor and finds where each one
//! starts (the user message that carries its prompt, or the `queued_command` attachment Claude
//! writes for a prompt it took in mid-turn) and the `WORKER` line of the assistant text that
//! ended its turn. A verdict is recorded once and never changed (idempotent), with the time.
//! An assignment found started without a verdict moves its anchor to that start, and only a
//! pane's newest [`OPEN_SCANNED`] open assignments are looked for, so a turn end reads only the
//! transcript of the work still open. The read runs on the app thread at a turn end of a pane
//! with open assignments, not in a render or parse loop.
//!
//! A newer prompt into the same pane that starts before an assignment's verdict supersedes it:
//! the assignment closes as `superseded` (naming the newer request) and never takes the later
//! work's verdict, which would end a wait on the wrong work. A turn end (no background task
//! left) that brings an open, started assignment no verdict marks it stopped without one until
//! the pane's next turn starts.
//!
//! A verdict counts as delivered only when its waiter acknowledges it (`agent.ack_verdict`, on
//! the connection of the `agent.wait_verdict` that answered with it or on its own), not when
//! the answer is written: a waiter killed between the two leaves it undelivered.
//!
//! Records are JSON files in herdr's state directory (`assignments/<request_id>.json`, apart per
//! named session), written atomically; the newest [`MAX_KEPT`] are kept. A server started by a
//! live handoff reads them, so `agent.wait_verdict` answers at once with a verdict recorded
//! before the handoff, and its first supervision sweep reconciles them all. `agent.wait_verdict`
//! waits register here ([`wait_started`]), so the supervision sweep knows a verdict nobody
//! received ([`undelivered_unwatched`]); a recorded verdict or a supersession wakes them at once
//! ([`changed_since`]), whatever the pane's events, and a waiter that ends without an
//! acknowledgement asks for a sweep.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Records kept on disk; the oldest go first.
const MAX_KEPT: usize = 512;
/// The newest open assignments of a pane a turn end looks for: a prompt that never produced a
/// `WORKER` line (an ordinary question to an agent) stays open, and older ones would make every
/// turn end read the transcript from further back.
const OPEN_SCANNED: usize = 8;

/// One prompt herdr typed into a pane's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Assignment {
    pub(crate) request_id: String,
    /// The pane's public id when the prompt was typed.
    pub(crate) pane_id: String,
    /// The agent's label (`claude`, `pi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    /// The agent's name in herdr, the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) worker: Option<String>,
    /// The pane that sent the prompt (its `HERDR_PANE_ID`), the coordinator that owns the
    /// assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner_pane: Option<String>,
    /// The prompt, whitespace runs collapsed to one space.
    pub(crate) text: String,
    /// The agent's transcript when the prompt was typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) transcript: Option<String>,
    /// The transcript's length then: the assignment starts after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) anchor: Option<u64>,
    /// Unix milliseconds.
    pub(crate) accepted_ms: u64,
    /// Orders the assignments of one server run; a later one starts after an earlier one.
    #[serde(default)]
    pub(crate) order: u64,
    /// When its start was found in the transcript, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verdict: Option<Verdict>,
    /// The newer prompt that started before this one's verdict and closed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) superseded: Option<Superseded>,
    /// When a turn end with no background task left brought this started assignment no
    /// verdict, Unix milliseconds; cleared when the pane's next turn starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stopped_ms: Option<u64>,
    /// When an `agent.wait_verdict` first answered with the verdict, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) answered_ms: Option<u64>,
    /// When the waiter acknowledged the verdict (`agent.ack_verdict`), Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivered_ms: Option<u64>,
}

impl Assignment {
    /// Neither a verdict nor a newer prompt closed it.
    pub(crate) fn is_open(&self) -> bool {
        self.verdict.is_none() && self.superseded.is_none()
    }
}

/// A newer prompt into the same pane that started before an assignment's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Superseded {
    /// The newer prompt's request.
    pub(crate) by: String,
    /// Unix milliseconds.
    pub(crate) at_ms: u64,
}

/// The `WORKER` line an assignment's work ended on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Verdict {
    /// The line, from `WORKER-DONE` or `WORKER-BLOCKED` on.
    pub(crate) line: String,
    pub(crate) blocked: bool,
    /// The commit a `WORKER-DONE` line names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha: Option<String>,
    /// Unix milliseconds.
    pub(crate) recorded_ms: u64,
}

#[derive(Default)]
struct Store {
    loaded: bool,
    records: BTreeMap<String, Assignment>,
    next_order: u64,
}

static STORE: Mutex<Store> = Mutex::new(Store {
    loaded: false,
    records: BTreeMap::new(),
    next_order: 0,
});
/// The open `agent.wait_verdict` waits, by request id.
static WAITERS: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());
/// How many times each request's assignment closed (a verdict, a supersession), so its waiters
/// read it again at once ([`changed_since`]).
static CHANGES: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
static TEST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Tests keep assignments in memory unless they name a directory, which also forgets what
/// was loaded, as a new server would.
#[cfg(test)]
pub(crate) fn set_test_dir(dir: Option<PathBuf>) {
    *lock(&TEST_DIR) = dir;
    *lock(&STORE) = Store::default();
}

#[cfg(test)]
fn store_dir() -> Option<PathBuf> {
    lock(&TEST_DIR).clone()
}

#[cfg(not(test))]
fn store_dir() -> Option<PathBuf> {
    let state_dir = crate::config::state_dir();
    Some(match crate::session::active_name() {
        Some(name) => state_dir.join("sessions").join(name).join("assignments"),
        None => state_dir.join("assignments"),
    })
}

fn file_of(dir: &Path, request_id: &str) -> PathBuf {
    let name: String = request_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{name}.json"))
}

fn read_record(path: &Path) -> Option<Assignment> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The store, with the records on disk read once per server.
fn store() -> MutexGuard<'static, Store> {
    let mut store = lock(&STORE);
    if !store.loaded {
        store.loaded = true;
        if let Some(dir) = store_dir() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    if let Some(record) = read_record(&path) {
                        store.next_order = store.next_order.max(record.order + 1);
                        store.records.insert(record.request_id.clone(), record);
                    }
                }
            }
        }
    }
    store
}

/// Writes `record` atomically (a temporary file renamed over it); a failure is logged and the
/// record stays in memory.
fn persist(record: &Assignment) {
    let Some(dir) = store_dir() else {
        return;
    };
    let path = file_of(&dir, &record.request_id);
    let result = (|| {
        std::fs::create_dir_all(&dir)?;
        let text = serde_json::to_string(record).map_err(std::io::Error::other)?;
        let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(error) = result {
        warn!(%error, request = %record.request_id, "cannot store the assignment");
    }
}

fn prune(store: &mut Store) {
    while store.records.len() > MAX_KEPT {
        let Some(oldest) = store
            .records
            .values()
            .min_by_key(|record| (record.accepted_ms, record.order))
            .map(|record| record.request_id.clone())
        else {
            return;
        };
        store.records.remove(&oldest);
        if let Some(dir) = store_dir() {
            let _ = std::fs::remove_file(file_of(&dir, &oldest));
        }
    }
}

/// Registers a prompt herdr accepted. `transcript` is the agent's transcript now, whose length
/// becomes the anchor.
pub(crate) fn accepted(
    request_id: &str,
    pane_id: &str,
    agent: Option<&str>,
    worker: Option<&str>,
    owner_pane: Option<&str>,
    text: &str,
    transcript: Option<&Path>,
) {
    let anchor = transcript
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    let mut store = store();
    let order = store.next_order;
    store.next_order += 1;
    let record = Assignment {
        request_id: request_id.to_string(),
        pane_id: pane_id.to_string(),
        agent: agent.map(str::to_string),
        worker: worker.map(str::to_string),
        owner_pane: owner_pane.map(str::to_string),
        text: normalize(text),
        transcript: transcript.map(|path| path.display().to_string()),
        anchor,
        accepted_ms: now_ms(),
        order,
        started_ms: None,
        verdict: None,
        superseded: None,
        stopped_ms: None,
        answered_ms: None,
        delivered_ms: None,
    };
    persist(&record);
    store.records.insert(record.request_id.clone(), record);
    prune(&mut store);
}

/// The assignment of `request_id`, also one another server recorded after this one read the
/// directory.
pub(crate) fn get(request_id: &str) -> Option<Assignment> {
    let mut store = store();
    // The file is the truth: a server that handed off may have written it after this one
    // read the directory.
    let on_disk = store_dir().and_then(|dir| read_record(&file_of(&dir, request_id)));
    if let Some(record) = on_disk {
        store
            .records
            .insert(record.request_id.clone(), record.clone());
        return Some(record);
    }
    store.records.get(request_id).cloned()
}

/// The pane's newest [`OPEN_SCANNED`] open assignments, oldest first.
pub(crate) fn open_for_pane(pane_id: &str) -> Vec<Assignment> {
    let store = store();
    let mut open: Vec<Assignment> = store
        .records
        .values()
        .filter(|record| record.pane_id == pane_id && record.is_open())
        .cloned()
        .collect();
    open.sort_by_key(|record| (record.accepted_ms, record.order));
    let skipped = open.len().saturating_sub(OPEN_SCANNED);
    open.split_off(skipped)
}

/// The assignment started at byte `offset` of `transcript` and has no verdict yet: the next
/// turn end reads the transcript from there instead of from where the prompt was typed.
pub(crate) fn started_at(request_id: &str, transcript: &Path, offset: u64) {
    let mut store = store();
    let Some(record) = store.records.get_mut(request_id) else {
        return;
    };
    let transcript = transcript.display().to_string();
    if !record.is_open()
        || (record.transcript.as_deref() == Some(transcript.as_str())
            && record.anchor == Some(offset)
            && record.started_ms.is_some())
    {
        return;
    }
    record.transcript = Some(transcript);
    record.anchor = Some(offset);
    record.started_ms.get_or_insert_with(now_ms);
    let record = record.clone();
    persist(&record);
}

/// Changes the record of `request_id` with `change` when it returns `true`, then stores it;
/// whether it changed.
fn update(request_id: &str, change: impl FnOnce(&mut Assignment) -> bool) -> bool {
    let mut store = store();
    let Some(record) = store.records.get_mut(request_id) else {
        return false;
    };
    if !change(record) {
        return false;
    }
    let record = record.clone();
    persist(&record);
    true
}

/// The assignment of `request_id` closed: its waiters read it again.
fn closed(request_id: &str) {
    *lock(&CHANGES).entry(request_id.to_string()).or_default() += 1;
}

/// Records the verdict of `request_id` unless it is closed; whether it was recorded now.
pub(crate) fn record_verdict(request_id: &str, verdict: Verdict) -> bool {
    let recorded = update(request_id, |record| {
        if !record.is_open() {
            return false;
        }
        record.verdict = Some(verdict);
        record.stopped_ms = None;
        true
    });
    if recorded {
        closed(request_id);
    }
    recorded
}

/// Closes `request_id` as superseded by the newer prompt `by` unless it is closed; whether it
/// was closed now.
pub(crate) fn supersede(request_id: &str, by: &str) -> bool {
    let superseded = update(request_id, |record| {
        if !record.is_open() {
            return false;
        }
        record.superseded = Some(Superseded {
            by: by.to_string(),
            at_ms: now_ms(),
        });
        record.stopped_ms = None;
        true
    });
    if superseded {
        closed(request_id);
    }
    superseded
}

/// A turn end with no background task left brought the open, started assignment
/// `request_id` no verdict; whether that is new.
pub(crate) fn stopped_without_verdict(request_id: &str) -> bool {
    update(request_id, |record| {
        if !record.is_open() || record.stopped_ms.is_some() {
            return false;
        }
        record.stopped_ms = Some(now_ms());
        true
    })
}

/// A turn of the agent in `pane_id` started: its open assignments are worked on again.
/// Whether one was marked stopped without a verdict.
pub(crate) fn turn_started(pane_id: &str) -> bool {
    let stopped: Vec<String> = store()
        .records
        .values()
        .filter(|record| record.pane_id == pane_id && record.stopped_ms.is_some())
        .map(|record| record.request_id.clone())
        .collect();
    let mut cleared = false;
    for request_id in stopped {
        cleared |= update(&request_id, |record| record.stopped_ms.take().is_some());
    }
    cleared
}

/// An `agent.wait_verdict` answered with the verdict of `request_id`; it is not delivered
/// until the waiter acknowledges it ([`acknowledge`]).
pub(crate) fn mark_answered(request_id: &str) {
    update(request_id, |record| {
        if record.verdict.is_none() || record.answered_ms.is_some() {
            return false;
        }
        record.answered_ms = Some(now_ms());
        true
    });
}

/// Why an acknowledgement was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AckError {
    /// Herdr keeps no assignment of that request.
    NotFound,
    /// The assignment has no verdict to acknowledge.
    NoVerdict,
}

/// The waiter acknowledged the verdict of `request_id`: it is delivered (once; a repeated
/// acknowledgement answers it as it is).
pub(crate) fn acknowledge(request_id: &str) -> Result<Assignment, AckError> {
    let record = get(request_id).ok_or(AckError::NotFound)?;
    if record.verdict.is_none() {
        return Err(AckError::NoVerdict);
    }
    let delivered_now = update(request_id, |record| {
        if record.delivered_ms.is_some() {
            return false;
        }
        record.delivered_ms = Some(now_ms());
        true
    });
    if delivered_now {
        // A finding about it resolves.
        crate::workers::supervision::request_sweep(&format!(
            "verdict of {request_id} acknowledged"
        ));
    }
    get(request_id).ok_or(AckError::NotFound)
}

/// The answer to an acknowledgement of the verdict of `request_id` (`agent.ack_verdict`), also
/// one the api server reads on the connection of the `agent.wait_verdict` that answered with
/// it.
pub(crate) fn ack_response(id: String, request_id: &str) -> String {
    use crate::api::schema::{ErrorBody, ErrorResponse, ResponseResult, SuccessResponse};
    let error = |code: &str, message: String| {
        serde_json::to_string(&ErrorResponse {
            id: id.clone(),
            error: ErrorBody {
                code: code.into(),
                message,
            },
        })
    };
    let encoded = match acknowledge(request_id) {
        Ok(assignment) => serde_json::to_string(&SuccessResponse {
            id: id.clone(),
            result: ResponseResult::AgentAssignment {
                assignment: info(&assignment),
            },
        }),
        Err(AckError::NotFound) => error(
            "prompt_request_not_found",
            format!(
                "herdr keeps no assignment {request_id}; it never typed that prompt, or the \
                 record is gone"
            ),
        ),
        Err(AckError::NoVerdict) => error(
            "verdict_not_recorded",
            format!("the assignment {request_id} has no verdict to acknowledge"),
        ),
    };
    encoded.unwrap_or_else(|encoding| {
        format!(
            r#"{{"id":{},"error":{{"code":"internal_error","message":{}}}}}"#,
            serde_json::Value::String(id),
            serde_json::Value::String(encoding.to_string())
        )
    })
}

/// The verdicts no waiter has acknowledged while none waits for them.
pub(crate) fn undelivered_unwatched() -> Vec<Assignment> {
    let waiting: BTreeSet<String> = lock(&WAITERS).keys().cloned().collect();
    store()
        .records
        .values()
        .filter(|record| {
            record.verdict.is_some()
                && record.delivered_ms.is_none()
                && !waiting.contains(&record.request_id)
        })
        .cloned()
        .collect()
}

/// The open assignments a turn end left without a verdict, while no turn started since.
pub(crate) fn stopped_open() -> Vec<Assignment> {
    store()
        .records
        .values()
        .filter(|record| record.is_open() && record.stopped_ms.is_some())
        .cloned()
        .collect()
}

/// An open `agent.wait_verdict` on one request, for as long as it lives (also while it waits
/// for the acknowledgement of the verdict it answered with). Its end without one asks for a
/// supervision sweep.
pub(crate) struct WaiterGuard(String);

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let last = {
            let mut waiters = lock(&WAITERS);
            match waiters.get_mut(&self.0) {
                Some(count) => {
                    *count -= 1;
                    let last = *count == 0;
                    if last {
                        waiters.remove(&self.0);
                    }
                    last
                }
                None => false,
            }
        };
        let unacknowledged = last
            && store()
                .records
                .get(&self.0)
                .is_some_and(|record| record.verdict.is_some() && record.delivered_ms.is_none());
        if unacknowledged {
            crate::workers::supervision::request_sweep(&format!(
                "the waiter of {} ended without acknowledging its verdict",
                self.0
            ));
        }
    }
}

/// How many times the assignment of `request_id` closed so far, for [`changed_since`].
pub(crate) fn change_count(request_id: &str) -> u64 {
    lock(&CHANGES).get(request_id).copied().unwrap_or(0)
}

/// Whether the assignment of `request_id` closed since `seen` ([`change_count`]), which it
/// moves to now.
pub(crate) fn changed_since(request_id: &str, seen: &mut u64) -> bool {
    let now = change_count(request_id);
    let changed = now != *seen;
    *seen = now;
    changed
}

pub(crate) fn wait_started(request_id: &str) -> WaiterGuard {
    *lock(&WAITERS).entry(request_id.to_string()).or_default() += 1;
    WaiterGuard(request_id.to_string())
}

pub(crate) fn has_waiter(request_id: &str) -> bool {
    lock(&WAITERS).contains_key(request_id)
}

/// Unix milliseconds now.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Whitespace runs collapse to one space, as herdr keeps a prompt's text.
pub(crate) fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn worker_line_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // `<sha>`/`<reason>` placeholders, as in a quoted task text, do not count.
        regex::Regex::new(r"^[\s⏺●•]*(WORKER-DONE ([0-9a-f]{7,40})\b.*|WORKER-BLOCKED [^<\s].*)$")
            .unwrap_or_else(|_| unreachable!("the WORKER line pattern is valid"))
    })
}

/// The last `WORKER` line of a text, as a verdict recorded at `now_ms`.
pub(crate) fn worker_line(text: &str, now_ms: u64) -> Option<Verdict> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .rev()
        .find_map(|line| {
            let captures = worker_line_pattern().captures(line)?;
            let verdict = captures.get(1)?.as_str().trim().to_string();
            Some(Verdict {
                blocked: verdict.starts_with("WORKER-BLOCKED"),
                sha: captures.get(2).map(|sha| sha.as_str().to_string()),
                line: verdict,
                recorded_ms: now_ms,
            })
        })
}

/// What one transcript read found for a pane's open assignments.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    /// The assignments whose start was found; `true` for one Claude took in mid-turn (a
    /// `queued_command` attachment), whose turn start the agent never reported.
    pub(crate) started: BTreeMap<String, bool>,
    /// The byte offset of each start found.
    pub(crate) start_offsets: BTreeMap<String, u64>,
    pub(crate) verdicts: BTreeMap<String, Verdict>,
    /// The assignments a newer prompt started before their verdict, with that prompt's request.
    pub(crate) superseded: BTreeMap<String, String>,
}

/// One record of a transcript that matters here.
enum Entry {
    /// A prompt the agent took: a user message, or a queued command it took in mid-turn.
    Prompt {
        text: String,
        absorbed: bool,
    },
    /// Another user record: a tool result, an interruption.
    User,
    Assistant {
        text: String,
        calls_tool: bool,
    },
}

const TOOL_CALL_PARTS: [&str; 4] = ["tool_use", "toolCall", "tool_call", "server_tool_use"];

fn text_of(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => Some(
            parts
                .iter()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// The entry of one transcript line, or `None` for one that does not matter (a subagent's,
/// another attachment, a duplicate of a queued command already seen).
fn entry_of(line: &str, absorbed_seen: &mut BTreeSet<String>) -> Option<Entry> {
    let record: serde_json::Value = serde_json::from_str(line).ok()?;
    if record["isSidechain"] == true {
        return None;
    }
    // Claude writes a prompt it took in mid-turn twice: a `queue-operation` `remove` with the
    // reason `absorbed_mid_turn`, then a `queued_command` attachment carrying it; both name the
    // command, so it counts once.
    let absorbed = match record["type"].as_str() {
        Some("queue-operation")
            if record["operation"] == "remove" && record["reason"] == "absorbed_mid_turn" =>
        {
            Some((record["commandUuid"].as_str(), &record["content"]))
        }
        Some("attachment") if record["attachment"]["type"] == "queued_command" => {
            let attachment = &record["attachment"];
            Some((attachment["source_uuid"].as_str(), &attachment["prompt"]))
        }
        _ => None,
    };
    if let Some((command, prompt)) = absorbed {
        if let Some(command) = command {
            if !absorbed_seen.insert(command.to_string()) {
                return None;
            }
        }
        return Some(Entry::Prompt {
            text: text_of(prompt)?,
            absorbed: true,
        });
    }
    let message = record.get("message")?;
    let role = message["role"].as_str().or(record["type"].as_str())?;
    let content = &message["content"];
    match role {
        "user" => {
            let text = text_of(content).unwrap_or_default();
            Some(if text.trim().is_empty() {
                Entry::User
            } else {
                Entry::Prompt {
                    text,
                    absorbed: false,
                }
            })
        }
        "assistant" => {
            let calls_tool = content.as_array().is_some_and(|parts| {
                parts.iter().any(|part| {
                    part["type"]
                        .as_str()
                        .is_some_and(|kind| TOOL_CALL_PARTS.contains(&kind))
                })
            });
            Some(Entry::Assistant {
                text: text_of(content).unwrap_or_default(),
                calls_tool,
            })
        }
        _ => None,
    }
}

/// Finds the start and the verdict of each open assignment (oldest first) in transcript
/// `lines`, each with its byte offset. An assignment starts at the last prompt carrying its
/// text after its anchor. Its verdict is the first `WORKER` line of an assistant text that
/// ended a turn after its start (no tool call followed it before the next user record; the
/// text at the end counts when `turn_over`). A record that starts a newer assignment
/// supersedes every earlier one still without a verdict that it does not carry too.
pub(crate) fn scan_lines<'a>(
    lines: impl Iterator<Item = (u64, &'a str)>,
    open: &[Assignment],
    turn_over: bool,
    now_ms: u64,
) -> Scan {
    let mut scan = Scan::default();
    let mut absorbed_seen = BTreeSet::new();
    let mut pending: Option<String> = None;
    // A text that ended a turn is the verdict of every started assignment still without one
    // (several when the agent took queued prompts as one).
    let settle = |scan: &mut Scan, pending: &mut Option<String>| {
        let Some(verdict) = pending.take().and_then(|text| worker_line(&text, now_ms)) else {
            return;
        };
        let waiting: Vec<String> = scan
            .started
            .keys()
            .filter(|id| !scan.verdicts.contains_key(*id))
            .cloned()
            .collect();
        for id in waiting {
            scan.verdicts.insert(id, verdict.clone());
        }
    };
    for (offset, line) in lines {
        let Some(entry) = entry_of(line, &mut absorbed_seen) else {
            continue;
        };
        match entry {
            Entry::Prompt { text, absorbed } => {
                settle(&mut scan, &mut pending);
                let text = normalize(&text);
                let carried: Vec<bool> = open
                    .iter()
                    .map(|assignment| {
                        !assignment.text.is_empty()
                            && assignment.anchor.is_none_or(|anchor| anchor <= offset)
                            && !scan.superseded.contains_key(&assignment.request_id)
                            && text.contains(&assignment.text)
                    })
                    .collect();
                // The newest assignment typed before this record whose prompt it carries.
                let Some(newest) = carried.iter().rposition(|carries| *carries) else {
                    continue;
                };
                // The earlier ones this record carries too (the agent took queued prompts as
                // one) start with it; every other earlier one still without a verdict is
                // superseded by it, so it never takes this later work's verdict.
                let mut starting = Vec::new();
                for (earlier, carries) in open[..newest].iter().zip(&carried) {
                    if *carries {
                        starting.push(earlier);
                    } else if !scan.verdicts.contains_key(&earlier.request_id)
                        && !scan.superseded.contains_key(&earlier.request_id)
                    {
                        scan.started.remove(&earlier.request_id);
                        scan.start_offsets.remove(&earlier.request_id);
                        scan.superseded
                            .insert(earlier.request_id.clone(), open[newest].request_id.clone());
                    }
                }
                starting.push(&open[newest]);
                for assignment in starting {
                    scan.verdicts.remove(&assignment.request_id);
                    scan.started.insert(assignment.request_id.clone(), absorbed);
                    scan.start_offsets
                        .insert(assignment.request_id.clone(), offset);
                }
            }
            Entry::User => settle(&mut scan, &mut pending),
            Entry::Assistant { text, calls_tool } => {
                if !text.trim().is_empty() {
                    pending = Some(text);
                }
                if calls_tool {
                    pending = None;
                }
            }
        }
    }
    if turn_over {
        settle(&mut scan, &mut pending);
    }
    scan
}

/// Reads transcript `path` at a turn end for the pane's `open` assignments (oldest first) and
/// scans it ([`scan_lines`]) from the oldest anchor among those typed into this transcript.
/// An assignment typed into another transcript (the agent's session changed since) or one
/// without an anchor is matched by its text from the start of the file; so are all when the
/// file is shorter than the anchor (it was replaced).
pub(crate) fn scan_open(path: &Path, open: &[Assignment], now_ms: u64) -> std::io::Result<Scan> {
    let here = path.display().to_string();
    let mut open = open.to_vec();
    for assignment in &mut open {
        if assignment.transcript.as_deref() != Some(here.as_str()) {
            assignment.anchor = None;
        }
    }
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut from = open
        .iter()
        .map(|assignment| assignment.anchor.unwrap_or(0))
        .min()
        .unwrap_or(0);
    if len < from {
        from = 0;
        open.iter_mut()
            .for_each(|assignment| assignment.anchor = None);
    }
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut offset = from;
    let lines = bytes.split(|byte| *byte == b'\n').map(|line| {
        let at = offset;
        offset += line.len() as u64 + 1;
        (at, std::str::from_utf8(line).unwrap_or(""))
    });
    Ok(scan_lines(lines, &open, true, now_ms))
}

/// An assignment as the API shows it.
pub(crate) fn info(assignment: &Assignment) -> crate::api::schema::AgentAssignmentInfo {
    use crate::api::schema::{AgentAssignmentInfo, AgentVerdictInfo, AgentVerdictKind};
    AgentAssignmentInfo {
        request_id: assignment.request_id.clone(),
        pane_id: assignment.pane_id.clone(),
        worker: assignment.worker.clone(),
        accepted_ms: assignment.accepted_ms,
        verdict: assignment.verdict.as_ref().map(|verdict| AgentVerdictInfo {
            kind: if verdict.blocked {
                AgentVerdictKind::Blocked
            } else {
                AgentVerdictKind::Done
            },
            line: verdict.line.clone(),
            sha: verdict.sha.clone(),
            recorded_ms: verdict.recorded_ms,
        }),
        superseded_by: assignment
            .superseded
            .as_ref()
            .map(|superseded| superseded.by.clone()),
        answered_ms: assignment.answered_ms,
        delivered_ms: assignment.delivered_ms,
    }
}

/// The transcript of an agent session: the path its integration reported, a path session, or
/// for Claude a session id under its projects directory.
pub(crate) fn transcript_of(
    reported: Option<&str>,
    session: Option<&crate::api::schema::AgentSessionInfo>,
) -> Option<PathBuf> {
    if let Some(path) = reported.filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let session = session?;
    match session.kind {
        crate::agent_resume::AgentSessionRefKind::Path => Some(PathBuf::from(&session.value)),
        crate::agent_resume::AgentSessionRefKind::Id if session.agent == "claude" => {
            let root = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude"))
                })?
                .join("projects");
            let file = format!("{}.jsonl", session.value);
            std::fs::read_dir(root)
                .ok()?
                .flatten()
                .map(|project| project.path().join(&file))
                .find(|path| path.is_file())
        }
        crate::agent_resume::AgentSessionRefKind::Id => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment(id: &str, text: &str, anchor: Option<u64>) -> Assignment {
        Assignment {
            request_id: id.into(),
            pane_id: "w1:p1".into(),
            agent: Some("claude".into()),
            worker: None,
            owner_pane: None,
            text: normalize(text),
            transcript: None,
            anchor,
            accepted_ms: 1,
            order: 0,
            started_ms: None,
            verdict: None,
            superseded: None,
            stopped_ms: None,
            answered_ms: None,
            delivered_ms: None,
        }
    }

    fn user(text: &str) -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": text}})
            .to_string()
    }

    fn tool_result() -> String {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
        ]}})
        .to_string()
    }

    fn assistant(text: &str, calls_tool: bool) -> String {
        let mut content = vec![serde_json::json!({"type": "text", "text": text})];
        if calls_tool {
            content.push(serde_json::json!({"type": "tool_use", "id": "t1", "name": "Bash"}));
        }
        serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": content}})
            .to_string()
    }

    fn absorbed(prompt: &str, command: &str) -> Vec<String> {
        vec![
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": prompt})
                .to_string(),
            serde_json::json!({"type": "queue-operation", "operation": "remove",
                "content": prompt, "reason": "absorbed_mid_turn", "commandUuid": command})
            .to_string(),
            serde_json::json!({"type": "attachment", "isSidechain": false, "attachment": {
                "type": "queued_command", "prompt": prompt, "source_uuid": command}})
            .to_string(),
        ]
    }

    fn scan(lines: &[String], open: &[Assignment], turn_over: bool) -> Scan {
        let mut offset = 0;
        let lines: Vec<(u64, &str)> = lines
            .iter()
            .map(|line| {
                let at = offset;
                offset += line.len() as u64 + 1;
                (at, line.as_str())
            })
            .collect();
        scan_lines(lines.into_iter(), open, turn_over, 7)
    }

    #[test]
    fn a_worker_line_ignores_placeholders_and_takes_the_last() {
        assert_eq!(
            worker_line("End with WORKER-DONE <sha> | <summary>", 1),
            None
        );
        assert_eq!(worker_line("WORKER-BLOCKED <reason>", 1), None);
        let verdict = worker_line("done\n⏺ WORKER-DONE abc1234 | it works\n\n", 1).unwrap();
        assert_eq!(verdict.line, "WORKER-DONE abc1234 | it works");
        assert_eq!(verdict.sha.as_deref(), Some("abc1234"));
        assert!(!verdict.blocked);
        let blocked = worker_line("WORKER-BLOCKED the sandbox refuses it", 1).unwrap();
        assert!(blocked.blocked);
        assert_eq!(blocked.sha, None);
    }

    #[test]
    fn a_prompt_taken_in_mid_turn_starts_its_assignment_once() {
        let open = [assignment("p2", "do item two", Some(0))];
        let mut lines = vec![user("do item one"), assistant("working", true)];
        lines.extend(absorbed("do item two", "c1"));
        lines.push(tool_result());
        lines.push(assistant("WORKER-DONE abc1234 | item two", false));
        let found = scan(&lines, &open, true);
        assert_eq!(found.started.get("p2"), Some(&true));
        assert_eq!(
            found
                .verdicts
                .get("p2")
                .map(|verdict| verdict.line.as_str()),
            Some("WORKER-DONE abc1234 | item two")
        );
    }

    #[test]
    fn an_enqueued_prompt_does_not_start_its_assignment_before_the_agent_takes_it() {
        // The earlier assignment's turn ends with its own verdict after the enqueue.
        let open = [assignment("p2", "do item two", Some(0))];
        let lines = vec![
            user("do item one"),
            serde_json::json!({"type": "queue-operation", "operation": "enqueue", "content": "do item two"})
                .to_string(),
            assistant("WORKER-DONE abc1234 | item one", false),
        ];
        let found = scan(&lines, &open, true);
        assert!(found.started.is_empty());
        assert!(found.verdicts.is_empty());
    }

    #[test]
    fn a_newer_prompt_supersedes_an_assignment_without_a_verdict() {
        let open = [
            assignment("p1", "do item one", Some(0)),
            assignment("p2", "do item two", Some(0)),
            assignment("p3", "do item three", Some(0)),
        ];
        let lines = vec![
            user("do item one"),
            assistant("a question?", false),
            user("do item two"),
            assistant("WORKER-BLOCKED item two needs a key", false),
            user("do item three"),
            assistant("WORKER-DONE abc1234 | three", false),
        ];
        let found = scan(&lines, &open, true);
        // p1 never takes p2's verdict, which would end its wait on the wrong work.
        assert_eq!(found.superseded.get("p1").map(String::as_str), Some("p2"));
        assert!(!found.verdicts.contains_key("p1"));
        assert!(!found.started.contains_key("p1"));
        assert!(found.verdicts["p2"].blocked);
        assert!(!found.superseded.contains_key("p2"));
        assert_eq!(found.verdicts["p3"].sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn a_prompt_taken_in_mid_turn_supersedes_the_running_assignment() {
        let open = [
            assignment("p1", "do item one", Some(0)),
            assignment("p2", "do item two", Some(0)),
        ];
        let mut lines = vec![user("do item one"), assistant("working", true)];
        lines.extend(absorbed("do item two", "c1"));
        lines.push(tool_result());
        lines.push(assistant("WORKER-DONE abc1234 | item two", false));
        let found = scan(&lines, &open, true);
        assert_eq!(found.superseded.get("p1").map(String::as_str), Some("p2"));
        assert!(!found.verdicts.contains_key("p1"));
        assert_eq!(found.verdicts["p2"].sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn queued_prompts_the_agent_took_as_one_share_the_verdict() {
        let open = [
            assignment("p1", "do item one", Some(0)),
            assignment("p2", "do item two", Some(0)),
        ];
        let lines = vec![
            user("do item one\ndo item two"),
            assistant("WORKER-DONE abc1234 | both", false),
        ];
        let found = scan(&lines, &open, true);
        assert!(found.superseded.is_empty());
        assert_eq!(found.verdicts["p1"].sha.as_deref(), Some("abc1234"));
        assert_eq!(found.verdicts["p2"].sha.as_deref(), Some("abc1234"));
    }

    #[test]
    fn the_anchor_skips_an_earlier_identical_prompt_and_a_tool_call_is_no_turn_end() {
        let first = user("do the item");
        let open = [assignment(
            "p2",
            "do the item",
            Some(first.len() as u64 + 1),
        )];
        let lines = vec![
            first,
            assistant("WORKER-DONE 1111111 | old", false),
            user("do the item"),
            assistant("WORKER-DONE abc1234 | before the check", true),
            tool_result(),
        ];
        let found = scan(&lines, &open, false);
        assert_eq!(found.started.get("p2"), Some(&false));
        assert!(found.verdicts.is_empty());
        let mut lines = lines;
        lines.push(assistant("WORKER-DONE abc1234 | new", false));
        assert_eq!(scan(&lines, &open, false).verdicts.get("p2"), None);
        assert_eq!(
            scan(&lines, &open, true).verdicts["p2"].line,
            "WORKER-DONE abc1234 | new"
        );
    }

    #[test]
    fn a_turn_end_reads_from_the_start_of_the_newest_open_assignments() {
        set_test_dir(None);
        for n in 0..10 {
            accepted(
                &format!("prompt_o_{n}"),
                "w9:p1",
                None,
                None,
                None,
                "ask",
                None,
            );
        }
        let open = open_for_pane("w9:p1");
        assert_eq!(open.len(), OPEN_SCANNED);
        assert_eq!(open[0].request_id, "prompt_o_2");
        assert_eq!(open[7].request_id, "prompt_o_9");

        let transcript = Path::new("/tmp/session.jsonl");
        started_at("prompt_o_9", transcript, 4096);
        let record = get("prompt_o_9").unwrap();
        assert_eq!(record.anchor, Some(4096));
        assert_eq!(record.transcript.as_deref(), Some("/tmp/session.jsonl"));
    }

    #[test]
    fn a_verdict_is_recorded_once_and_survives_a_new_server() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-assignments-{}-{}",
            std::process::id(),
            now_ms()
        ));
        set_test_dir(Some(dir.clone()));
        accepted(
            "prompt_a_1",
            "w1:p1",
            Some("claude"),
            Some("w"),
            Some("w1:p9"),
            "do it",
            None,
        );
        assert_eq!(open_for_pane("w1:p1").len(), 1);
        let verdict = worker_line("WORKER-DONE abc1234 | x", 5).unwrap();
        assert!(record_verdict("prompt_a_1", verdict.clone()));
        assert!(!record_verdict(
            "prompt_a_1",
            worker_line("WORKER-BLOCKED other", 6).unwrap()
        ));
        assert!(open_for_pane("w1:p1").is_empty());
        assert_eq!(undelivered_unwatched().len(), 1);
        {
            let _waiter = wait_started("prompt_a_1");
            assert!(has_waiter("prompt_a_1"));
            assert!(undelivered_unwatched().is_empty());
        }
        assert!(!has_waiter("prompt_a_1"));

        // A new server reads the directory again.
        set_test_dir(Some(dir.clone()));
        let record = get("prompt_a_1").unwrap();
        assert_eq!(record.verdict, Some(verdict));
        assert_eq!(record.owner_pane.as_deref(), Some("w1:p9"));
        // An answer written is not a delivery; the waiter's acknowledgement is.
        mark_answered("prompt_a_1");
        set_test_dir(Some(dir.clone()));
        assert!(get("prompt_a_1").unwrap().answered_ms.is_some());
        assert_eq!(undelivered_unwatched().len(), 1);
        let acknowledged = acknowledge("prompt_a_1").unwrap();
        assert!(acknowledged.delivered_ms.is_some());
        assert_eq!(acknowledge("prompt_a_1").unwrap(), acknowledged);
        set_test_dir(Some(dir.clone()));
        assert!(undelivered_unwatched().is_empty());
        assert_eq!(acknowledge("prompt_none"), Err(AckError::NotFound));
        set_test_dir(None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_superseded_or_stopped_assignment_is_kept_and_wakes_its_waiters() {
        set_test_dir(None);
        accepted("prompt_s_1", "w8:p1", None, None, None, "one", None);
        accepted("prompt_s_2", "w8:p1", None, None, None, "two", None);
        assert_eq!(acknowledge("prompt_s_1"), Err(AckError::NoVerdict));
        let mut seen = change_count("prompt_s_1");
        assert!(!changed_since("prompt_s_1", &mut seen));
        assert!(supersede("prompt_s_1", "prompt_s_2"));
        assert!(changed_since("prompt_s_1", &mut seen));
        assert!(!supersede("prompt_s_1", "prompt_s_3"));
        let record = get("prompt_s_1").unwrap();
        assert_eq!(
            record.superseded.map(|superseded| superseded.by).as_deref(),
            Some("prompt_s_2")
        );
        // A closed assignment takes no verdict, and is not open any more.
        assert!(!record_verdict(
            "prompt_s_1",
            worker_line("WORKER-DONE abc1234 | x", 1).unwrap()
        ));
        assert_eq!(
            open_for_pane("w8:p1")
                .iter()
                .map(|record| record.request_id.as_str())
                .collect::<Vec<_>>(),
            vec!["prompt_s_2"]
        );

        // A turn end without a verdict holds until the pane's next turn starts.
        assert!(stopped_without_verdict("prompt_s_2"));
        assert!(!stopped_without_verdict("prompt_s_2"));
        assert_eq!(stopped_open().len(), 1);
        assert!(turn_started("w8:p1"));
        assert!(!turn_started("w8:p1"));
        assert!(stopped_open().is_empty());
        assert!(stopped_without_verdict("prompt_s_2"));
        assert!(record_verdict(
            "prompt_s_2",
            worker_line("WORKER-DONE abc1234 | two", 1).unwrap()
        ));
        assert!(stopped_open().is_empty());
    }
}
