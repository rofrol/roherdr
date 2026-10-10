//! A worker's transcript for its read-only tab: the journal's records as
//! structured events ([`super::log::record_entries`]), read from a line on,
//! with the tab's identity every client agrees on. `worker.transcript`
//! reads it, `worker.transcript_wait` waits for new lines, and a client
//! shell gets them pushed while its worker tab is open
//! ([`set_append_notifier`]).

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::api::schema::{
    PaneKind, WorkerTab, WorkerTranscript, WorkerTranscriptEvent, WorkerTranscriptParams,
};

/// The id of a worker's tab and of its one pane: `worker:<worker_id>`.
pub(crate) fn worker_tab_id(worker_id: &str) -> String {
    format!("{WORKER_TAB_PREFIX}{worker_id}")
}

/// The worker whose tab or pane `id` names.
pub(crate) fn worker_of_tab_id(id: &str) -> Option<&str> {
    id.strip_prefix(WORKER_TAB_PREFIX)
        .filter(|worker| !worker.is_empty())
}

const WORKER_TAB_PREFIX: &str = "worker:";

/// Where a reader of a journal is: the bytes and the whole lines read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct JournalPosition {
    pub(crate) byte: u64,
    pub(crate) line: u64,
}

/// The events of the whole lines from `position` on, after line `after`
/// and at most `limit` of them, with the position past the last line read
/// and whether whole lines are left. A line the server has not finished
/// writing stays for the next read.
pub(crate) fn read_journal_events(
    path: &Path,
    position: JournalPosition,
    after: u64,
    limit: Option<u32>,
    worker_id: &str,
    run_id: Option<&str>,
) -> std::io::Result<(Vec<WorkerTranscriptEvent>, JournalPosition, bool)> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        // Nothing journaled yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), position, false))
        }
        Err(error) => return Err(error),
    };
    // A journal never shrinks; one that did was replaced: read it anew.
    let position = if file.metadata()?.len() < position.byte {
        JournalPosition::default()
    } else {
        position
    };
    file.seek(SeekFrom::Start(position.byte))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut events = Vec::new();
    let mut at = position;
    let mut taken = 0u32;
    let mut more = false;
    let mut rest = bytes.as_slice();
    while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
        if at.line >= after && limit.is_some_and(|limit| taken >= limit) {
            more = true;
            break;
        }
        let line = &rest[..end];
        rest = &rest[end + 1..];
        at.byte += end as u64 + 1;
        at.line += 1;
        if at.line <= after {
            continue;
        }
        taken += 1;
        let text = String::from_utf8_lossy(line);
        let record = serde_json::from_str::<Value>(&text).ok();
        let entries = match &record {
            Some(record) => super::log::record_value_entries(record),
            None => super::log::record_entries(&text),
        };
        let seq = record.as_ref().and_then(|record| record["seq"].as_i64());
        let ts_ms = record
            .as_ref()
            .and_then(|record| record["ts_ms"].as_u64())
            .unwrap_or(0);
        events.extend(entries.into_iter().map(|entry| WorkerTranscriptEvent {
            worker_id: worker_id.to_owned(),
            run_id: run_id.map(str::to_owned),
            line: at.line,
            seq,
            ts_ms,
            entry,
        }));
    }
    Ok((events, at, more))
}

/// The tab a worker shows in.
pub(crate) fn worker_tab(worker: &WorkerInfo) -> WorkerTab {
    WorkerTab {
        tab_id: worker_tab_id(&worker.worker_id),
        pane_id: worker_tab_id(&worker.worker_id),
        pane_kind: PaneKind::Worker,
        workspace_id: worker.workspace_id.clone(),
        read_only: true,
        takeover_tab_id: worker.takeover_tab_id.clone(),
    }
}

/// Called after every record a worker's journal gets, so the server pushes
/// the new lines to the client shells that show that worker's tab. It only
/// wakes the server: [`take_appended`] says whether anything was appended
/// since the last look.
type AppendNotifier = Arc<dyn Fn() -> bool + Send + Sync>;

/// The server's wake for appended journal records: one per process, the
/// server's. A test builds its own so parallel tests never share it.
struct AppendWake {
    notifier: Mutex<Option<AppendNotifier>>,
    appended: AtomicBool,
}

impl AppendWake {
    const fn new() -> Self {
        Self {
            notifier: Mutex::new(None),
            appended: AtomicBool::new(false),
        }
    }

    fn set_notifier(&self, notifier: AppendNotifier) {
        *lock(&self.notifier) = Some(notifier);
    }

    fn take(&self) -> bool {
        self.appended.swap(false, Ordering::AcqRel)
    }

    fn note(&self) {
        if self.appended.swap(true, Ordering::AcqRel) {
            return;
        }
        let notifier = lock(&self.notifier).clone();
        if let Some(notifier) = notifier {
            if !notifier() {
                self.appended.store(false, Ordering::Release);
            }
        }
    }
}

static APPEND_WAKE: AppendWake = AppendWake::new();

/// `notifier` wakes the server; it returns false when the wake was not
/// delivered, so the next append tries again.
pub(crate) fn set_append_notifier(notifier: AppendNotifier) {
    APPEND_WAKE.set_notifier(notifier);
}

/// Whether a journal got a record since the last call; the server reads
/// the clients' transcripts again when it did.
pub(crate) fn take_appended() -> bool {
    APPEND_WAKE.take()
}

/// Wakes the server once per batch of appends: a wake already on its way
/// covers the records that follow it.
pub(super) fn note_appended() {
    APPEND_WAKE.note();
}

impl WorkerSupervisor {
    /// The `herdr todo run` whose worker this is, if any.
    pub(crate) fn todo_run_of_worker(&self, worker_id: &str) -> Option<String> {
        let store = self.run_store().ok()?;
        store
            .runs(None)
            .ok()?
            .into_iter()
            .find(|run| run.info.worker_id.as_deref() == Some(worker_id))
            .map(|run| run.info.run_id)
    }

    /// `worker.transcript`: the events of the journal lines after
    /// `params.after`, at most `params.limit` lines.
    pub(crate) fn transcript(
        &self,
        params: &WorkerTranscriptParams,
    ) -> Result<WorkerTranscript, WorkerError> {
        let worker = self.status(&params.worker_id)?;
        let run_id = self.todo_run_of_worker(&params.worker_id);
        let (transcript, _) = self.transcript_from(
            &worker,
            run_id,
            JournalPosition::default(),
            params.after.unwrap_or(0),
            params.limit,
        )?;
        Ok(transcript)
    }

    /// The transcript of `worker`'s journal lines after `after`, read from
    /// `position` on, and the position past what it read.
    pub(crate) fn transcript_from(
        &self,
        worker: &WorkerInfo,
        run_id: Option<String>,
        position: JournalPosition,
        after: u64,
        limit: Option<u32>,
    ) -> Result<(WorkerTranscript, JournalPosition), WorkerError> {
        let (events, position, more) = read_journal_events(
            Path::new(&worker.journal_path),
            position,
            after,
            limit,
            &worker.worker_id,
            run_id.as_deref(),
        )?;
        Ok((
            WorkerTranscript {
                worker_id: worker.worker_id.clone(),
                run_id,
                tab: worker_tab(worker),
                state: worker.state,
                events,
                cursor: position.line.max(after),
                more,
            },
            position,
        ))
    }

    /// `worker.transcript_wait`: blocks until the journal has lines after
    /// `params.after` or the worker is gone, then answers as
    /// [`Self::transcript`]. Woken by the worker's events, which its journal
    /// records under the same lock; returns `None` when the caller gave up.
    pub(crate) fn transcript_wait(
        &self,
        params: &WorkerTranscriptParams,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<WorkerTranscript>, WorkerError> {
        let after = params.after.unwrap_or(0);
        let run_id = self.todo_run_of_worker(&params.worker_id);
        loop {
            // The `seq` before reading: an event between the two changes it,
            // so the wait below returns at once instead of missing it.
            let worker = self.status(&params.worker_id)?;
            let seen = worker.seq;
            let (transcript, _) = self.transcript_from(
                &worker,
                run_id.clone(),
                JournalPosition::default(),
                after,
                params.limit,
            )?;
            let gone = matches!(worker.state, WorkerState::Exited | WorkerState::Lost);
            if !transcript.events.is_empty() || transcript.cursor > after || gone {
                return Ok(Some(transcript));
            }
            let changed = self.wait_on(
                &params.worker_id,
                liveness_check,
                &mut keep_waiting,
                |entry| {
                    (Some(entry.status.last_seq) != seen || entry.status.is_gone()).then_some(())
                },
            )?;
            if changed.is_none() {
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{WorkerTranscriptEntry, WorkerTranscriptRole};

    /// A directory of its own under the temp dir, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "herdr-transcript-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn journal(lines: &[serde_json::Value]) -> (TempDir, PathBuf) {
        let dir = TempDir::new();
        let path = dir.path().join("w1.jsonl");
        let mut text = String::new();
        for line in lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).expect("journal");
        (dir, path)
    }

    #[test]
    fn reads_structured_events_tagged_with_worker_and_run() {
        let (_dir, path) = journal(&[
            json!({"ts_ms": 5, "dir": "in", "seq": 1,
                   "event": {"type": "user", "message": {"content": "fix it"}}}),
            json!({"ts_ms": 6, "dir": "out", "seq": 2, "event": {"type": "assistant",
            "message": {"content": [
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "name": "Bash", "input": {"command": "git status"}}
            ]}}}),
            json!({"ts_ms": 7, "dir": "out", "seq": 3, "event": {"type": "user",
                   "message": {"content": [{"type": "tool_result", "content": "clean",
                                            "is_error": true}]}}}),
            json!({"ts_ms": 8, "dir": "herdr", "seq": 4, "event": {"type": "exited", "code": 0}}),
        ]);
        let (events, position, more) = read_journal_events(
            &path,
            JournalPosition::default(),
            0,
            None,
            "w1",
            Some("r-abc"),
        )
        .expect("read");
        assert!(!more);
        assert_eq!(position.line, 4);
        assert!(events
            .iter()
            .all(|event| event.worker_id == "w1" && event.run_id.as_deref() == Some("r-abc")));
        let entries: Vec<_> = events.iter().map(|event| event.entry.clone()).collect();
        assert_eq!(
            entries,
            vec![
                WorkerTranscriptEntry::Message {
                    role: WorkerTranscriptRole::User,
                    text: "fix it".into()
                },
                WorkerTranscriptEntry::Message {
                    role: WorkerTranscriptRole::Assistant,
                    text: "Looking.".into()
                },
                WorkerTranscriptEntry::ToolCall {
                    name: "Bash".into(),
                    input: "git status".into()
                },
                WorkerTranscriptEntry::ToolResult {
                    text: "clean".into(),
                    is_error: true
                },
                WorkerTranscriptEntry::Status {
                    text: "■ exited with code 0".into()
                },
            ]
        );
        assert_eq!(events[1].line, 2);
        assert_eq!(events[1].seq, Some(2));
        assert_eq!(events[1].ts_ms, 6);
    }

    #[test]
    fn backfills_in_pages_and_continues_from_the_cursor() {
        let record = |n: u64| json!({"ts_ms": n, "dir": "herdr", "event": {"type": "signal", "signal": format!("S{n}")}});
        let (_dir, path) = journal(&[record(1), record(2), record(3)]);
        let (first, position, more) =
            read_journal_events(&path, JournalPosition::default(), 0, Some(2), "w1", None)
                .expect("first page");
        assert!(more);
        assert_eq!(first.len(), 2);
        let (rest, end, more) =
            read_journal_events(&path, position, 0, None, "w1", None).expect("the rest");
        assert!(!more);
        assert_eq!(end.line, 3);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].line, 3);
        // Skipping by line count reads the same as resuming from the bytes.
        let (skipped, _, _) =
            read_journal_events(&path, JournalPosition::default(), 2, None, "w1", None)
                .expect("skip");
        assert_eq!(skipped, rest);
    }

    #[test]
    fn a_half_written_line_waits_for_its_end() {
        let (_dir, path) = journal(&[json!({"ts_ms": 1, "dir": "herdr",
                                            "event": {"type": "signal", "signal": "S"}})]);
        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(b"{\"ts_ms\": 2, \"dir\"").expect("half");
        let (events, position, _) =
            read_journal_events(&path, JournalPosition::default(), 0, None, "w1", None)
                .expect("read");
        assert_eq!(events.len(), 1);
        file.write_all(b": \"err\", \"raw\": \"boom\"}\n")
            .expect("rest");
        let (events, end, _) =
            read_journal_events(&path, position, 0, None, "w1", None).expect("read on");
        assert_eq!(end.line, 2);
        assert_eq!(
            events[0].entry,
            WorkerTranscriptEntry::Status {
                text: "stderr: boom".into()
            }
        );
    }

    #[test]
    fn a_missing_journal_is_an_empty_transcript() {
        let dir = TempDir::new();
        let (events, position, more) = read_journal_events(
            &dir.path().join("none.jsonl"),
            JournalPosition::default(),
            0,
            None,
            "w1",
            None,
        )
        .expect("read");
        assert!(events.is_empty() && !more);
        assert_eq!(position, JournalPosition::default());
    }

    #[test]
    fn the_tab_identity_names_its_worker() {
        assert_eq!(worker_tab_id("w7"), "worker:w7");
        assert_eq!(worker_of_tab_id("worker:w7"), Some("w7"));
        assert_eq!(worker_of_tab_id("worker:"), None);
        assert_eq!(worker_of_tab_id("w1:t1"), None);
    }

    #[test]
    fn every_append_wakes_the_server_once_until_it_looks() {
        let wake = AppendWake::new();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        wake.set_notifier(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            true
        }));
        wake.note();
        wake.note();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(wake.take());
        assert!(!wake.take());
        wake.note();
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }
}
