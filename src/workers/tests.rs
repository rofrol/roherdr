//! Supervisor tests against a stub `claude` that speaks stream-json. No real
//! Claude runs here. The stub's behavior is chosen by each user message:
//! `finish`, `fail`, `crash`, `block` (until an interrupt), `perm <tool>
//! <words...>` (one `can_use_tool` request whose path or command is the
//! words), `classifier <tool> <words...>` (the same, escalated by the auto
//! mode classifier), `refuse` (a model refusal, then `result/success`),
//! `refuse-wait` (a refusal, then the next input line, then the result),
//! `exit1` (`result/success`, then exit code 1), `done <sha>` (a reply
//! ending with `WORKER-DONE <sha> | summary`), `denials` (a result with
//! `permission_denials`), `ask` (an `AskUserQuestion` request), `pair <tool>
//! <words...>` (two requests at once, `perm-1` and `perm-2`), `cancel <tool>
//! <words...>` (`perm-1`, cancelled, then `perm-2`), `two <tool> <words...>`
//! (classifier-escalated `perm-1`, its answer, then `perm-2`), `ignore-term` (SIGTERM is
//! ignored from then on), `orphan <fifo>` (a tool process in its own
//! session that holds `<fifo>` open until it dies) and `gate <fifo>` (a tool
//! use, then, once `<fifo>` is written, three text events, a stderr line and
//! the result) and `gate-perm <fifo> <tool> <words...>` (`perm`, once
//! `<fifo>` is written), `commit <file> <subject...>` (appends to
//! `<file>`, commits it with that subject and ends with `WORKER-DONE <sha>`),
//! `amend <file> <subject...>` (the same, amending `HEAD` instead),
//! `commit2 <file> <subject...>` (`commit` twice, the second changing what
//! the first added),
//! `perm-commit <file> <subject...>` (`perm WebFetch`, then `commit`),
//! `stubborn-commit <file> <subject...>` (`ignore-term`, then `commit`) and
//! `hook <tool> <words...>` (a PreToolUse `hook_callback` for the hook an
//! `initialize` request registered for that tool, whose command is the
//! words; the result is its answer as JSON, or `unregistered`) and
//! `hook-bg <tool> <words...>` (the same with `run_in_background: true`).
//! A message with `Review of attempt N:` lines (a todo run's later attempt)
//! is chosen by the line after the last one instead.
#![cfg(unix)]

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::*;
use crate::api::schema::{
    WorkerAnswerParams, WorkerCommandTarget, WorkerDecision, WorkerPromptParams,
    WorkerQuestionKind, WorkerQuestionState,
};

const STUB: &str = r#"#!/usr/bin/env python3
import json, os, signal, subprocess, sys, time

def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()

def result(subtype="success", is_error=False, reason="completed", text="ok"):
    emit({"type": "result", "subtype": subtype, "is_error": is_error,
          "terminal_reason": reason, "api_error_status": None, "result": text,
          "session_id": "stub-session"})

emit({"type": "system", "subtype": "init", "session_id": "stub-session",
      "herdr_env": sorted(k for k in os.environ if k.startswith("HERDR_")),
      "cwd": os.getcwd(),
      "cache_env": {k: os.environ.get(k) for k in
                    ("CARGO_TARGET_DIR", "ZIG_GLOBAL_CACHE_DIR", "ZIG_LOCAL_CACHE_DIR")}})

ignore_term = False
hooks = {}

def commit(path, subject, amend=False):
    with open(path, "a") as f:
        f.write("change\n")
    git = ["git", "-c", "user.name=t", "-c", "user.email=t@example.com",
           "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"]
    subprocess.run(git + ["add", path], check=True)
    subprocess.run(git + ["commit", "-q"] + (["--amend"] if amend else []) + ["-m", subject],
                   check=True)
    sha = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True,
                         check=True).stdout.strip()
    result(text="committed\nWORKER-DONE " + sha + " | summary")

def read():
    line = sys.stdin.readline()
    if not line:
        if ignore_term:
            # Still alive after SIGTERM and the closed input: only
            # SIGKILL ends it.
            while True:
                time.sleep(1000)
        sys.exit(0)
    return json.loads(line)

def ask_host(tool, tool_input, reason_type=None):
    request = {"subtype": "can_use_tool", "tool_name": tool, "input": tool_input}
    if reason_type:
        request["decision_reason_type"] = reason_type
    emit({"type": "control_request", "request_id": "perm-1", "request": request})
    answer = read()
    response = answer["response"]
    assert response["request_id"] == "perm-1", answer
    return response["response"]

def hook_callback(tool, command, background=False):
    import re
    tool_input = {"command": command}
    if background:
        tool_input["run_in_background"] = True
    for entry in hooks.get("PreToolUse", []):
        if re.fullmatch(entry.get("matcher") or ".*", tool):
            emit({"type": "control_request", "request_id": "hook-1", "request": {
                "subtype": "hook_callback", "callback_id": entry["hookCallbackIds"][0],
                "tool_use_id": "toolu-1",
                "input": {"hook_event_name": "PreToolUse", "tool_name": tool,
                          "tool_input": tool_input}}})
            response = read()["response"]
            assert response["request_id"] == "hook-1", response
            return json.dumps(response["response"], sort_keys=True)
    return "unregistered"

while True:
    message = read()
    if message.get("type") == "control_request" and \
            message["request"].get("subtype") == "initialize":
        hooks = message["request"].get("hooks") or {}
        emit({"type": "control_response", "response": {
            "subtype": "success", "request_id": message["request_id"], "response": {}}})
        continue
    if message.get("type") != "user":
        continue
    # The first line chooses: a todo run appends its contract below it. A
    # later attempt's task ends with the review, whose first line chooses.
    lines = message["message"]["content"].split("\n")
    reviews = [i for i, line in enumerate(lines) if line.startswith("Review of attempt ")]
    words = (lines[reviews[-1] + 1] if reviews else lines[0]).split()
    emit({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}})
    command = words[0]
    if command == "finish":
        result()
    elif command in ("hook", "hook-bg"):
        result(text=hook_callback(words[1], " ".join(words[2:]), command == "hook-bg"))
    elif command == "done":
        result(text="work done\nWORKER-DONE " + words[1] + " | summary")
    elif command == "commit":
        commit(words[1], " ".join(words[2:]))
    elif command == "amend":
        commit(words[1], " ".join(words[2:]), amend=True)
    elif command == "commit2":
        with open(words[1], "a") as f:
            f.write("first\n")
        subprocess.run(["git", "add", words[1]], check=True)
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.com",
                        "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                        "commit", "-q", "-m", "first"], check=True)
        commit(words[1], " ".join(words[2:]))
    elif command == "perm-commit":
        ask_host("WebFetch", {"url": "https://example.com"})
        commit(words[1], " ".join(words[2:]))
    elif command == "stubborn-commit":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        ignore_term = True
        commit(words[1], " ".join(words[2:]))
    elif command == "fail":
        result("error_during_execution", True, "model_error", None)
    elif command == "crash":
        sys.exit(3)
    elif command == "block":
        emit({"type": "assistant", "message": {"content": [{"type": "tool_use"}]}})
        while True:
            request = read()
            if request.get("type") == "control_request":
                emit({"type": "control_response", "response": {
                    "subtype": "success", "request_id": request["request_id"],
                    "response": {"still_queued": []}}})
                result("error_during_execution", True, "aborted_tools", None)
                break
    elif command == "refuse":
        emit({"type": "system", "subtype": "model_refusal_no_fallback",
              "api_refusal_category": "cyber"})
        result()
    elif command == "refuse-wait":
        emit({"type": "system", "subtype": "model_refusal_no_fallback",
              "api_refusal_category": "cyber"})
        read()
        result()
    elif command == "exit1":
        result()
        sys.exit(1)
    elif command == "denials":
        emit({"type": "result", "subtype": "success", "is_error": False,
              "terminal_reason": "completed", "result": "ok",
              "permission_denials": [{"tool_name": "Write", "tool_use_id": "t1",
                                      "tool_input": {"file_path": "/outside"}}]})
    elif command in ("perm", "classifier"):
        rest = " ".join(words[2:])
        response = ask_host(words[1], {"file_path": rest, "command": rest},
                            "classifier" if command == "classifier" else None)
        text = response["behavior"]
        if text == "deny":
            text += ": " + response["message"]
        result(text=text)
    elif command in ("pair", "cancel"):
        rest = " ".join(words[2:])
        request = {"subtype": "can_use_tool", "tool_name": words[1],
                   "input": {"file_path": rest, "command": rest}}
        emit({"type": "control_request", "request_id": "perm-1", "request": request})
        if command == "cancel":
            emit({"type": "control_cancel_request", "request_id": "perm-1"})
            ids = ["perm-2"]
        else:
            ids = ["perm-1", "perm-2"]
        emit({"type": "control_request", "request_id": "perm-2", "request": request})
        behaviors = {}
        while len(behaviors) < len(ids):
            response = read()["response"]
            behaviors[response["request_id"]] = response["response"]["behavior"]
        result(text=" ".join(f"{id}={behaviors[id]}" for id in ids))
    elif command == "two":
        rest = " ".join(words[2:])
        behaviors = []
        for request_id in ("perm-1", "perm-2"):
            emit({"type": "control_request", "request_id": request_id, "request": {
                "subtype": "can_use_tool", "tool_name": words[1],
                "input": {"file_path": rest, "command": rest},
                "decision_reason_type": "classifier"}})
            response = read()["response"]
            assert response["request_id"] == request_id, response
            behaviors.append(response["response"]["behavior"])
        result(text=" ".join(behaviors))
    elif command == "ask":
        response = ask_host("AskUserQuestion", {"questions": [
            {"question": "Which file?", "header": "File", "multiSelect": False,
             "options": [{"label": "alpha.txt", "description": "a"},
                         {"label": "blue.txt", "description": "b"}]},
            {"question": "Which colors?", "header": "Colors", "multiSelect": True,
             "options": [{"label": "green", "description": "g"},
                         {"label": "red", "description": "r"}]}]})
        if response["behavior"] == "allow":
            result(text=json.dumps(response["updatedInput"]["answers"], sort_keys=True))
        else:
            result(text="deny: " + response["message"])
    elif command == "ignore-term":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        ignore_term = True
        result()
    elif command == "orphan":
        subprocess.Popen([sys.executable, "-c",
            "import sys, time; f = open(sys.argv[1], 'w'); time.sleep(1000)", words[1]],
            start_new_session=True, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        # The tool runs; this event lets the supervisor see its session.
        emit({"type": "assistant", "message": {"content": [{"type": "tool_use"}]}})
        while True:
            read()
    elif command == "gate-perm":
        with open(words[1]) as gate:
            gate.read()
        rest = " ".join(words[3:])
        response = ask_host(words[2], {"file_path": rest, "command": rest})
        result(text=response["behavior"])
    elif command == "gate":
        emit({"type": "assistant", "message": {"content": [{"type": "tool_use"}]}})
        with open(words[1]) as gate:
            gate.read()
        for n in range(3):
            emit({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "line %d" % n}]}})
        sys.stderr.write("gated stderr\n")
        sys.stderr.flush()
        result(text="gated")
"#;

/// Fails a broken test instead of hanging the suite; never decides an
/// outcome a passing test depends on.
const HANG_GUARD: Duration = Duration::from_secs(60);

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    supervisor: WorkerSupervisor,
    /// Dropped after the supervisor's workers are killed: ends and reaps the
    /// brokers that are left.
    brokers: broker::tests::Reaper,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-workers-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        Self::at(root, None)
    }

    /// A fixture whose supervisor starts each worker through a broker (this
    /// test binary as one). Its root is short: a socket's path has at most
    /// 103 bytes.
    fn with_broker() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "hb{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        Self::at(root, Some(broker::tests::test_launcher()))
    }

    fn at(root: PathBuf, broker: Option<broker::Launcher>) -> Self {
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        if broker.is_some() {
            broker::tests::require_unix_sockets(&root);
        }
        let stub = root.join("claude-stub");
        std::fs::write(&stub, STUB).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let supervisor = WorkerSupervisor::open_with(root.join("workers"), stub, broker);
        Self {
            brokers: broker::tests::Reaper::new(&root),
            root,
            repo,
            supervisor,
        }
    }

    fn start(&self, prompt: &str) -> String {
        self.supervisor
            .start(&start_params(&self.repo, prompt, Some("stub-model")))
            .unwrap()
            .worker_id
    }

    fn wait(&self, worker_id: &str, until: WorkerWaitUntil) -> WorkerInfo {
        let started = Instant::now();
        self.supervisor
            .wait(worker_id, until, Duration::from_millis(100), || {
                assert!(started.elapsed() < HANG_GUARD, "worker {worker_id} hung");
                true
            })
            .unwrap()
            .unwrap()
    }

    /// Waits, woken by state changes, until the worker's status satisfies
    /// `done`.
    fn wait_for(&self, worker_id: &str, done: impl Fn(&WorkerInfo) -> bool) -> WorkerInfo {
        let started = Instant::now();
        loop {
            let worker = self.supervisor.status(worker_id).unwrap();
            if done(&worker) {
                return worker;
            }
            assert!(started.elapsed() < HANG_GUARD, "worker {worker_id} hung");
            let registry = lock(&self.supervisor.shared.registry);
            drop(
                self.supervisor
                    .shared
                    .changed
                    .wait_timeout(registry, Duration::from_millis(100)),
            );
        }
    }

    /// Waits, woken by state changes, until the worker's internal status
    /// satisfies `done`.
    fn wait_for_status(&self, worker_id: &str, done: impl Fn(&Status) -> bool) {
        let started = Instant::now();
        let number = worker_number(worker_id).unwrap();
        let mut registry = lock(&self.supervisor.shared.registry);
        while !done(&registry.workers[&number].status) {
            assert!(started.elapsed() < HANG_GUARD, "worker {worker_id} hung");
            registry = self
                .supervisor
                .shared
                .changed
                .wait_timeout(registry, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }

    /// `worker.wait --attention --after`, with the liveness re-check as long
    /// as the hang guard, so a missed wake fails the test. `on_block` runs
    /// at the first point the wait would block (its first liveness check),
    /// in the window between its check and its block. Returns the attention
    /// and how many times the wait got that far.
    fn attention(
        &self,
        worker_id: &str,
        after: Option<i64>,
        on_block: impl FnMut(),
    ) -> (Attention, usize) {
        attention_on(&self.supervisor, worker_id, after, on_block)
    }

    fn wait_for_question(&self, worker_id: &str) -> WorkerInfo {
        self.wait_for(worker_id, |worker| !worker.questions.is_empty())
    }

    fn answer(
        &self,
        worker_id: &str,
        decision: Option<WorkerDecision>,
        answers: &[&str],
    ) -> Result<WorkerInfo, WorkerError> {
        self.supervisor.answer(&WorkerAnswerParams {
            worker_id: worker_id.to_owned(),
            request_id: None,
            decision,
            answers: answers.iter().map(|answer| (*answer).to_owned()).collect(),
            message: None,
            command_id: None,
        })
    }

    fn answer_request(
        &self,
        worker_id: &str,
        request_id: &str,
        decision: WorkerDecision,
    ) -> Result<WorkerInfo, WorkerError> {
        self.supervisor.answer(&WorkerAnswerParams {
            worker_id: worker_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            decision: Some(decision),
            answers: Vec::new(),
            message: None,
            command_id: None,
        })
    }

    fn journal(&self, worker_id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.root.join("workers").join(format!("{worker_id}.jsonl")))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn herdr_events(&self, worker_id: &str, kind: &str) -> Vec<Value> {
        self.journal(worker_id)
            .into_iter()
            .filter(|record| record["dir"] == "herdr" && record["event"]["type"] == kind)
            .map(|record| record["event"].clone())
            .collect()
    }
}

/// [`Fixture::attention`] on any supervisor (another server).
fn attention_on(
    supervisor: &WorkerSupervisor,
    worker_id: &str,
    after: Option<i64>,
    mut on_block: impl FnMut(),
) -> (Attention, usize) {
    let started = Instant::now();
    let mut blocked = 0;
    let attention = supervisor
        .wait_attention(worker_id, after, HANG_GUARD, || {
            assert!(started.elapsed() < HANG_GUARD, "worker {worker_id} hung");
            blocked += 1;
            if blocked == 1 {
                on_block();
            }
            true
        })
        .unwrap()
        .unwrap();
    (attention, blocked)
}

fn start_params(repo: &Path, prompt: &str, model: Option<&str>) -> WorkerStartParams {
    WorkerStartParams {
        cwd: repo.display().to_string(),
        prompt: prompt.to_owned(),
        model: model.map(str::to_owned),
        name: None,
        workspace_id: None,
        folder_slot: None,
        branch: None,
        base: None,
        fresh_build: false,
        owner_pane_id: None,
        owner_session_id: None,
        item: None,
        command_id: None,
    }
}

/// A supervisor whose worker ran one `finish` turn, for tests outside this
/// module (the server's worker tab); its directory goes when dropped.
pub(crate) struct FinishedWorker {
    fixture: Fixture,
    pub(crate) worker_id: String,
}

impl FinishedWorker {
    pub(crate) fn new(name: &str) -> Self {
        let fixture = Fixture::new(name);
        let worker_id = fixture.start("finish");
        fixture.wait(&worker_id, WorkerWaitUntil::TurnEnd);
        Self { fixture, worker_id }
    }

    pub(crate) fn supervisor(&self) -> &WorkerSupervisor {
        &self.fixture.supervisor
    }

    /// Another `finish` turn, returned once it ended.
    pub(crate) fn run_another_turn(&self) {
        self.fixture
            .supervisor
            .prompt(&self.worker_id, "finish")
            .unwrap();
        self.fixture.wait(&self.worker_id, WorkerWaitUntil::TurnEnd);
    }

    pub(crate) fn journal_lines(&self) -> u64 {
        self.fixture.journal(&self.worker_id).len() as u64
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A failed test (a hang guard fired) may have left the supervisor
        // stuck: end the brokers first, whose death guards end their
        // workers, so nothing below can keep one alive past the test.
        if std::thread::panicking() {
            self.brokers.reap_now();
        }
        for worker in self.supervisor.list() {
            if !matches!(worker.state, WorkerState::Exited | WorkerState::Lost) {
                let _ = self.supervisor.kill(&worker.worker_id, false);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_turn_finishes_and_the_next_prompt_runs_another() {
    let fixture = Fixture::new("finish");
    let id = fixture.start("finish");

    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.session_id.as_deref(), Some("stub-session"));
    assert_eq!(worker.model.as_deref(), Some("stub-model"));
    assert_eq!(worker.turns, 1);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("ok"));
    assert_eq!(worker.rate_limit.unwrap()["status"], "allowed");

    fixture.supervisor.prompt(&id, "fail").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Failed);
    assert_eq!(worker.turns, 2);

    let journal = fixture.journal(&id);
    let started = &journal[0]["event"];
    assert_eq!(started["type"], "started");
    let args: Vec<&str> = started["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    for flag in [
        "--permission-prompt-tool",
        "--replay-user-messages",
        "--settings",
    ] {
        assert!(args.contains(&flag), "{flag} missing from {args:?}");
    }
    let init = journal
        .iter()
        .find(|record| record["event"]["subtype"] == "init")
        .unwrap();
    assert_eq!(init["event"]["herdr_env"], serde_json::json!([]));
    assert!(journal.iter().any(|record| record["dir"] == "in"));
}

/// How often a transcript wait in these tests looks whether its caller
/// gave up; the worker's events end the wait.
// delay: not a wait, the caller-liveness check interval, as in Fixture::wait.
const TRANSCRIPT_LIVENESS: Duration = Duration::from_millis(100);

#[test]
fn the_transcript_backfills_structured_events_and_waits_for_new_lines() {
    use crate::api::schema::{
        PaneKind, WorkerTranscriptEntry, WorkerTranscriptParams, WorkerTranscriptRole,
    };
    let fixture = Fixture::new("transcript");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);

    let params = |after: Option<u64>| WorkerTranscriptParams {
        worker_id: id.clone(),
        after,
        limit: None,
    };
    let transcript = fixture.supervisor.transcript(&params(None)).unwrap();
    assert_eq!(transcript.tab.tab_id, format!("worker:{id}"));
    assert_eq!(transcript.tab.pane_id, format!("worker:{id}"));
    assert_eq!(transcript.tab.pane_kind, PaneKind::Worker);
    assert!(transcript.tab.read_only);
    assert_eq!(transcript.state, WorkerState::Finished);
    assert_eq!(transcript.cursor, fixture.journal(&id).len() as u64);
    assert!(transcript.events.iter().all(|event| event.worker_id == id));
    assert!(transcript.events.iter().any(|event| event.entry
        == WorkerTranscriptEntry::Message {
            role: WorkerTranscriptRole::User,
            text: "finish".into(),
        }));
    // The same lines `herdr worker log` prints.
    let rendered: Vec<String> = transcript
        .events
        .iter()
        .flat_map(|event| log::entry_lines(&event.entry))
        .collect();
    let logged: Vec<String> = fixture
        .journal(&id)
        .iter()
        .flat_map(|record| log::log_lines(&record.to_string()))
        .collect();
    assert_eq!(rendered, logged);

    // Paged: the first line only, then the rest from its cursor.
    let first = fixture
        .supervisor
        .transcript(&WorkerTranscriptParams {
            limit: Some(1),
            ..params(None)
        })
        .unwrap();
    assert!(first.more);
    assert_eq!(first.cursor, 1);
    let rest = fixture.supervisor.transcript(&params(Some(1))).unwrap();
    assert_eq!(
        first.events.len() + rest.events.len(),
        transcript.events.len()
    );

    // Lines already there return at once; none past the cursor waits for
    // the next turn's.
    let started = Instant::now();
    let at_once = fixture
        .supervisor
        .transcript_wait(&params(Some(0)), TRANSCRIPT_LIVENESS, || {
            assert!(started.elapsed() < HANG_GUARD, "transcript wait hung");
            true
        })
        .unwrap()
        .unwrap();
    assert_eq!(at_once.cursor, transcript.cursor);
    let cursor = transcript.cursor;
    let waiter = {
        let supervisor = fixture.supervisor.clone();
        let params = params(Some(cursor));
        std::thread::spawn(move || {
            let started = Instant::now();
            supervisor
                .transcript_wait(&params, TRANSCRIPT_LIVENESS, || {
                    started.elapsed() < HANG_GUARD
                })
                .unwrap()
        })
    };
    fixture.supervisor.prompt(&id, "finish").unwrap();
    let waited = waiter.join().unwrap().expect("new lines");
    assert!(waited.cursor > cursor);
    assert!(waited.events.iter().all(|event| event.line > cursor));
}

/// The value after `flag` in the worker's launch arguments.
fn launch_arg(journal: &[Value], flag: &str) -> String {
    let args = journal[0]["event"]["args"].as_array().unwrap();
    let index = args.iter().position(|arg| arg == flag).unwrap();
    args[index + 1].as_str().unwrap().to_owned()
}

#[test]
fn a_worker_runs_in_the_sandbox_with_its_own_temp_dir() {
    let fixture = Fixture::new("sandbox");
    let id = fixture.start("finish");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);

    let journal = fixture.journal(&id);
    let temp = fixture
        .root
        .join("workers/tmp")
        .join(&id)
        .canonicalize()
        .unwrap();
    let cwd = fixture.repo.canonicalize().unwrap();
    assert_eq!(journal[0]["event"]["temp_dir"], temp.display().to_string());
    assert_eq!(launch_arg(&journal, "--permission-mode"), "manual");

    let settings: Value = serde_json::from_str(&launch_arg(&journal, "--settings")).unwrap();
    let mut deny_read: Vec<String> = [
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.config/gh",
        "~/.claude/.credentials.json",
        "~/.git-credentials",
        "~/.netrc",
        "~/.npmrc",
        "~/.docker",
        "~/.kube",
        "~/.cargo/credentials",
        "~/.cargo/credentials.toml",
    ]
    .map(str::to_owned)
    .to_vec();
    for glob in ["**/.env", "**/.env.*", "**/.envrc"] {
        deny_read.push(cwd.join(glob).display().to_string());
    }
    let mut deny_rules = Vec::new();
    for pattern in [
        "~/.ssh/**",
        "~/.aws/**",
        "~/.gnupg/**",
        "~/.config/gh/**",
        "~/.claude/.credentials.json",
        "~/.git-credentials",
        "~/.netrc",
        "~/.npmrc",
        "~/.docker/**",
        "~/.kube/**",
        "~/.cargo/credentials",
        "~/.cargo/credentials.toml",
        "**/.env",
        "**/.env.*",
        "**/.envrc",
    ] {
        deny_rules.push(format!("Read({pattern})"));
        deny_rules.push(format!("Edit({pattern})"));
    }
    deny_rules.push("WebFetch".into());
    deny_rules.push("WebSearch".into());
    assert_eq!(
        settings,
        serde_json::json!({
            "disableAllHooks": true,
            "attribution": {"commit": "", "pr": "", "sessionUrl": false},
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "autoAllowBashIfSandboxed": true,
                "allowUnsandboxedCommands": false,
                "filesystem": {
                    "allowWrite": [temp.display().to_string()],
                    "denyRead": deny_read,
                },
                "network": {"allowedDomains": [], "strictAllowlist": true},
            },
            "permissions": {"deny": deny_rules},
        })
    );
    assert!(journal[0]["event"]["removed_env"].is_array());
    let contract = launch_arg(&journal, "--append-system-prompt");
    assert!(
        contract.contains(&format!("under {} by that absolute path", temp.display())),
        "{contract}"
    );
    assert!(contract.contains("never use $TMPDIR"), "{contract}");
    let policy = &fixture.herdr_events(&id, "policy")[0];
    assert_eq!(
        policy["file_tool_roots"],
        serde_json::json!([cwd.display().to_string(), temp.display().to_string()])
    );

    // The temp dir is private, lives as long as the worker and goes with it.
    assert!(temp.is_dir());
    for dir in [&temp, &temp.parent().unwrap().to_path_buf()] {
        let mode = std::fs::metadata(dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
    }
    std::fs::write(temp.join("draft.txt"), "x").unwrap();
    fixture.supervisor.stop(&id).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    assert!(!temp.exists());

    let crashed = fixture.start("crash");
    let crashed_temp = fixture.root.join("workers/tmp").join(&crashed);
    fixture.wait(&crashed, WorkerWaitUntil::Exit);
    assert!(!crashed_temp.exists());
}

#[test]
fn a_restart_removes_temp_dirs_only_of_workers_whose_process_is_gone() {
    let root = std::env::temp_dir().join(format!(
        "herdr-workers-temp-restart-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let dir = root.join("workers");
    // A process group that is still alive, as an old server's worker during
    // a live handoff would be.
    let mut alive = std::process::Command::new("sleep");
    alive.arg("1000");
    std::os::unix::process::CommandExt::process_group(&mut alive, 0);
    let mut alive = alive.spawn().unwrap();
    let started = |pid: Option<u32>| {
        serde_json::json!({"ts_ms": 1, "dir": "herdr",
            "event": {"type": "started", "cwd": "/x", "pid": pid}})
    };
    // w1: lost, never had a process; w2: lost, process alive; w3: exited.
    write_journal(&dir, "w1", &[started(None)]);
    write_journal(&dir, "w2", &[started(Some(alive.id()))]);
    write_journal(
        &dir,
        "w3",
        &[
            started(Some(alive.id())),
            serde_json::json!({"ts_ms": 2, "dir": "herdr", "event": {"type": "exited", "code": 0}}),
        ],
    );
    for id in ["w1", "w2", "w3"] {
        let leftover = dir.join("tmp").join(id);
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::write(leftover.join("draft.txt"), "x").unwrap();
    }

    let supervisor = WorkerSupervisor::open(dir.clone(), PathBuf::from("claude"));
    assert_eq!(supervisor.status("w1").unwrap().state, WorkerState::Lost);
    assert_eq!(supervisor.status("w2").unwrap().state, WorkerState::Lost);
    assert!(!dir.join("tmp/w1").exists());
    assert!(dir.join("tmp/w2/draft.txt").exists());
    assert!(!dir.join("tmp/w3").exists());

    // Once that process is gone, the next start removes its temp dir.
    alive.kill().unwrap();
    alive.wait().unwrap();
    drop(supervisor);
    WorkerSupervisor::open(dir.clone(), PathBuf::from("claude"));
    assert!(!dir.join("tmp/w2").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_worker_does_not_start_over_anything_at_its_temp_path() {
    let fixture = Fixture::new("temp-taken");
    let outside = fixture.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let tmp = fixture.root.join("workers/tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    std::os::unix::fs::symlink(&outside, tmp.join("w1")).unwrap();

    let error = fixture
        .supervisor
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap_err();
    assert_eq!(error.code(), "worker_io_error", "{error}");
    assert!(error.to_string().contains("worker temp dir"), "{error}");
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    assert!(std::fs::symlink_metadata(tmp.join("w1"))
        .unwrap()
        .file_type()
        .is_symlink());

    // A plain directory left there is refused the same way.
    std::fs::create_dir(tmp.join("w2")).unwrap();
    let error = fixture
        .supervisor
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap_err();
    assert_eq!(error.code(), "worker_io_error", "{error}");
    // The next number is free: it starts.
    let id = fixture.start("finish");
    assert_eq!(id, "w3");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
}

#[test]
fn a_temp_parent_that_is_a_symlink_is_refused() {
    let fixture = Fixture::new("temp-parent");
    let outside = fixture.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(fixture.root.join("workers")).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("workers/tmp")).unwrap();
    let error = fixture
        .supervisor
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap_err();
    assert!(
        error.to_string().contains("not a real directory"),
        "{error}"
    );
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}

#[test]
fn a_refused_turn_fails_even_with_result_success() {
    let fixture = Fixture::new("refusal");
    let id = fixture.start("refuse");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Failed);
    let result = worker.last_result.unwrap();
    assert_eq!(result.subtype, "success");
    assert_eq!(
        result.failure.as_deref(),
        Some("the model refused the turn (cyber)")
    );

    // A message during a refused turn does not clear the refusal.
    let id = fixture.start("refuse-wait");
    fixture.wait_for_status(&id, |status| status.refusal.is_some());
    let (_, live, _) = fixture.supervisor.live(&id).unwrap();
    live.send(&fixture.supervisor, &user_message("meanwhile"))
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Failed);
    assert_eq!(
        worker.last_result.unwrap().failure.as_deref(),
        Some("the model refused the turn (cyber)")
    );

    // The refusal belongs to that turn only.
    fixture.supervisor.prompt(&id, "finish").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert!(worker.last_result.unwrap().failure.is_none());

    // Replayed from the journal, the same turns fold the same way.
    let replayed = replay_journal(
        &id,
        &fixture.root.join("workers").join(format!("{id}.jsonl")),
    )
    .unwrap();
    assert_eq!(replayed.turns, 2);
}

#[test]
fn an_exit_code_after_a_finished_turn_records_a_failure() {
    let fixture = Fixture::new("exit1");
    let id = fixture.start("exit1");
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.exit_code, Some(1));
    // Visibly failed, not only exited; the process is gone all the same.
    assert_eq!(worker.state, WorkerState::Failed);
    assert_eq!(
        fixture.supervisor.prompt(&id, "finish").unwrap_err().code(),
        "worker_not_running"
    );
    let replayed = replay_journal(
        &id,
        &fixture.root.join("workers").join(format!("{id}.jsonl")),
    )
    .unwrap();
    assert_eq!(replayed.state, WorkerState::Failed);
    assert!(replayed.is_gone());
    let result = worker.last_result.unwrap();
    assert_eq!(result.subtype, "success");
    assert_eq!(
        result.failure.as_deref(),
        Some("the CLI exited with code 1 after the turn")
    );

    // Herdr's own stop is no failure of the turn.
    let stopped = fixture.start("finish");
    fixture.wait(&stopped, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.stop(&stopped).unwrap();
    let worker = fixture.wait(&stopped, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Exited);
    assert!(worker.last_result.unwrap().failure.is_none());
}

#[test]
fn permission_denials_are_kept_in_the_status() {
    let fixture = Fixture::new("denials");
    let id = fixture.start("denials");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    let denials = worker.last_result.unwrap().permission_denials;
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0]["tool_name"], "Write");
    assert_eq!(denials[0]["tool_input"]["file_path"], "/outside");
    let replayed = replay_journal(
        &id,
        &fixture.root.join("workers").join(format!("{id}.jsonl")),
    )
    .unwrap();
    assert_eq!(replayed.last_result.unwrap().permission_denials, denials);

    // A result without the field has none.
    fixture.supervisor.prompt(&id, "finish").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert!(worker.last_result.unwrap().permission_denials.is_empty());
}

#[test]
fn a_classifier_escalation_goes_to_the_user() {
    let fixture = Fixture::new("classifier");
    let id = fixture.start("classifier Write inside.txt");
    let worker = fixture.wait_for_question(&id);
    assert_eq!(worker.questions[0].tool_name, "Write");
    let decision = &fixture.herdr_events(&id, "permission")[0];
    assert_eq!(decision["decision"], "ask");
    assert_eq!(decision["decision_reason_type"], "classifier");
}

#[test]
fn a_prompt_during_a_turn_is_refused() {
    let fixture = Fixture::new("busy");
    let id = fixture.start("block");
    let error = fixture.supervisor.prompt(&id, "finish").unwrap_err();
    assert_eq!(error.code(), "worker_busy");
}

#[test]
fn an_interrupt_ends_the_turn_as_interrupted() {
    let fixture = Fixture::new("interrupt");
    let id = fixture.start("block");
    fixture
        .supervisor
        .interrupt(&WorkerInterruptParams {
            worker_id: id.clone(),
            turn: None,
            command_id: None,
        })
        .unwrap();

    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Interrupted);
    assert_eq!(
        worker.last_result.unwrap().terminal_reason.as_deref(),
        Some("aborted_tools")
    );
    assert!(fixture.journal(&id).iter().any(
        |record| record["dir"] == "in" && record["event"]["request"]["subtype"] == "interrupt"
    ));
}

#[test]
fn the_policy_answers_the_requests_it_decides() {
    let fixture = Fixture::new("permissions");
    let outside = fixture.root.join("outside.txt").display().to_string();
    let temp = fixture.root.join("workers/tmp");
    for (prompt, expected) in [
        ("perm Write inside.txt".to_owned(), "allow"),
        (format!("perm Write {outside}"), "deny"),
        // The next worker's temp dir: w3.
        (format!("perm Write {}/w3/msg.txt", temp.display()), "allow"),
        // Another worker's temp dir is outside this one's roots.
        (format!("perm Write {}/w1/msg.txt", temp.display()), "deny"),
        // Bash runs in the sandbox: herdr allows it and journals it.
        ("perm Bash cd . && awk NR<5 TODO.md".to_owned(), "allow"),
    ] {
        let id = fixture.start(&prompt);
        let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
        let text = worker.last_result.unwrap().text.unwrap();
        assert!(text.starts_with(expected), "{prompt}: {text}");
        let decisions = fixture.herdr_events(&id, "permission");
        assert_eq!(decisions.len(), 1, "{prompt}");
        assert_eq!(decisions[0]["decision"], expected, "{prompt}");
        if expected == "deny" {
            assert!(text.contains("herdr worker policy"), "{text}");
        }
        assert!(fixture.herdr_events(&id, "question").is_empty());
    }
}

#[test]
fn a_request_left_to_the_user_waits_for_the_answer() {
    let fixture = Fixture::new("approval");
    let id = fixture.start("classifier Bash git push origin master");

    let worker = fixture.wait_for_question(&id);
    assert_eq!(worker.state, WorkerState::WaitingApproval);
    let question = &worker.questions[0];
    assert_eq!(question.kind, WorkerQuestionKind::Approval);
    assert_eq!(question.tool_name, "Bash");
    assert_eq!(question.text, "git push origin master");
    assert_eq!(question.request_id, "perm-1");
    let pending = fixture.supervisor.pending_questions();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].worker_id, id);
    assert_eq!(
        fixture.herdr_events(&id, "permission")[0]["decision"],
        "ask"
    );
    assert!(fixture
        .journal(&id)
        .iter()
        .all(|record| record["dir"] != "in" || record["event"]["type"] != "control_response"));

    let wrong = fixture.answer(&id, None, &["blue.txt"]).unwrap_err();
    assert_eq!(wrong.code(), "invalid_request");
    let undecided = fixture.answer(&id, None, &[]).unwrap_err();
    assert_eq!(undecided.code(), "invalid_request");
    assert_eq!(fixture.supervisor.status(&id).unwrap().questions.len(), 1);

    let answered = fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap();
    assert!(answered.questions.is_empty());
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert!(fixture.supervisor.pending_questions().is_empty());
    let answers = fixture.herdr_events(&id, "answer_intent");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["decision"], "allow");
    assert_eq!(answers[0]["by"], "user");

    let none = fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap_err();
    assert_eq!(none.code(), "worker_no_question");
}

#[test]
fn a_question_detail_carries_the_whole_input_and_a_settled_one_is_gone() {
    let fixture = Fixture::new("detail");
    let id = fixture.start("classifier Bash git push origin master");
    fixture.wait_for_question(&id);

    let detail = fixture.supervisor.question_detail(&id, "perm-1").unwrap();
    assert_eq!(detail.worker_id, id);
    assert_eq!(detail.question.request_id, "perm-1");
    assert!(
        detail.input_text.starts_with("git push origin master"),
        "{}",
        detail.input_text
    );
    // A worker without an owner asks the user at once.
    assert!(!detail.quiet);
    assert_eq!(detail.owner_pane_id, None);
    let unknown = fixture
        .supervisor
        .question_detail(&id, "perm-9")
        .unwrap_err();
    assert_eq!(unknown.code(), "worker_no_question");

    fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap();
    let gone = fixture
        .supervisor
        .question_detail(&id, "perm-1")
        .unwrap_err();
    assert_eq!(gone.code(), "worker_question_gone");
    assert!(gone.to_string().contains("answered"), "{gone}");
}

#[test]
fn the_whole_input_keeps_a_commands_newlines_and_its_other_fields() {
    let input = serde_json::json!({
        "command": "git add -A\ngit commit -m 'x'",
        "dangerouslyDisableSandbox": true,
        "description": "Commit",
    });
    assert_eq!(
        full_input_text(&input),
        "git add -A\ngit commit -m 'x'\n\ndangerouslyDisableSandbox: true\ndescription: Commit"
    );
    let long = "x".repeat(1_000);
    let input = serde_json::json!({"file_path": "/tmp/a", "content": long});
    let text = full_input_text(&input);
    assert!(text.contains(&long), "never cut");
    assert!(text.contains("\"file_path\": \"/tmp/a\""), "{text}");
}

#[test]
fn deny_and_stop_denies_the_question_then_stops_the_worker() {
    let fixture = Fixture::new("deny-stop");
    let id = fixture.start("classifier Bash git push origin master");
    fixture.wait_for_question(&id);

    fixture
        .supervisor
        .deny_and_stop(&WorkerDenyAndStopParams {
            worker_id: id.clone(),
            request_id: "perm-1".into(),
            message: None,
        })
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Exited);
    let answers = fixture.herdr_events(&id, "answer_intent");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["decision"], "deny");
    assert_eq!(
        answers[0]["response"]["message"],
        "The user stopped this worker."
    );
}

#[test]
fn an_answer_must_name_one_of_several_questions() {
    let fixture = Fixture::new("pair");
    let id = fixture.start("pair WebFetch https://example.com");
    fixture.wait_for(&id, |worker| worker.questions.len() == 2);

    let unnamed = fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap_err();
    assert_eq!(unnamed.code(), "invalid_request");
    assert!(unnamed.to_string().contains("perm-1, perm-2"), "{unnamed}");
    assert_eq!(fixture.supervisor.status(&id).unwrap().questions.len(), 2);

    let worker = fixture
        .answer_request(&id, "perm-2", WorkerDecision::Deny)
        .unwrap();
    assert_eq!(worker.questions.len(), 1);
    let again = fixture
        .answer_request(&id, "perm-2", WorkerDecision::Allow)
        .unwrap_err();
    assert_eq!(again.code(), "worker_question_gone");
    assert!(again.to_string().contains("answered"), "{again}");

    fixture
        .answer_request(&id, "perm-1", WorkerDecision::Allow)
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        worker.last_result.unwrap().text.as_deref(),
        Some("perm-1=allow perm-2=deny")
    );
    let late = fixture
        .answer_request(&id, "perm-1", WorkerDecision::Deny)
        .unwrap_err();
    assert_eq!(late.code(), "worker_question_gone");
    assert!(late.to_string().contains("answered"), "{late}");
    let unknown = fixture
        .answer_request(&id, "perm-9", WorkerDecision::Deny)
        .unwrap_err();
    assert_eq!(unknown.code(), "worker_no_question");
}

#[test]
fn an_answer_to_a_cancelled_question_says_so() {
    let fixture = Fixture::new("cancel");
    let id = fixture.start("cancel WebFetch https://example.com");
    // perm-1 is cancelled before perm-2 is asked.
    let worker = fixture.wait_for(&id, |worker| {
        worker
            .questions
            .iter()
            .any(|question| question.request_id == "perm-2")
    });
    assert_eq!(worker.questions.len(), 1);

    let gone = fixture
        .answer_request(&id, "perm-1", WorkerDecision::Allow)
        .unwrap_err();
    assert_eq!(gone.code(), "worker_question_gone");
    assert!(gone.to_string().contains("cancelled"), "{gone}");
    assert_eq!(fixture.supervisor.status(&id).unwrap().questions.len(), 1);

    fixture
        .answer_request(&id, "perm-2", WorkerDecision::Allow)
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        worker.last_result.unwrap().text.as_deref(),
        Some("perm-2=allow")
    );
}

#[test]
fn a_denial_carries_the_users_message() {
    let fixture = Fixture::new("deny");
    let id = fixture.start("perm WebFetch https://example.com");
    let worker = fixture.wait_for_question(&id);
    assert_eq!(worker.questions[0].text, "https://example.com");
    fixture
        .supervisor
        .answer(&WorkerAnswerParams {
            worker_id: id.clone(),
            request_id: Some("perm-1".into()),
            decision: Some(WorkerDecision::Deny),
            answers: Vec::new(),
            message: Some("not today".into()),
            command_id: None,
        })
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        worker.last_result.unwrap().text.as_deref(),
        Some("deny: not today")
    );
}

#[test]
fn ask_user_question_answers_flow_back_as_updated_input() {
    let fixture = Fixture::new("ask");
    let id = fixture.start("ask");
    let worker = fixture.wait_for_question(&id);
    let question = &worker.questions[0];
    assert_eq!(question.kind, WorkerQuestionKind::Choice);
    assert_eq!(question.questions.len(), 2);
    assert_eq!(question.questions[0].options, vec!["alpha.txt", "blue.txt"]);
    assert!(question.questions[1].multi_select);
    assert!(question.text.contains("Which file?"), "{}", question.text);

    let short = fixture.answer(&id, None, &["2"]).unwrap_err();
    assert_eq!(short.code(), "invalid_request");
    let approval = fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap_err();
    assert_eq!(approval.code(), "invalid_request");

    fixture.answer(&id, None, &["2", "RED, 1"]).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let answers: Value =
        serde_json::from_str(worker.last_result.unwrap().text.as_deref().unwrap()).unwrap();
    assert_eq!(
        answers,
        serde_json::json!({"Which colors?": "red, green", "Which file?": "blue.txt"})
    );
    assert_eq!(
        fixture.herdr_events(&id, "answer_intent")[0]["answers"]["Which file?"],
        "blue.txt"
    );

    let second = fixture.start("ask");
    fixture.wait_for_question(&second);
    fixture
        .answer(&second, Some(WorkerDecision::Deny), &[])
        .unwrap();
    let worker = fixture.wait(&second, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        worker.last_result.unwrap().text.as_deref(),
        Some("deny: The user declined to answer.")
    );
}

#[test]
fn questions_end_with_the_worker() {
    let fixture = Fixture::new("question-exit");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    fixture.supervisor.stop(&id).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert!(worker.questions.is_empty());
    assert!(fixture.supervisor.pending_questions().is_empty());
    let gone = fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap_err();
    assert_eq!(gone.code(), "worker_not_running");
}

#[test]
fn a_crash_ends_as_exited_without_a_result() {
    let fixture = Fixture::new("crash");
    let id = fixture.start("crash");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Exited);
    assert_eq!(worker.exit_code, Some(3));
    assert!(worker.last_result.is_none());
    assert_eq!(fixture.herdr_events(&id, "exited").len(), 1);
    let error = fixture.supervisor.prompt(&id, "finish").unwrap_err();
    assert_eq!(error.code(), "worker_not_running");
}

#[test]
fn stop_reports_the_exit_from_the_exit_event() {
    let fixture = Fixture::new("stop");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let stopped = fixture.supervisor.stop(&id).unwrap();
    // The stop returns at once; the exit may or may not have been seen yet.
    assert!(stopped.stop_requested_ms.is_some() || stopped.state == WorkerState::Exited);
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Exited);
    assert_eq!(worker.stop_requested_ms, None);
    assert_eq!(fixture.herdr_events(&id, "signal")[0]["signal"], "SIGTERM");
    assert_eq!(fixture.herdr_events(&id, "exited").len(), 1);
    let again = fixture.supervisor.stop(&id).unwrap_err();
    assert_eq!(again.code(), "worker_not_running");
}

#[test]
fn a_worker_alive_after_stop_shows_it_until_it_exits() {
    let fixture = Fixture::new("stop-ignored");
    let id = fixture.start("ignore-term");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let first = fixture.supervisor.stop(&id).unwrap();
    let requested = first.stop_requested_ms.unwrap();

    // It ignores SIGTERM, so asking again finds it alive, and the second
    // stop sends nothing.
    let again = fixture.supervisor.stop(&id).unwrap();
    assert_eq!(again.state, WorkerState::Finished);
    assert_eq!(again.stop_requested_ms, Some(requested));
    assert_eq!(fixture.herdr_events(&id, "signal").len(), 1);

    fixture.supervisor.kill(&id, false).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.exit_signal, Some(libc::SIGKILL));
    assert_eq!(worker.stop_requested_ms, None);
}

#[test]
fn kill_ends_the_worker_and_its_recorded_tool_sessions() {
    let fixture = Fixture::new("kill");
    let fifo = fixture.root.join("tool.fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

    let id = fixture.start(&format!("orphan {}", fifo.display()));
    // Opening the read end returns once the tool process opened the write
    // end; reading returns EOF once every writer, the tool, is gone.
    let mut tool = std::fs::File::open(&fifo).unwrap();
    let mut started = Instant::now();
    let worker = loop {
        let worker = fixture.supervisor.status(&id).unwrap();
        if !worker.tool_sessions.is_empty() {
            break worker;
        }
        assert!(
            started.elapsed() < HANG_GUARD,
            "tool session never recorded"
        );
        let registry = lock(&fixture.supervisor.shared.registry);
        drop(
            fixture
                .supervisor
                .shared
                .changed
                .wait_timeout(registry, Duration::from_millis(100)),
        );
    };
    assert!(!worker.tool_sessions.contains(&worker.pid.unwrap()));

    fixture.supervisor.kill(&id, false).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.exit_signal, Some(libc::SIGKILL));
    started = Instant::now();
    let mut buffer = Vec::new();
    tool.read_to_end(&mut buffer).unwrap();
    assert!(started.elapsed() < HANG_GUARD);
    let killed = fixture.herdr_events(&id, "killed_tool_processes");
    assert!(!killed[0]["pids"].as_array().unwrap().is_empty());
    let recorded = fixture.herdr_events(&id, "tool_sessions");
    assert!(recorded[0]["sessions"][0]["leader_start"].is_u64());
}

fn write_journal(dir: &Path, worker_id: &str, lines: &[Value]) {
    std::fs::create_dir_all(dir).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(dir.join(format!("{worker_id}.jsonl")), text).unwrap();
}

/// A process leading a session of its own, as Claude Code's Bash tools do.
fn spawn_session_leader() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("sleep");
    command.arg("1000");
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}

/// Ends `child` with SIGTERM and returns the signal it died of: SIGTERM
/// unless something else (a SIGKILL from `kill`) ended it first.
fn terminate(mut child: std::process::Child) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    child.wait().unwrap().signal()
}

#[test]
fn kill_signals_only_tool_sessions_whose_leader_is_the_recorded_process() {
    use std::os::unix::process::ExitStatusExt;
    let fixture = Fixture::new("kill-identity");
    let dir = fixture.root.join("workers");
    let mut current = spawn_session_leader();
    let replaced = spawn_session_leader();
    let unrecorded = spawn_session_leader();
    let start =
        |child: &std::process::Child| crate::platform::process_start_token(child.id()).unwrap();
    write_journal(
        &dir,
        "w1",
        &[
            serde_json::json!({"dir": "herdr", "event": {
                "type": "started", "cwd": "/repo", "pid": 99999}}),
            // A journal from before herdr recorded the leader's start.
            serde_json::json!({"dir": "herdr", "event": {
                "type": "tool_sessions", "sessions": [unrecorded.id()]}}),
            // `replaced` stands for a later process that got the pid of a
            // recorded leader: it started after the recorded one.
            serde_json::json!({"dir": "herdr", "event": {
            "type": "tool_sessions", "sessions": [
                {"session": current.id(), "leader_start": start(&current)},
                {"session": replaced.id(), "leader_start": start(&replaced) - 1},
            ]}}),
        ],
    );
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    assert_eq!(supervisor.status("w1").unwrap().state, WorkerState::Lost);

    let Err(WorkerError::NeedsForce(message)) = supervisor.kill("w1", false) else {
        panic!("a lost worker was killed without force");
    };
    assert!(
        message.contains(&format!("pids {}", current.id())),
        "{message}"
    );
    assert!(
        message.contains(&format!("not verified, not killed: {}", unrecorded.id())),
        "{message}"
    );
    assert!(current.try_wait().unwrap().is_none());

    let (_, report) = supervisor.kill("w1", true).unwrap();
    assert_eq!(report.pids, vec![current.id()]);
    assert_eq!(report.unverified_sessions, vec![unrecorded.id()]);
    assert_eq!(report.stale_sessions, vec![replaced.id()]);
    assert_eq!(current.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert_eq!(terminate(replaced), Some(libc::SIGTERM));
    assert_eq!(terminate(unrecorded), Some(libc::SIGTERM));
}

#[test]
fn a_session_plan_keeps_members_that_started_with_or_after_the_leader() {
    let starts = BTreeMap::from([(10, 100), (11, 150), (12, 50), (20, 300)]);
    let sessions = BTreeMap::from([(10, Some(100)), (20, Some(200)), (30, None)]);
    let plan = plan_session_kill(
        &sessions,
        |pid| starts.get(&pid).copied(),
        |session| match session {
            // 13 has no start token any more: it is gone.
            10 => vec![10, 11, 12, 13],
            20 => vec![20, 21],
            _ => vec![30],
        },
    );
    assert_eq!(plan.pids, vec![10, 11]);
    assert_eq!(plan.stale_sessions, vec![20]);
    assert_eq!(plan.unverified_sessions, vec![30]);
}

#[test]
fn a_restart_marks_unfinished_workers_lost_and_keeps_exited_ones() {
    let fixture = Fixture::new("lost");
    let dir = fixture.root.join("workers");
    let started = serde_json::json!({"dir": "herdr", "event": {
        "type": "started", "cwd": "/repo", "pid": 99999}});
    let init = serde_json::json!({"dir": "out", "event": {
        "type": "system", "subtype": "init", "session_id": "s-7"}});
    write_journal(&dir, "w7", &[started.clone(), init]);
    write_journal(
        &dir,
        "w3",
        &[
            started,
            serde_json::json!({"dir": "herdr", "event": {"type": "exited", "code": 0}}),
        ],
    );

    let supervisor = WorkerSupervisor::open(dir.clone(), fixture.root.join("claude-stub"));
    let lost = supervisor.status("w7").unwrap();
    assert_eq!(lost.state, WorkerState::Lost);
    assert_eq!(lost.session_id.as_deref(), Some("s-7"));
    assert_eq!(supervisor.status("w3").unwrap().state, WorkerState::Exited);
    let last: Value = serde_json::from_str(
        std::fs::read_to_string(dir.join("w7.jsonl"))
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["event"]["type"], "lost");
    assert_eq!(
        supervisor
            .wait("w7", WorkerWaitUntil::Exit, Duration::ZERO, || false)
            .unwrap()
            .unwrap()
            .state,
        WorkerState::Lost
    );

    let next = supervisor
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap();
    assert_eq!(next.worker_id, "w8");
    supervisor.kill("w8", false).unwrap();
}

#[test]
fn a_worker_is_named_by_its_task_and_keeps_its_space() {
    let fixture = Fixture::new("name");
    let named = fixture
        .supervisor
        .start(&WorkerStartParams {
            name: Some("fix login".into()),
            workspace_id: Some("ws_1".into()),
            ..start_params(&fixture.repo, "finish", None)
        })
        .unwrap();
    assert_eq!(named.name, "fix login");
    assert_eq!(named.workspace_id.as_deref(), Some("ws_1"));
    let unnamed = fixture
        .supervisor
        .start(&start_params(&fixture.repo, "\n  finish  \nmore", None))
        .unwrap();
    assert_eq!(unnamed.name, "finish");
    let summaries = fixture.supervisor.summaries();
    assert_eq!(summaries[0].name, "fix login");
    assert_eq!(summaries[0].workspace_id.as_deref(), Some("ws_1"));

    // The journal keeps them across a restart.
    fixture.wait(&named.worker_id, WorkerWaitUntil::TurnEnd);
    let reopened = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let replayed = reopened.status(&named.worker_id).unwrap();
    assert_eq!(replayed.name, "fix login");
    assert_eq!(replayed.workspace_id.as_deref(), Some("ws_1"));
}

#[test]
fn a_takeover_interrupts_stops_and_waits_for_the_exit() {
    let fixture = Fixture::new("takeover");
    let id = fixture.start("block");
    fixture.wait_for(&id, |worker| worker.session_id.is_some());
    let takeover = fixture.supervisor.begin_takeover(&id, false).unwrap();
    assert_eq!(takeover.session_id, "stub-session");
    assert!(matches!(
        fixture.supervisor.begin_takeover(&id, false),
        Err(WorkerError::Busy(_))
    ));
    let worker = fixture.supervisor.end_for_takeover(&id).unwrap();
    assert_eq!(worker.state, WorkerState::Exited);
    assert!(worker.takeover_ms.is_some());
    assert_eq!(
        worker.last_result.and_then(|result| result.terminal_reason),
        Some("aborted_tools".into())
    );
    // In this order: claimed, interrupted, its turn's result, then stopped.
    let journal = fixture.journal(&id);
    let position = |found: &dyn Fn(&Value) -> bool| {
        journal
            .iter()
            .position(found)
            .unwrap_or_else(|| panic!("missing in {journal:?}"))
    };
    let claimed = position(&|record| record["event"]["type"] == "takeover");
    let interrupted = position(&|record| {
        record["dir"] == "in" && record["event"]["request"]["subtype"] == "interrupt"
    });
    let result = position(&|record| record["event"]["type"] == "result");
    let stopped = position(&|record| record["event"]["signal"] == "SIGTERM");
    assert!(claimed < interrupted && interrupted < result && result < stopped);
}

#[test]
fn a_worker_that_asks_is_not_taken_over() {
    let fixture = Fixture::new("takeover-asks");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    assert!(matches!(
        fixture.supervisor.begin_takeover(&id, false),
        Err(WorkerError::Busy(_))
    ));
    assert_eq!(fixture.supervisor.status(&id).unwrap().takeover_ms, None);
}

#[test]
fn an_exited_worker_is_taken_over_once() {
    let fixture = Fixture::new("takeover-exited");
    let id = fixture.start("crash");
    fixture.wait(&id, WorkerWaitUntil::Exit);
    fixture.supervisor.begin_takeover(&id, false).unwrap();
    let worker = fixture.supervisor.end_for_takeover(&id).unwrap();
    assert!(worker.takeover_ms.is_some());
    let running = fixture.supervisor.begin_takeover(&id, false).unwrap_err();
    assert_eq!(running.code(), "worker_busy");
    assert!(running.to_string().contains("being taken over"));

    fixture
        .supervisor
        .takeover_tab_opened(&id, "tab_9")
        .unwrap();
    let done = fixture.supervisor.begin_takeover(&id, false).unwrap_err();
    assert_eq!(done.code(), "worker_busy");
    assert!(done.to_string().contains("tab_9"), "{done}");
    assert_eq!(fixture.herdr_events(&id, "takeover").len(), 1);
    assert_eq!(
        fixture
            .supervisor
            .status(&id)
            .unwrap()
            .takeover_tab_id
            .as_deref(),
        Some("tab_9")
    );
}

#[test]
fn a_failed_takeover_releases_its_claim() {
    let fixture = Fixture::new("takeover-failed");
    let id = fixture.start("crash");
    fixture.wait(&id, WorkerWaitUntil::Exit);
    fixture.supervisor.begin_takeover(&id, false).unwrap();
    fixture
        .supervisor
        .fail_takeover(&id, "the tab was not created")
        .unwrap();
    let worker = fixture.supervisor.status(&id).unwrap();
    assert_eq!(worker.takeover_ms, None);
    assert_eq!(
        worker.takeover_error.as_deref(),
        Some("the tab was not created")
    );
    assert!(!fixture.supervisor.summaries()[0].takeover);
    assert_eq!(
        fixture.herdr_events(&id, "takeover_failed")[0]["error"],
        "the tab was not created"
    );

    // The retry is claimed again and clears the old error.
    fixture.supervisor.begin_takeover(&id, false).unwrap();
    let worker = fixture.supervisor.status(&id).unwrap();
    assert!(worker.takeover_ms.is_some());
    assert_eq!(worker.takeover_error, None);
    // Without a claim there is nothing to release.
    fixture.supervisor.fail_takeover(&id, "again").unwrap();
    let unclaimed = fixture.supervisor.fail_takeover(&id, "again").unwrap_err();
    assert_eq!(unclaimed.code(), "invalid_request");
}

#[test]
fn prompts_and_answers_are_refused_during_a_takeover() {
    let fixture = Fixture::new("takeover-refuses");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.begin_takeover(&id, false).unwrap();
    let prompt = fixture.supervisor.prompt(&id, "finish").unwrap_err();
    assert_eq!(prompt.code(), "worker_busy");
    fixture.supervisor.kill(&id, false).unwrap();

    // A question that arrived after the claim (the claim itself is refused
    // while one waits): claimed here directly.
    let asking = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&asking);
    {
        let mut registry = lock(&fixture.supervisor.shared.registry);
        let number = worker_number(&asking).unwrap();
        registry.workers.get_mut(&number).unwrap().status.apply(
            Direction::Herdr,
            &serde_json::json!({"type": "takeover", "at_ms": 1}),
        );
    }
    let answer = fixture
        .answer(&asking, Some(WorkerDecision::Allow), &[])
        .unwrap_err();
    assert_eq!(answer.code(), "worker_busy");
    assert_eq!(
        fixture.supervisor.status(&asking).unwrap().questions.len(),
        1
    );
    fixture.supervisor.kill(&asking, false).unwrap();
}

#[test]
fn a_restart_replays_takeover_steps() {
    let fixture = Fixture::new("takeover-replay");
    let dir = fixture.root.join("workers");
    let ended = [
        serde_json::json!({"dir": "herdr", "event": {"type": "started", "cwd": "/repo"}}),
        serde_json::json!({"dir": "out", "event": {
            "type": "system", "subtype": "init", "session_id": "s-1"}}),
        serde_json::json!({"dir": "herdr", "event": {"type": "exited", "code": 0}}),
        serde_json::json!({"dir": "herdr", "event": {"type": "takeover", "at_ms": 5}}),
    ];
    let failed = serde_json::json!({"dir": "herdr", "event": {
        "type": "takeover_failed", "error": "boom", "at_ms": 6}});
    let opened = serde_json::json!({"dir": "herdr", "event": {
        "type": "takeover_tab_opened", "tab_id": "tab_4", "at_ms": 6}});
    write_journal(&dir, "w1", &[&ended[..], &[failed]].concat());
    write_journal(&dir, "w2", &[&ended[..], &[opened]].concat());
    write_journal(&dir, "w3", &ended);

    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let released = supervisor.status("w1").unwrap();
    assert_eq!(released.takeover_ms, None);
    assert_eq!(released.takeover_error.as_deref(), Some("boom"));
    assert!(!released.takeover_unfinished);

    let done = supervisor.status("w2").unwrap();
    assert_eq!(done.takeover_ms, Some(5));
    assert_eq!(done.takeover_tab_id.as_deref(), Some("tab_4"));
    assert!(!done.takeover_unfinished);
    assert_eq!(
        supervisor.begin_takeover("w2", false).unwrap_err().code(),
        "worker_busy"
    );

    // Claimed, but the tab was never recorded: reported, and retried only
    // with force (see `an_unfinished_takeover_is_retried_once_only_with_force`).
    let unfinished = supervisor.status("w3").unwrap();
    assert_eq!(unfinished.takeover_ms, Some(5));
    assert!(unfinished.takeover_unfinished);
    let summary = supervisor
        .summaries()
        .into_iter()
        .find(|summary| summary.worker_id == "w3")
        .unwrap();
    assert!(!summary.takeover);
    assert_eq!(
        supervisor.unfinished_takeovers(),
        vec![UnfinishedTakeover {
            worker_id: "w3".into(),
            takeover_id: None,
            session_id: Some("s-1".into()),
        }]
    );

    supervisor.begin_takeover("w1", false).unwrap();
}

/// A journal whose takeover `takeover-w1-1` was claimed by a server that
/// ended before recording its tab.
fn unfinished_takeover_journal(fixture: &Fixture) -> PathBuf {
    let dir = fixture.root.join("workers");
    write_journal(
        &dir,
        "w1",
        &[
            serde_json::json!({"dir": "herdr", "event": {"type": "started", "cwd": "/repo"}}),
            serde_json::json!({"dir": "out", "event": {
                "type": "system", "subtype": "init", "session_id": "s-1"}}),
            serde_json::json!({"dir": "herdr", "event": {"type": "exited", "code": 0}}),
            serde_json::json!({"dir": "herdr", "event": {
                "type": "takeover", "at_ms": 5, "takeover_id": "takeover-w1-1"}}),
        ],
    );
    dir
}

/// Processes as a test lays them out: `(pid, parent, takeover id in its
/// environment, resumed session)`.
struct FakeProcesses(Vec<(u32, u32, Option<&'static str>, Option<&'static str>)>);

impl ProcessProbe for FakeProcesses {
    fn env_value(&self, pid: u32, key: &str) -> Option<String> {
        assert_eq!(key, TAKEOVER_ID_ENV);
        self.0
            .iter()
            .find(|process| process.0 == pid)
            .and_then(|process| process.2.map(str::to_owned))
    }

    fn resuming(&self, session_id: &str) -> Vec<u32> {
        self.0
            .iter()
            .filter(|process| process.3 == Some(session_id))
            .map(|process| process.0)
            .collect()
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.0
            .iter()
            .find(|process| process.0 == pid)
            .map(|process| process.1)
    }
}

fn tab(tab_id: &str, title: &str, shell_pids: &[u32]) -> TakeoverTabCandidate {
    TakeoverTabCandidate {
        tab_id: tab_id.into(),
        title: title.into(),
        shell_pids: shell_pids.to_vec(),
    }
}

#[test]
fn an_unfinished_takeover_adopts_the_tab_carrying_its_id() {
    let fixture = Fixture::new("takeover-adopt-env");
    let dir = unfinished_takeover_journal(&fixture);
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let [unfinished] = supervisor.unfinished_takeovers().try_into().unwrap();
    assert_eq!(unfinished.takeover_id.as_deref(), Some("takeover-w1-1"));
    // The renamed tab is found by its shell's environment, not its title.
    let tabs = [tab("tab_1", "shell", &[10]), tab("tab_2", "renamed", &[20])];
    let processes = FakeProcesses(vec![
        (10, 1, Some("takeover-w1-0"), None),
        (20, 1, Some("takeover-w1-1"), None),
    ]);
    assert_eq!(
        find_takeover_tab(&unfinished, &tabs, &processes),
        TakeoverFound::Tab("tab_2".into())
    );
    // And by its title alone.
    let titled = [tab("tab_3", "fix login takeover-w1-1", &[])];
    assert_eq!(
        find_takeover_tab(&unfinished, &titled, &FakeProcesses(Vec::new())),
        TakeoverFound::Tab("tab_3".into())
    );

    supervisor.adopt_takeover_tab(&unfinished, "tab_2").unwrap();
    let worker = supervisor.status("w1").unwrap();
    assert!(!worker.takeover_unfinished);
    assert_eq!(worker.takeover_tab_id.as_deref(), Some("tab_2"));
    let journal = std::fs::read_to_string(fixture.root.join("workers/w1.jsonl")).unwrap();
    assert!(
        journal.contains(r#""type":"takeover_tab_opened""#),
        "{journal}"
    );
    // Adopted once: the claim is no longer unfinished, and a takeover is
    // refused with the adopted tab.
    assert_eq!(
        supervisor
            .adopt_takeover_tab(&unfinished, "tab_9")
            .unwrap_err()
            .code(),
        "invalid_request"
    );
    let refused = supervisor.begin_takeover("w1", true).unwrap_err();
    assert!(refused.to_string().contains("tab_2"), "{refused}");
}

#[test]
fn an_unfinished_takeover_adopts_the_tab_whose_process_resumes_its_session() {
    let fixture = Fixture::new("takeover-adopt-process");
    let dir = unfinished_takeover_journal(&fixture);
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let [unfinished] = supervisor.unfinished_takeovers().try_into().unwrap();
    // `claude --resume s-1` (31) runs under the shell (30) of tab_2, whose
    // environment does not carry the id.
    let tabs = [tab("tab_1", "shell", &[10]), tab("tab_2", "shell", &[30])];
    let processes = FakeProcesses(vec![
        (10, 1, None, None),
        (30, 1, None, None),
        (31, 30, None, Some("s-1")),
        (40, 1, None, Some("s-2")),
    ]);
    let found = find_takeover_tab(&unfinished, &tabs, &processes);
    assert_eq!(found, TakeoverFound::Tab("tab_2".into()));
    supervisor.adopt_takeover_tab(&unfinished, "tab_2").unwrap();
    assert_eq!(
        supervisor.status("w1").unwrap().takeover_tab_id.as_deref(),
        Some("tab_2")
    );

    // A process resuming it under no tab is reported, not adopted.
    let outside = FakeProcesses(vec![(50, 1, None, Some("s-1"))]);
    assert_eq!(
        find_takeover_tab(&unfinished, &tabs, &outside),
        TakeoverFound::Process(50)
    );
    assert_eq!(
        find_takeover_tab(&unfinished, &tabs, &FakeProcesses(Vec::new())),
        TakeoverFound::Nothing
    );
}

#[test]
fn an_unfinished_takeover_is_retried_once_only_with_force() {
    let fixture = Fixture::new("takeover-force");
    let dir = unfinished_takeover_journal(&fixture);
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let refused = supervisor.begin_takeover("w1", false).unwrap_err();
    assert_eq!(refused.code(), "worker_busy");
    assert!(refused.to_string().contains("--force"), "{refused}");
    assert!(refused.to_string().contains("two writers"), "{refused}");
    assert!(supervisor.status("w1").unwrap().takeover_unfinished);

    let retry = supervisor.begin_takeover("w1", true).unwrap();
    assert_ne!(retry.takeover_id, "takeover-w1-1");
    let worker = supervisor.status("w1").unwrap();
    assert!(!worker.takeover_unfinished);
    assert_eq!(
        worker.takeover_id.as_deref(),
        Some(retry.takeover_id.as_str())
    );
    // The new claim serializes retries: a second one, forced or not, is
    // refused while it holds.
    for force in [true, false] {
        let again = supervisor.begin_takeover("w1", force).unwrap_err();
        assert!(again.to_string().contains("being taken over"), "{again}");
    }
    let journal = std::fs::read_to_string(fixture.root.join("workers/w1.jsonl")).unwrap();
    assert_eq!(journal.matches(r#""type":"takeover""#).count(), 2);
    assert!(journal.contains(r#""forced":true"#), "{journal}");
}

#[test]
fn a_later_exit_replaces_lost_and_a_finished_worker_keeps_its_state() {
    let fixture = Fixture::new("lost-replay");
    let dir = fixture.root.join("workers");
    let started = serde_json::json!({"dir": "herdr", "event": {
        "type": "started", "cwd": "/repo", "pid": 99999}});
    let init = serde_json::json!({"dir": "out", "event": {
        "type": "system", "subtype": "init", "session_id": "s-1"}});
    let lost = serde_json::json!({"dir": "herdr", "event": {
        "type": "lost", "reason": "server restarted"}});
    let finished = serde_json::json!({"dir": "out", "event": {
        "type": "result", "subtype": "success", "is_error": false,
        "terminal_reason": "completed", "result": "done"}});
    write_journal(
        &dir,
        "w1",
        &[
            started.clone(),
            init.clone(),
            lost.clone(),
            serde_json::json!({"dir": "herdr", "event": {"type": "exited", "code": 3}}),
        ],
    );
    write_journal(&dir, "w2", &[started, init, finished, lost]);

    let supervisor = WorkerSupervisor::open(dir.clone(), fixture.root.join("claude-stub"));
    let exited = supervisor.status("w1").unwrap();
    assert_eq!(exited.state, WorkerState::Exited);
    assert_eq!(exited.exit_code, Some(3));
    assert_eq!(exited.end_note, None);

    let ended = supervisor.status("w2").unwrap();
    assert_eq!(ended.state, WorkerState::Finished);
    assert_eq!(ended.end_note.as_deref(), Some("ended by a server restart"));
    assert_eq!(ended.last_result.unwrap().text.as_deref(), Some("done"));
    // It is gone: an exit wait returns, and no second `lost` is appended.
    supervisor
        .wait("w2", WorkerWaitUntil::Exit, Duration::ZERO, || false)
        .unwrap()
        .unwrap();
    let journal = std::fs::read_to_string(dir.join("w2.jsonl")).unwrap();
    assert_eq!(journal.matches("\"lost\"").count(), 1);
    // Its process is gone, so its session can be taken over.
    supervisor.begin_takeover("w2", false).unwrap();
}

#[test]
fn a_restart_leaves_a_journal_another_server_owns_until_it_lets_go() {
    let fixture = Fixture::new("lost-owned");
    let dir = fixture.root.join("workers");
    let started = serde_json::json!({"dir": "herdr", "event": {
        "type": "started", "cwd": "/repo", "pid": 99999}});
    let init = serde_json::json!({"dir": "out", "event": {
        "type": "system", "subtype": "init", "session_id": "s-1"}});
    write_journal(&dir, "w1", &[started.clone(), init.clone()]);
    write_journal(&dir, "w2", &[started, init]);
    let hold = |worker_id: &str| {
        let lock = File::create(dir.join(format!("{worker_id}.lock"))).unwrap();
        lock.try_lock().unwrap();
        lock
    };
    let (w1_lock, w2_lock) = (hold("w1"), hold("w2"));

    let supervisor = WorkerSupervisor::open(dir.clone(), fixture.root.join("claude-stub"));
    for worker_id in ["w1", "w2"] {
        assert_eq!(
            supervisor.status(worker_id).unwrap().state,
            WorkerState::Working
        );
        let journal = std::fs::read_to_string(dir.join(format!("{worker_id}.jsonl"))).unwrap();
        assert!(!journal.contains("\"lost\""), "{journal}");
    }

    // The owner journals the exit, then lets go: the exit is adopted.
    Journal::open(&dir.join("w1.jsonl"))
        .unwrap()
        .export(
            None,
            now_ms(),
            Direction::Herdr,
            store::Recorded::Event(&serde_json::json!({"type": "exited", "code": 0})),
        )
        .unwrap();
    drop(w1_lock);
    // The owner ends without journaling an exit: now it is lost.
    drop(w2_lock);
    let started = Instant::now();
    for (worker_id, state) in [("w1", WorkerState::Exited), ("w2", WorkerState::Lost)] {
        let number = worker_number(worker_id).unwrap();
        let mut registry = lock(&supervisor.shared.registry);
        while registry.workers[&number].status.state != state {
            assert!(started.elapsed() < HANG_GUARD, "{worker_id} not adopted");
            registry = supervisor
                .shared
                .changed
                .wait_timeout(registry, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }
    let w2 = std::fs::read_to_string(dir.join("w2.jsonl")).unwrap();
    assert_eq!(w2.matches("\"lost\"").count(), 1);
    let w1 = std::fs::read_to_string(dir.join("w1.jsonl")).unwrap();
    assert!(!w1.contains("\"lost\""));
}

#[test]
fn a_running_worker_is_not_marked_lost_by_another_server() {
    let fixture = Fixture::new("lost-live");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let other = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(other.status(&id).unwrap().state, WorkerState::Finished);
    assert!(fixture.herdr_events(&id, "lost").is_empty());
}

#[test]
fn a_handoff_is_refused_while_any_worker_process_is_alive() {
    let fixture = Fixture::new("handoff-busy");
    let busy = fixture.start("block");
    fixture.wait_for(&busy, |worker| worker.state == WorkerState::Working);
    let idle = fixture.start("finish");
    fixture.wait(&idle, WorkerWaitUntil::TurnEnd);

    let refused = fixture.supervisor.prepare_for_handoff(false).unwrap_err();
    assert!(refused.contains(&format!("{busy} (working)")), "{refused}");
    assert!(
        refused.contains(&format!("{idle} (idle between turns)")),
        "{refused}"
    );
    assert!(refused.contains("herdr worker stop"), "{refused}");
    assert!(refused.contains("--force"), "{refused}");
    // Nothing was stopped.
    for worker_id in [&busy, &idle] {
        assert!(fixture.herdr_events(worker_id, "signal").is_empty());
    }

    // Forced, both get SIGTERM; the server does not wait for them.
    let signalled = fixture.supervisor.prepare_for_handoff(true).unwrap();
    assert_eq!(signalled, vec![busy.clone(), idle.clone()]);
    for worker_id in [&busy, &idle] {
        assert_eq!(
            fixture.herdr_events(worker_id, "signal")[0]["signal"],
            "SIGTERM"
        );
    }
}

/// `worker.wait_drained` with the liveness re-check as long as the hang
/// guard, so a missed wake fails the test. `on_block` runs at the first
/// point the wait would block, in the window between its check and its
/// block. Returns the reply and how many times the wait got that far.
fn wait_drained(
    supervisor: &WorkerSupervisor,
    seen: &[String],
    draining: Option<bool>,
    mut on_block: impl FnMut(),
) -> (WorkerDrain, usize) {
    let started = Instant::now();
    let mut blocked = 0;
    let params = WorkerWaitDrainedParams {
        in_turn: seen.to_vec(),
        draining,
    };
    let drain = supervisor
        .wait_drained(&params, HANG_GUARD, || {
            assert!(started.elapsed() < HANG_GUARD, "the drain hung");
            blocked += 1;
            if blocked == 1 {
                on_block();
            }
            true
        })
        .unwrap();
    (drain, blocked)
}

fn ids(workers: &[WorkerInfo]) -> Vec<String> {
    workers
        .iter()
        .map(|worker| worker.worker_id.clone())
        .collect()
}

#[test]
fn a_drain_waits_for_the_running_turn_to_end() {
    let fixture = Fixture::new("drain-busy");
    let busy = fixture.start("block");
    fixture.wait_for(&busy, |worker| worker.state == WorkerState::Working);
    let idle = fixture.start("finish");
    fixture.wait(&idle, WorkerWaitUntil::TurnEnd);

    let drain = fixture
        .supervisor
        .drain(WorkerDrainAction::Start, Some("an install (test)"));
    assert!(drain.draining);
    assert_eq!(drain.reason.as_deref(), Some("an install (test)"));
    assert_eq!(ids(&drain.in_turn), vec![busy.clone()]);

    // Knowing nothing yet, the wait answers at once with the turn it waits for.
    let (first, blocked) = wait_drained(&fixture.supervisor, &[], None, || {});
    assert_eq!((ids(&first.in_turn), blocked), (vec![busy.clone()], 0));

    // Knowing it, the wait blocks until that turn's end event, which the
    // interrupt brings only once the wait is about to block.
    let (drained, blocked) = wait_drained(
        &fixture.supervisor,
        std::slice::from_ref(&busy),
        Some(true),
        || {
            fixture
                .supervisor
                .interrupt(&WorkerInterruptParams {
                    worker_id: busy.clone(),
                    turn: None,
                    command_id: None,
                })
                .unwrap();
        },
    );
    assert!(blocked >= 1);
    assert!(drained.in_turn.is_empty(), "{drained:?}");
    assert_eq!(ids(&drained.ended), vec![busy.clone()]);
    assert_eq!(drained.ended[0].state, WorkerState::Interrupted);
    assert!(
        drained.draining,
        "the drain lasts until the handoff or a cancel"
    );

    // A cancel ends a wait that saw the drain, though a turn still runs.
    fixture.supervisor.prompt(&idle, "block").unwrap_err();
    fixture.supervisor.drain(WorkerDrainAction::Cancel, None);
    fixture.supervisor.prompt(&idle, "block").unwrap();
    fixture.supervisor.drain(WorkerDrainAction::Start, None);
    let (cancelled, _) = wait_drained(
        &fixture.supervisor,
        std::slice::from_ref(&idle),
        Some(true),
        || {
            fixture.supervisor.drain(WorkerDrainAction::Cancel, None);
        },
    );
    assert!(!cancelled.draining);
    assert_eq!(ids(&cancelled.in_turn), vec![idle.clone()]);
}

#[test]
fn a_drain_without_turns_is_drained_at_once_and_a_new_server_does_not_drain() {
    let fixture = Fixture::new("drain-idle");
    let idle = fixture.start("finish");
    fixture.wait(&idle, WorkerWaitUntil::TurnEnd);

    fixture.supervisor.drain(WorkerDrainAction::Start, None);
    let (drain, blocked) = wait_drained(&fixture.supervisor, &[], None, || {});
    assert_eq!(blocked, 0);
    assert!(drain.in_turn.is_empty() && drain.ended.is_empty());
    assert_eq!(drain.reason.as_deref(), Some("an install"));

    // The handoff's new server starts without the drain.
    fixture.supervisor.stop(&idle).unwrap();
    fixture.wait(&idle, WorkerWaitUntil::Exit);
    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert!(!next.drain(WorkerDrainAction::Status, None).draining);
}

#[test]
fn prompts_and_starts_are_refused_during_a_drain_until_it_is_cancelled() {
    let fixture = Fixture::new("drain-refuse");
    let idle = fixture.start("finish");
    fixture.wait(&idle, WorkerWaitUntil::TurnEnd);
    fixture
        .supervisor
        .drain(WorkerDrainAction::Start, Some("install pid 7"));
    // A second start keeps the first drain.
    let again = fixture
        .supervisor
        .drain(WorkerDrainAction::Start, Some("other"));
    assert_eq!(again.reason.as_deref(), Some("install pid 7"));

    let refused = fixture.supervisor.prompt(&idle, "finish").unwrap_err();
    assert_eq!(refused.code(), "workers_draining");
    assert!(refused.to_string().contains("install pid 7"), "{refused}");
    assert!(
        refused.to_string().contains("herdr worker drain cancel"),
        "{refused}"
    );
    let with_id = WorkerStartParams {
        command_id: Some("item:branch".into()),
        ..start_params(&fixture.repo, "finish", Some("stub-model"))
    };
    let refused = fixture.supervisor.start(&with_id).unwrap_err();
    assert_eq!(refused.code(), "workers_draining");
    assert_eq!(fixture.supervisor.list().len(), 1, "no worker started");
    assert_eq!(
        fixture
            .supervisor
            .drain(WorkerDrainAction::Status, None)
            .starting,
        0
    );

    let cancelled = fixture.supervisor.drain(WorkerDrainAction::Cancel, None);
    assert!(!cancelled.draining && cancelled.reason.is_none());
    fixture.supervisor.prompt(&idle, "finish").unwrap();
    fixture.wait(&idle, WorkerWaitUntil::TurnEnd);
    // The refusal was not stored with the command id: the same start runs now.
    let started = fixture.supervisor.start(&with_id).unwrap();
    fixture.wait(&started.worker_id, WorkerWaitUntil::TurnEnd);
}

#[test]
fn a_handoff_goes_ahead_once_stopped_workers_have_exited() {
    let fixture = Fixture::new("handoff-idle");
    let idle = fixture.start("finish");
    fixture.wait(&idle, WorkerWaitUntil::TurnEnd);
    assert!(fixture.supervisor.prepare_for_handoff(false).is_err());

    // What the install script does: stop, then wait for the exit event.
    fixture.supervisor.stop(&idle).unwrap();
    let refused = fixture.supervisor.prepare_for_handoff(false);
    if let Err(refused) = &refused {
        assert!(refused.contains(&format!("{idle} (stopping)")), "{refused}");
    }
    fixture.wait(&idle, WorkerWaitUntil::Exit);
    assert_eq!(
        fixture.supervisor.prepare_for_handoff(false).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(fixture.herdr_events(&idle, "exited").len(), 1);
    // A next server finds the exit, not a lost worker.
    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(next.status(&idle).unwrap().state, WorkerState::Exited);
    assert!(fixture.herdr_events(&idle, "lost").is_empty());
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_folder_slot_is_reused_clean_by_one_worker_at_a_time() {
    let fixture = Fixture::new("slot");
    let repo = &fixture.repo;
    git_in(repo, &["init", "-q", "-b", "master"]);
    std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git_in(repo, &["add", "."]);
    git_in(repo, &["commit", "-q", "-m", "init"]);
    let in_slot = |branch: &str| WorkerStartParams {
        folder_slot: Some("worker".into()),
        branch: Some(branch.into()),
        ..start_params(repo, "finish", None)
    };

    let first = fixture.supervisor.start(&in_slot("w/one")).unwrap();
    let slot = fixture
        .root
        .join("herdr-worktrees/worker")
        .canonicalize()
        .unwrap();
    assert_eq!(first.cwd, slot.display().to_string());
    assert_eq!(git_in(&slot, &["branch", "--show-current"]).trim(), "w/one");
    let target = slot.join("target");
    let zig_cache = slot.join("target/zig-cache");
    assert!(zig_cache.is_dir());
    let packages = zig_cache.join("p");
    if packages.exists() {
        assert!(std::fs::symlink_metadata(&packages)
            .unwrap()
            .file_type()
            .is_symlink());
    }
    let started = &fixture.herdr_events(&first.worker_id, "started")[0];
    assert_eq!(started["folder_slot"]["branch"], "w/one");
    assert_eq!(started["folder_slot"]["created"], true);
    let args: Vec<&str> = started["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let settings_at = args.iter().position(|arg| *arg == "--settings").unwrap();
    let settings: Value = serde_json::from_str(args[settings_at + 1]).unwrap();
    let allow_write = &settings["sandbox"]["filesystem"]["allowWrite"];
    for dir in [&target, &zig_cache] {
        assert!(
            allow_write
                .as_array()
                .unwrap()
                .contains(&Value::String(dir.display().to_string())),
            "{allow_write}"
        );
    }
    fixture.wait(&first.worker_id, WorkerWaitUntil::TurnEnd);
    let init = fixture
        .journal(&first.worker_id)
        .into_iter()
        .find(|record| record["event"]["subtype"] == "init")
        .unwrap();
    let cache_env = &init["event"]["cache_env"];
    assert_eq!(cache_env["CARGO_TARGET_DIR"], target.display().to_string());
    assert_eq!(
        cache_env["ZIG_GLOBAL_CACHE_DIR"],
        zig_cache.display().to_string()
    );
    assert_eq!(
        cache_env["ZIG_LOCAL_CACHE_DIR"],
        zig_cache.display().to_string()
    );

    // A worker between turns still holds the slot.
    let busy = fixture.supervisor.start(&in_slot("w/two")).unwrap_err();
    assert_eq!(busy.code(), "worker_busy", "{busy}");
    fixture.supervisor.stop(&first.worker_id).unwrap();
    fixture.wait(&first.worker_id, WorkerWaitUntil::Exit);

    // What a worker left uncommitted is not thrown away.
    std::fs::write(slot.join("left.txt"), "x").unwrap();
    let dirty = fixture.supervisor.start(&in_slot("w/two")).unwrap_err();
    assert_eq!(dirty.code(), "worker_busy", "{dirty}");
    assert!(dirty.to_string().contains("left.txt"), "{dirty}");
    std::fs::remove_file(slot.join("left.txt")).unwrap();
    let taken = fixture.supervisor.start(&in_slot("w/one")).unwrap_err();
    assert_eq!(taken.code(), "invalid_request", "{taken}");
    assert!(fixture
        .supervisor
        .start(&WorkerStartParams {
            branch: None,
            ..in_slot("unused")
        })
        .is_err());

    // Ignored build output stays warm for the next worker.
    let marker = target.join("debug/marker");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "warm").unwrap();
    let second = fixture.supervisor.start(&in_slot("w/two")).unwrap();
    assert_eq!(git_in(&slot, &["branch", "--show-current"]).trim(), "w/two");
    assert!(marker.exists());
    assert_eq!(
        fixture.herdr_events(&second.worker_id, "started")[0]["folder_slot"]["created"],
        false
    );
    fixture.wait(&second.worker_id, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.stop(&second.worker_id).unwrap();
    fixture.wait(&second.worker_id, WorkerWaitUntil::Exit);

    // A fresh build starts from empty caches.
    let third = fixture
        .supervisor
        .start(&WorkerStartParams {
            fresh_build: true,
            ..in_slot("w/three")
        })
        .unwrap();
    assert!(!marker.exists());
    assert!(zig_cache.is_dir());
    fixture.wait(&third.worker_id, WorkerWaitUntil::TurnEnd);
}

#[test]
fn credential_variables_are_recognized() {
    for name in [
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_PROFILE",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "NPM_TOKEN",
        "NODE_AUTH_TOKEN",
        "CARGO_REGISTRY_TOKEN",
        "CARGO_REGISTRIES_MY_TOKEN",
        "AZURE_CLIENT_SECRET",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "HOMEBREW_GITHUB_API_TOKEN",
        "DB_PASSWORD",
    ] {
        assert!(is_credential_env(name, false, false), "{name}");
    }
    for name in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "HOME",
        "PATH",
        "TERM",
        "SSH_AUTH_SOCK",
        "ZIG",
        "CARGO_HOME",
    ] {
        assert!(!is_credential_env(name, false, false), "{name}");
    }
    // The CLI's own login on Bedrock or Vertex stays.
    assert!(!is_credential_env("AWS_ACCESS_KEY_ID", true, false));
    assert!(!is_credential_env(
        "GOOGLE_APPLICATION_CREDENTIALS",
        false,
        true
    ));
    assert!(is_credential_env("GITHUB_TOKEN", true, true));
}

fn store_of(supervisor: &WorkerSupervisor) -> &store::Store {
    supervisor.shared.store.as_ref().unwrap()
}

#[test]
fn a_reopened_supervisor_rebuilds_the_same_workers_from_the_store() {
    let fixture = Fixture::new("store-reopen");
    let finished = fixture.start("finish");
    fixture.wait(&finished, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.stop(&finished).unwrap();
    fixture.wait(&finished, WorkerWaitUntil::Exit);
    let asked = fixture.start("pair WebFetch https://example.com");
    fixture.wait_for(&asked, |worker| worker.questions.len() == 2);
    fixture
        .answer_request(&asked, "perm-1", WorkerDecision::Allow)
        .unwrap();
    fixture
        .answer_request(&asked, "perm-2", WorkerDecision::Deny)
        .unwrap();
    fixture.wait(&asked, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.kill(&asked, false).unwrap();
    fixture.wait(&asked, WorkerWaitUntil::Exit);
    let before = fixture.supervisor.list();

    // The journals are only an export now: the store alone rebuilds them.
    for worker in &before {
        std::fs::remove_file(&worker.journal_path).unwrap();
    }
    let reopened = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(reopened.list(), before);
    assert_eq!(
        reopened
            .answer_request_gone(&asked, "perm-1")
            .unwrap_err()
            .to_string(),
        format!("question perm-1 of worker {asked} is no longer pending: answered")
    );
}

impl WorkerSupervisor {
    fn answer_request_gone(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerInfo, WorkerError> {
        self.answer(&WorkerAnswerParams {
            worker_id: worker_id.to_owned(),
            request_id: Some(request_id.to_owned()),
            decision: Some(WorkerDecision::Allow),
            answers: Vec::new(),
            message: None,
            command_id: None,
        })
    }
}

#[test]
fn journals_from_before_the_store_are_imported_once() {
    let fixture = Fixture::new("store-import");
    let dir = fixture.root.join("workers");
    let started = serde_json::json!({"ts_ms": 3, "dir": "herdr", "event": {
        "type": "started", "cwd": "/repo", "name": "old", "pid": 99999}});
    let init = serde_json::json!({"ts_ms": 4, "dir": "out", "event": {
        "type": "system", "subtype": "init", "session_id": "s-2"}});
    let exited = serde_json::json!({"ts_ms": 5, "dir": "herdr", "event": {
        "type": "exited", "code": 0}});
    let stderr = serde_json::json!({"ts_ms": 5, "dir": "err", "raw": "a warning"});
    write_journal(&dir, "w2", &[started.clone(), init.clone(), stderr, exited]);
    write_journal(&dir, "w5", &[started, init]);
    // The fixture's supervisor opened the store before the journals were
    // there; a new server imports them.
    let supervisor = WorkerSupervisor::open(dir.clone(), fixture.root.join("claude-stub"));
    let listed: Vec<(String, WorkerState, String)> = supervisor
        .list()
        .into_iter()
        .map(|worker| (worker.worker_id, worker.state, worker.name))
        .collect();
    assert_eq!(
        listed,
        [
            ("w2".to_owned(), WorkerState::Exited, "old".to_owned()),
            ("w5".to_owned(), WorkerState::Lost, "old".to_owned()),
        ]
    );
    let events = |supervisor: &WorkerSupervisor| -> Vec<(String, String, Option<String>, i64)> {
        store_of(supervisor)
            .connection()
            .prepare("SELECT worker_id, direction, type, ts_ms FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let imported = events(&supervisor);
    let kinds: Vec<(&str, &str, Option<&str>)> = imported
        .iter()
        .map(|(worker, dir, kind, _)| (worker.as_str(), dir.as_str(), kind.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("w2", "herdr", Some("started")),
            ("w2", "out", Some("system")),
            ("w2", "err", None),
            ("w2", "herdr", Some("exited")),
            ("w5", "herdr", Some("started")),
            ("w5", "out", Some("system")),
            ("w5", "herdr", Some("lost")),
        ]
    );
    assert_eq!(imported[0].3, 3);

    // Opened again, nothing is imported twice and the numbers stay taken.
    let again = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    assert_eq!(events(&again), imported);
    assert_eq!(again.list(), supervisor.list());
    let next = again
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap();
    assert_eq!(next.worker_id, "w6");
    again.kill("w6", false).unwrap();
}

#[test]
fn an_unreadable_journal_still_reserves_its_number() {
    let fixture = Fixture::new("store-unreadable");
    let dir = fixture.root.join("workers");
    std::fs::create_dir_all(dir.join("w4.jsonl")).unwrap();
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let next = supervisor
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap();
    assert_eq!(next.worker_id, "w5");
    supervisor.kill("w5", false).unwrap();
}

#[test]
fn a_failed_store_write_marks_the_worker_degraded() {
    let fixture = Fixture::new("store-degraded");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(fixture.supervisor.status(&id).unwrap().degraded, None);
    store_of(&fixture.supervisor)
        .connection()
        .execute_batch(
            "CREATE TRIGGER forced BEFORE INSERT ON events
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    fixture.supervisor.prompt(&id, "finish").unwrap();
    // The status still follows the worker, and says what was not stored.
    let worker = fixture.wait_for(&id, |worker| worker.turns == 2);
    let degraded = worker.degraded.unwrap();
    assert!(degraded.contains("worker store write failed"), "{degraded}");
    assert!(degraded.contains("disk full"), "{degraded}");
    // The journal export still has the events, without a seq.
    let last = fixture.journal(&id).pop().unwrap();
    assert!(last.get("seq").is_none(), "{last}");
    fixture.supervisor.kill(&id, false).unwrap();
}

fn force_store_failure(supervisor: &WorkerSupervisor, failing: bool) {
    let sql = if failing {
        "CREATE TRIGGER forced BEFORE INSERT ON events
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;"
    } else {
        "DROP TRIGGER forced;"
    };
    store_of(supervisor)
        .connection()
        .execute_batch(sql)
        .unwrap();
}

#[test]
fn a_degraded_mark_is_stored_with_the_next_write_and_survives_a_restart() {
    let fixture = Fixture::new("store-degraded-stored");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    force_store_failure(&fixture.supervisor, true);
    fixture.supervisor.prompt(&id, "finish").unwrap();
    fixture.wait_for(&id, |worker| worker.turns == 2);
    // The store takes writes again: the exit is stored with the mark.
    force_store_failure(&fixture.supervisor, false);
    fixture.supervisor.kill(&id, false).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    let last = fixture.journal(&id).pop().unwrap();
    assert!(last.get("seq").is_some(), "{last}");

    let reopened = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let degraded = reopened.status(&id).unwrap().degraded.unwrap();
    assert!(degraded.contains("worker store write failed"), "{degraded}");
    assert!(degraded.contains("disk full"), "{degraded}");
}

#[test]
fn a_restart_after_a_failed_store_write_still_says_the_record_has_a_gap() {
    let fixture = Fixture::new("store-degraded-gap");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    force_store_failure(&fixture.supervisor, true);
    fixture.supervisor.prompt(&id, "finish").unwrap();
    fixture.wait_for(&id, |worker| worker.turns == 2);
    fixture.supervisor.kill(&id, false).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    // The server ends with the mark only in memory; the next one finds the
    // store writable and the journal export holding what it missed.
    force_store_failure(&fixture.supervisor, false);
    let missing = fixture
        .journal(&id)
        .iter()
        .rev()
        .take_while(|record| record.get("seq").is_none())
        .count();
    assert!(missing > 0);

    let reopened = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let degraded = reopened.status(&id).unwrap().degraded.unwrap();
    assert!(
        degraded.contains(&format!("missing the last {missing} event(s)")),
        "{degraded}"
    );
    // And the next restart still says so: the mark went in with `lost`.
    drop(reopened);
    let again = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let degraded = again.status(&id).unwrap().degraded.unwrap();
    assert!(degraded.contains("missing the last"), "{degraded}");
}

#[test]
fn an_imported_journal_without_seqs_has_no_gap() {
    let fixture = Fixture::new("store-imported-no-gap");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.kill(&id, false).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    // A journal from before the store: the same records, without seqs,
    // imported by the next server.
    let path = fixture.root.join("workers").join(format!("{id}.jsonl"));
    let stripped: String = fixture
        .journal(&id)
        .into_iter()
        .map(|mut record| {
            record.as_object_mut().unwrap().remove("seq");
            format!("{record}\n")
        })
        .collect();
    std::fs::write(&path, stripped).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let file = format!("{}{suffix}", store::STORE_FILE);
        let _ = std::fs::remove_file(fixture.root.join("workers").join(file));
    }
    let imported = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(imported.status(&id).unwrap().degraded, None);
    drop(imported);
    let reopened = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(reopened.status(&id).unwrap().degraded, None);
}

#[test]
fn seq_increases_across_workers_in_the_store_and_the_journals() {
    let fixture = Fixture::new("store-seq");
    let first = fixture.start("finish");
    let second = fixture.start("finish");
    fixture.wait(&first, WorkerWaitUntil::TurnEnd);
    fixture.wait(&second, WorkerWaitUntil::TurnEnd);
    let stored: Vec<(i64, String)> = store_of(&fixture.supervisor)
        .connection()
        .prepare("SELECT seq, worker_id FROM events ORDER BY rowid")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(stored.windows(2).all(|pair| pair[0].0 < pair[1].0));
    let mut exported = Vec::new();
    for worker_id in [&first, &second] {
        let seqs: Vec<i64> = fixture
            .journal(worker_id)
            .iter()
            .map(|record| record["seq"].as_i64().unwrap())
            .collect();
        assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
        let mine: Vec<i64> = stored
            .iter()
            .filter(|(_, id)| id == worker_id)
            .map(|(seq, _)| *seq)
            .collect();
        assert_eq!(seqs, mine);
        exported.extend(seqs);
        assert_eq!(
            fixture
                .supervisor
                .with_entry(worker_id, |entry| entry.status.last_seq)
                .unwrap(),
            *mine.last().unwrap()
        );
    }
    exported.sort_unstable();
    assert_eq!(
        exported,
        stored.iter().map(|(seq, _)| *seq).collect::<Vec<_>>()
    );
    for worker_id in [&first, &second] {
        fixture.supervisor.kill(worker_id, false).unwrap();
    }
}

fn request_ids(questions: &[WorkerQuestion]) -> Vec<&str> {
    questions
        .iter()
        .map(|question| question.request_id.as_str())
        .collect()
}

#[test]
fn an_attention_wait_returns_a_question_pending_before_it_at_once() {
    let fixture = Fixture::new("attention-pending");
    let id = fixture.start("classifier Bash git push origin master");
    let worker = fixture.wait_for_question(&id);
    let (attention, blocked) = fixture.attention(&id, None, || {});
    assert_eq!(blocked, 0);
    assert_eq!(attention.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&attention.questions), ["perm-1"]);
    assert_eq!(attention.worker.state, WorkerState::WaitingApproval);
    assert_eq!(Some(attention.seq), worker.seq);
    // Level-triggered: asked again without `after`, the same answer.
    let (again, blocked) = fixture.attention(&id, None, || {});
    assert_eq!(
        (again.reason, blocked),
        (WorkerAttentionReason::Question, 0)
    );
}

#[test]
fn an_attention_wait_wakes_on_new_events_and_skips_what_after_has_seen() {
    let fixture = Fixture::new("attention-after");
    let id = fixture.start("finish");
    let (ended, _) = fixture.attention(&id, None, || {});
    assert_eq!(ended.reason, WorkerAttentionReason::TurnEnd);

    // The ended turn was seen: the wait blocks, and the question the next
    // prompt brings wakes it.
    let (asked, blocked) = fixture.attention(&id, Some(ended.seq), || {
        fixture
            .supervisor
            .prompt(&id, "two Bash git push origin master")
            .unwrap();
    });
    assert!(blocked >= 1);
    assert_eq!(asked.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&asked.questions), ["perm-1"]);
    assert!(asked.seq > ended.seq);

    // After its seq the same question does not return; answering it brings
    // the next one.
    let (next, blocked) = fixture.attention(&id, Some(asked.seq), || {
        fixture
            .answer_request(&id, "perm-1", WorkerDecision::Allow)
            .unwrap();
    });
    assert!(blocked >= 1);
    assert_eq!(next.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&next.questions), ["perm-2"]);

    // The turn's end wakes it.
    let (turn_end, blocked) = fixture.attention(&id, Some(next.seq), || {
        fixture
            .answer_request(&id, "perm-2", WorkerDecision::Deny)
            .unwrap();
    });
    assert!(blocked >= 1);
    assert_eq!(turn_end.reason, WorkerAttentionReason::TurnEnd);
    assert!(turn_end.questions.is_empty());
    assert_eq!(
        turn_end.worker.last_result.unwrap().text.as_deref(),
        Some("allow deny")
    );

    // A request id the CLI uses again is a new question.
    fixture
        .supervisor
        .prompt(&id, "classifier Bash git push origin master")
        .unwrap();
    let (reused, _) = fixture.attention(&id, Some(turn_end.seq), || {});
    assert_eq!(reused.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&reused.questions), ["perm-1"]);

    // The exit wakes it, and a gone worker answers at once whatever `after`
    // says: nothing can follow.
    let (gone, blocked) = fixture.attention(&id, Some(reused.seq), || {
        fixture.supervisor.stop(&id).unwrap();
    });
    assert!(blocked >= 1);
    assert_eq!(gone.reason, WorkerAttentionReason::Gone);
    assert!(gone.questions.is_empty());
    let (still_gone, blocked) = fixture.attention(&id, Some(gone.seq), || {});
    assert_eq!(
        (still_gone.reason, still_gone.seq, blocked),
        (WorkerAttentionReason::Gone, gone.seq, 0)
    );
}

#[test]
fn an_event_committed_between_the_check_and_the_block_is_not_missed() {
    let fixture = Fixture::new("attention-gap");
    let id = fixture.start("block");
    // The stub is quiet once its tool runs: no other event can wake the
    // wait.
    fixture.wait_for(&id, |_| {
        fixture
            .journal(&id)
            .iter()
            .any(|record| record["event"]["type"] == "assistant")
    });
    let number = worker_number(&id).unwrap();
    let (attention, blocked) = fixture.attention(&id, None, || {
        let question = question_from_request(
            "gap-1",
            &serde_json::json!({"tool_name": "Bash", "input": {"command": "ls"}}),
            "asked",
        );
        fixture.supervisor.record(
            number,
            Direction::Herdr,
            &serde_json::json!({"type": "question", "question": question, "input": {}}),
        );
    });
    // Seen at once after the gap, not after the liveness re-check (as long
    // as the hang guard).
    assert_eq!(blocked, 1);
    assert_eq!(attention.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&attention.questions), ["gap-1"]);
}

#[test]
fn a_prompt_returns_its_seq_and_an_interrupt_of_an_ended_turn_is_refused() {
    let fixture = Fixture::new("interrupt-turn");
    let started = fixture
        .supervisor
        .start(&start_params(&fixture.repo, "block", None))
        .unwrap();
    let id = started.worker_id;
    let first = started.turn_seq.unwrap();
    let interrupt = |turn: Option<i64>| {
        fixture.supervisor.interrupt(&WorkerInterruptParams {
            worker_id: id.clone(),
            turn,
            command_id: None,
        })
    };
    assert_eq!(interrupt(Some(first)).unwrap().turn_seq, Some(first));
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Interrupted);
    assert_eq!(worker.turn_seq, Some(first));

    // A repeat after the turn ended sends nothing.
    let repeated = interrupt(Some(first)).unwrap_err();
    assert_eq!(repeated.code(), "worker_turn_ended");

    // Nor does it reach the next turn.
    let second = fixture
        .supervisor
        .prompt(&id, "block")
        .unwrap()
        .turn_seq
        .unwrap();
    assert!(second > first);
    assert_eq!(
        interrupt(Some(first)).unwrap_err().code(),
        "worker_turn_ended"
    );
    assert_eq!(
        interrupt(Some(second + 1000)).unwrap_err().code(),
        "invalid_request"
    );
    let user_seqs: Vec<i64> = fixture
        .journal(&id)
        .iter()
        .filter(|record| record["dir"] == "in" && record["event"]["type"] == "user")
        .map(|record| record["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(user_seqs, [first, second]);
    let interrupts = |fixture: &Fixture| {
        fixture
            .journal(&id)
            .iter()
            .filter(|record| {
                record["dir"] == "in" && record["event"]["request"]["subtype"] == "interrupt"
            })
            .count()
    };
    assert_eq!(interrupts(&fixture), 1);
    assert_eq!(
        fixture.supervisor.status(&id).unwrap().state,
        WorkerState::Working
    );

    assert_eq!(interrupt(Some(second)).unwrap().turn_seq, Some(second));
    assert_eq!(
        fixture.wait(&id, WorkerWaitUntil::TurnEnd).state,
        WorkerState::Interrupted
    );
    assert_eq!(interrupts(&fixture), 2);
}

fn answer_params(
    worker_id: &str,
    decision: WorkerDecision,
    command_id: Option<&str>,
) -> WorkerAnswerParams {
    WorkerAnswerParams {
        worker_id: worker_id.to_owned(),
        request_id: Some("perm-1".into()),
        decision: Some(decision),
        answers: Vec::new(),
        message: None,
        command_id: command_id.map(str::to_owned),
    }
}

/// The control_responses herdr wrote for `request_id`.
fn responses_to(fixture: &Fixture, worker_id: &str, request_id: &str) -> usize {
    fixture
        .journal(worker_id)
        .iter()
        .filter(|record| {
            record["dir"] == "in"
                && record["event"]["type"] == "control_response"
                && record["event"]["response"]["request_id"] == request_id
        })
        .count()
}

fn question_row_state(fixture: &Fixture, worker_id: &str, request_id: &str) -> String {
    store_of(&fixture.supervisor)
        .connection()
        .query_row(
            "SELECT state FROM questions WHERE worker_id = ?1 AND request_id = ?2",
            [worker_id, request_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_repeated_start_command_id_returns_the_worker_it_started() {
    let fixture = Fixture::new("receipt-start");
    let params = WorkerStartParams {
        command_id: Some("item-1:todo/receipts".into()),
        ..start_params(&fixture.repo, "finish", None)
    };
    let first = fixture.supervisor.start(&params).unwrap();
    let again = fixture.supervisor.start(&params).unwrap();
    assert_eq!(again, first);
    assert_eq!(fixture.supervisor.list().len(), 1);

    // The same id with another prompt is refused, and starts nothing.
    let other = WorkerStartParams {
        prompt: "fail".into(),
        ..params.clone()
    };
    let conflict = fixture.supervisor.start(&other).unwrap_err();
    assert_eq!(conflict.code(), "worker_command_conflict");
    assert_eq!(fixture.supervisor.list().len(), 1);

    // A refused start is refused again, with the same code.
    let bad = WorkerStartParams {
        cwd: "relative".into(),
        command_id: Some("item-2".into()),
        ..start_params(&fixture.repo, "finish", None)
    };
    let refused = fixture.supervisor.start(&bad).unwrap_err();
    assert_eq!(refused.code(), "invalid_request");
    let replayed = fixture.supervisor.start(&bad).unwrap_err();
    assert_eq!(
        (replayed.code(), replayed.to_string()),
        (refused.code(), refused.to_string())
    );
    let receipts: Vec<(String, String, Option<String>)> = store_of(&fixture.supervisor)
        .connection()
        .prepare("SELECT command_id, state, worker_id FROM receipts ORDER BY command_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        receipts,
        [
            (
                "item-1:todo/receipts".to_owned(),
                "accepted".to_owned(),
                Some(first.worker_id.clone())
            ),
            ("item-2".to_owned(), "rejected".to_owned(), None),
        ]
    );
}

#[test]
fn a_command_id_is_reserved_with_its_first_event() {
    let fixture = Fixture::new("receipt-reserve");
    let store = store_of(&fixture.supervisor);
    // Every event that reserves a receipt fails, so the start's first
    // event and its receipt are not stored, together.
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER forced BEFORE INSERT ON receipts WHEN NEW.state = 'pending'
             BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
        )
        .unwrap();
    let started = fixture
        .supervisor
        .start(&WorkerStartParams {
            command_id: Some("c1".into()),
            ..start_params(&fixture.repo, "finish", None)
        })
        .unwrap();
    let started_events: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE worker_id = ?1 AND type = 'started'",
            [&started.worker_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(started_events, 0);
    // The outcome is still stored, so the id stays answered.
    assert_eq!(
        store.receipt("c1").unwrap().unwrap().state,
        store::ReceiptState::Accepted
    );
}

#[test]
fn a_repeated_prompt_or_refusal_does_nothing_twice() {
    let fixture = Fixture::new("receipt-prompt");
    let id = fixture.start("block");
    let prompt = |text: &str, command_id: &str| {
        fixture.supervisor.prompt_command(&WorkerPromptParams {
            worker_id: id.clone(),
            text: text.into(),
            command_id: Some(command_id.into()),
        })
    };
    // Refused while the turn runs; the same id stays refused after it.
    assert_eq!(prompt("finish", "p1").unwrap_err().code(), "worker_busy");
    fixture
        .supervisor
        .interrupt(&WorkerInterruptParams {
            worker_id: id.clone(),
            turn: None,
            command_id: Some("i1".into()),
        })
        .unwrap();
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(prompt("finish", "p1").unwrap_err().code(), "worker_busy");

    let first = prompt("finish", "p2").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.turns, 2);
    // The repeat returns the first reply; no second message goes out.
    assert_eq!(prompt("finish", "p2").unwrap(), first);
    let users = fixture
        .journal(&id)
        .iter()
        .filter(|record| record["dir"] == "in" && record["event"]["type"] == "user")
        .count();
    assert_eq!(users, 2);
    assert_eq!(
        prompt("fail", "p2").unwrap_err().code(),
        "worker_command_conflict"
    );
    // Nor is an id reused across methods.
    let stop = fixture.supervisor.stop_command(&WorkerCommandTarget {
        worker_id: id.clone(),
        caller_pane_id: None,
        command_id: Some("p2".into()),
    });
    assert_eq!(stop.unwrap_err().code(), "worker_command_conflict");
}

#[test]
fn a_repeated_answer_command_id_sends_one_control_response() {
    let fixture = Fixture::new("receipt-answer");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    let params = answer_params(&id, WorkerDecision::Allow, Some("a1"));
    // Two at once: one runs, the other waits for its outcome.
    let (first, second) = std::thread::scope(|scope| {
        let one = scope.spawn(|| fixture.supervisor.answer(&params));
        let two = scope.spawn(|| fixture.supervisor.answer(&params));
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(first.unwrap(), second.unwrap());
    let third = fixture.supervisor.answer(&params).unwrap();
    assert!(third.questions.is_empty());
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(responses_to(&fixture, &id, "perm-1"), 1);
    assert_eq!(fixture.herdr_events(&id, "answer_intent").len(), 1);
    assert_eq!(fixture.herdr_events(&id, "answer_sent").len(), 1);
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answered");
    let settled = &fixture.supervisor.status(&id).unwrap().settled_questions;
    assert_eq!(settled.len(), 1);
    assert_eq!(
        (settled[0].request_id.as_str(), settled[0].state),
        ("perm-1", WorkerQuestionState::Answered)
    );
    // Without an id, a second answer finds no pending question.
    let again = fixture
        .supervisor
        .answer(&answer_params(&id, WorkerDecision::Deny, None))
        .unwrap_err();
    assert_eq!(again.code(), "worker_question_gone");
}

#[test]
fn a_failed_answer_write_leaves_the_question_pending_and_retryable() {
    let fixture = Fixture::new("answer-failed");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    {
        let registry = lock(&fixture.supervisor.shared.registry);
        let live = registry.workers[&worker_number(&id).unwrap()]
            .live
            .clone()
            .unwrap();
        live.fail_next_write
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let failed = fixture
        .supervisor
        .answer(&answer_params(&id, WorkerDecision::Allow, Some("a1")))
        .unwrap_err();
    assert_eq!(failed.code(), "worker_io_error");
    let worker = fixture.supervisor.status(&id).unwrap();
    assert_eq!(request_ids(&worker.questions), ["perm-1"]);
    assert_eq!(worker.questions[0].state, WorkerQuestionState::Pending);
    assert_eq!(worker.state, WorkerState::WaitingApproval);
    assert_eq!(fixture.supervisor.pending_questions().len(), 1);
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "pending");
    assert_eq!(fixture.herdr_events(&id, "answer_failed").len(), 1);
    // The refused id stays refused; a new answer goes through.
    let replayed = fixture
        .supervisor
        .answer(&answer_params(&id, WorkerDecision::Allow, Some("a1")))
        .unwrap_err();
    assert_eq!(replayed.code(), "worker_io_error");
    fixture
        .supervisor
        .answer(&answer_params(&id, WorkerDecision::Allow, Some("a2")))
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answered");
}

#[test]
fn a_turn_end_cleanup_does_not_erase_an_answer_in_flight() {
    let fixture = Fixture::new("answer-in-flight");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    let number = worker_number(&id).unwrap();
    // The answer's intent is stored and its write has not been confirmed
    // when the turn ends.
    fixture.supervisor.record(
        number,
        Direction::Herdr,
        &serde_json::json!({"type": "answer_intent", "request_id": "perm-1",
            "tool_name": "WebFetch", "decision": "allow", "by": "user"}),
    );
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answering");
    let answering = fixture.supervisor.status(&id).unwrap();
    assert_eq!(answering.questions[0].state, WorkerQuestionState::Answering);
    assert!(fixture.supervisor.pending_questions().is_empty());
    let refused = fixture
        .supervisor
        .answer(&answer_params(&id, WorkerDecision::Deny, None))
        .unwrap_err();
    assert_eq!(refused.code(), "worker_question_gone");
    assert!(refused.to_string().contains("being sent"), "{refused}");

    fixture.supervisor.record(
        number,
        Direction::Out,
        &serde_json::json!({"type": "result", "subtype": "success", "is_error": false,
            "terminal_reason": "completed", "result": "ended"}),
    );
    let ended = fixture.supervisor.status(&id).unwrap();
    assert_eq!(ended.state, WorkerState::Finished);
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answering");
    assert_eq!(ended.questions[0].state, WorkerQuestionState::Answering);

    fixture.supervisor.record(
        number,
        Direction::Herdr,
        &serde_json::json!({"type": "answer_sent", "request_id": "perm-1"}),
    );
    let sent = fixture.supervisor.status(&id).unwrap();
    assert!(sent.questions.is_empty());
    assert_eq!(
        sent.settled_questions[0].state,
        WorkerQuestionState::Answered
    );
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answered");
}

#[test]
fn a_failed_write_after_the_turn_ended_settles_as_the_cleanup_said() {
    let fixture = Fixture::new("answer-failed-late");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    let number = worker_number(&id).unwrap();
    for (direction, event) in [
        (
            Direction::Herdr,
            serde_json::json!({"type": "answer_intent", "request_id": "perm-1"}),
        ),
        (
            Direction::Out,
            serde_json::json!({"type": "result", "subtype": "success", "is_error": false}),
        ),
        (
            Direction::Herdr,
            serde_json::json!({"type": "answer_failed", "request_id": "perm-1"}),
        ),
    ] {
        fixture.supervisor.record(number, direction, &event);
    }
    let worker = fixture.supervisor.status(&id).unwrap();
    assert!(worker.questions.is_empty());
    assert_eq!(
        worker.settled_questions[0].state,
        WorkerQuestionState::Expired
    );
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "expired");
}

#[test]
fn a_restart_expires_an_answer_in_flight_of_a_gone_worker() {
    let fixture = Fixture::new("answer-restart");
    let dir = fixture.root.join("workers");
    let question = question_from_request(
        "perm-1",
        &serde_json::json!({"tool_name": "Bash", "input": {"command": "ls"}}),
        "asked",
    );
    write_journal(
        &dir,
        "w3",
        &[
            serde_json::json!({"ts_ms": 1, "dir": "herdr", "event": {
                "type": "started", "cwd": "/repo", "name": "old", "pid": 99999}}),
            serde_json::json!({"ts_ms": 2, "dir": "herdr", "event": {
                "type": "question", "question": question, "input": {"command": "ls"}}}),
            serde_json::json!({"ts_ms": 3, "dir": "herdr", "event": {
                "type": "answer_intent", "request_id": "perm-1", "decision": "allow"}}),
        ],
    );
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let worker = supervisor.status("w3").unwrap();
    assert_eq!(worker.state, WorkerState::Lost);
    assert!(worker.questions.is_empty());
    assert_eq!(worker.settled_questions.len(), 1);
    assert_eq!(
        worker.settled_questions[0].state,
        WorkerQuestionState::Expired
    );
    assert!(
        worker.settled_questions[0]
            .how
            .contains("not confirmed sent"),
        "{:?}",
        worker.settled_questions
    );
    let state: String = store_of(&supervisor)
        .connection()
        .query_row(
            "SELECT state FROM questions WHERE worker_id = 'w3' AND request_id = 'perm-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "expired");
}

#[test]
fn a_command_a_restart_cut_off_says_so_and_a_start_returns_its_worker() {
    let fixture = Fixture::new("receipt-cut-off");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let params = WorkerStartParams {
        command_id: Some("s1".into()),
        ..start_params(&fixture.repo, "finish", None)
    };
    let prompt = WorkerPromptParams {
        worker_id: id.clone(),
        text: "finish".into(),
        command_id: Some("p1".into()),
    };
    // Reserved by a server that ended before storing the outcome.
    for (command_id, method, params) in [
        ("s1", "worker.start", command_params(&params)),
        ("p1", "worker.prompt", command_params(&prompt)),
    ] {
        store_of(&fixture.supervisor)
            .transaction(|tx| {
                tx.reserve_receipt(
                    &store::NewReceipt {
                        command_id,
                        method,
                        params: &params.to_string(),
                    },
                    &id,
                    1,
                )
            })
            .unwrap();
    }
    let started = fixture.supervisor.start(&params).unwrap();
    assert_eq!(started.worker_id, id);
    assert_eq!(fixture.supervisor.list().len(), 1);
    let cut_off = fixture.supervisor.prompt_command(&prompt).unwrap_err();
    assert_eq!(cut_off.code(), "worker_command_interrupted");
    assert!(cut_off.to_string().contains(&id), "{cut_off}");
    assert_eq!(fixture.supervisor.status(&id).unwrap().turns, 1);
}

/// Starts a worker owned by `pane`.
fn start_owned(fixture: &Fixture, pane: &str, prompt: &str) -> String {
    fixture
        .supervisor
        .start(&WorkerStartParams {
            owner_pane_id: Some(pane.to_owned()),
            owner_session_id: Some(format!("{pane}-session")),
            ..start_params(&fixture.repo, prompt, None)
        })
        .unwrap()
        .worker_id
}

#[test]
fn workers_started_by_a_coordinators_pane_carry_its_tenure() {
    let fixture = Fixture::new("owner-tenure");
    let tenure = fixture
        .supervisor
        .coordinator_start("/repo", "p1", Some("p1-session"))
        .unwrap();
    let owned = start_owned(&fixture, "p1", "finish");
    let other = start_owned(&fixture, "p2", "finish");
    let worker = fixture.wait(&owned, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        worker.owner_coordinator_id.as_deref(),
        Some(tenure.coordinator_id.as_str())
    );
    // A pane bound to no tenure gives its workers none.
    let worker = fixture.wait(&other, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.owner_coordinator_id, None);
    // The tenure's end does not change what a worker started under.
    fixture
        .supervisor
        .coordinator_end(&tenure.coordinator_id, "ended", None)
        .unwrap();
    let stored = fixture
        .supervisor
        .shared
        .store
        .as_ref()
        .unwrap()
        .load(&owned)
        .unwrap()
        .unwrap();
    assert_eq!(stored.owner_coordinator, Some(tenure.coordinator_id));
}

/// The events recorded under `id` (a worker, run or tenure) of `kind`.
fn events_of(fixture: &Fixture, id: &str, kind: &str) -> Vec<serde_json::Value> {
    fixture
        .supervisor
        .shared
        .store
        .as_ref()
        .unwrap()
        .run_events_of(id, kind)
        .unwrap()
}

#[test]
fn a_cleared_coordinator_session_keeps_the_tenure_and_its_workers() {
    let fixture = Fixture::new("owner-clear");
    let tenure = fixture
        .supervisor
        .coordinator_start("/repo", "p1", Some("before-clear"))
        .unwrap();
    let id = start_owned(&fixture, "p1", "finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);

    // A clear from a session the binding does not have moves nothing.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_session_cleared("p1", Some("stranger"), "after-clear", None)
            .unwrap(),
        None
    );
    // Nor does one in a pane bound to no tenure.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_session_cleared("p2", Some("before-clear"), "after-clear", None)
            .unwrap(),
        None
    );

    let cleared = fixture
        .supervisor
        .coordinator_session_cleared("p1", Some("before-clear"), "after-clear", None)
        .unwrap()
        .unwrap();
    assert_eq!(cleared.coordinator_id, tenure.coordinator_id);
    assert_eq!(
        (cleared.pane_id.as_deref(), cleared.epoch, cleared.ended_ms),
        (Some("p1"), tenure.epoch, None)
    );
    // Seen again, nothing moves.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_session_cleared("p1", Some("before-clear"), "after-clear", None)
            .unwrap(),
        None
    );
    let store = fixture.supervisor.shared.store.as_ref().unwrap();
    let bindings: Vec<(String, Option<String>, bool)> = store
        .connection()
        .prepare(
            "SELECT pane_id, session_id, to_at IS NULL FROM coordinator_bindings
             WHERE coordinator_id = ?1 ORDER BY rowid",
        )
        .unwrap()
        .query_map([&tenure.coordinator_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        bindings,
        [
            ("p1".to_owned(), Some("before-clear".to_owned()), false),
            ("p1".to_owned(), Some("after-clear".to_owned()), true),
        ]
    );
    assert_eq!(
        events_of(
            &fixture,
            &tenure.coordinator_id,
            "coordinator_session_cleared"
        )
        .len(),
        1
    );
    // The worker stays the tenure's, with the new session.
    let worker = fixture.supervisor.status(&id).unwrap();
    assert_eq!(
        (
            worker.owner_pane_id.as_deref(),
            worker.owner_session_id.as_deref(),
            worker.owner_coordinator_id.as_deref()
        ),
        (
            Some("p1"),
            Some("after-clear"),
            Some(tenure.coordinator_id.as_str())
        )
    );
    assert!(obligation_of(&fixture, "p1", &id).is_some());

    // The new session resumed in another pane finds the tenure; the old one
    // no longer does.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_resume("p3", "before-clear", None)
            .unwrap(),
        None
    );
    let resumed = fixture
        .supervisor
        .coordinator_resume("p2", "after-clear", None)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.coordinator_id, tenure.coordinator_id);
    assert_eq!(resumed.pane_id.as_deref(), Some("p2"));
    assert_eq!(
        fixture
            .supervisor
            .status(&id)
            .unwrap()
            .owner_pane_id
            .as_deref(),
        Some("p2")
    );
}

#[test]
fn obligations_follow_the_tenure_to_the_pane_its_session_resumed_in() {
    let fixture = Fixture::new("owner-resume");
    let tenure = fixture
        .supervisor
        .coordinator_start("/repo", "p1", Some("p1-session"))
        .unwrap();
    let id = start_owned(&fixture, "p1", "finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert!(obligation_of(&fixture, "p1", &id).is_some());

    // The coordinator's session comes back in another pane: the same
    // tenure, bound there now, with its worker.
    let resumed = fixture
        .supervisor
        .coordinator_resume("p2", "p1-session", None)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.coordinator_id, tenure.coordinator_id);
    assert_eq!(
        (resumed.pane_id.as_deref(), resumed.epoch, resumed.ended_ms),
        (Some("p2"), tenure.epoch, None)
    );
    let owed = obligation_of(&fixture, "p2", &id).unwrap();
    assert_eq!(owed.owner_pane_id, "p2");
    assert!(obligation_of(&fixture, "p1", &id).is_none());
    let worker = fixture.supervisor.status(&id).unwrap();
    assert_eq!(
        (
            worker.owner_pane_id.as_deref(),
            worker.owner_coordinator_id.as_deref()
        ),
        (Some("p2"), Some(tenure.coordinator_id.as_str()))
    );
    let moved = events_of(&fixture, &id, "owner_moved");
    assert_eq!(moved.len(), 1, "{moved:#?}");
    // Its bindings: p1 from its start to the resume, then p2.
    let store = fixture.supervisor.shared.store.as_ref().unwrap();
    let bindings: Vec<(String, Option<String>, bool)> = store
        .connection()
        .prepare(
            "SELECT pane_id, session_id, to_at IS NULL FROM coordinator_bindings
             WHERE coordinator_id = ?1 ORDER BY rowid",
        )
        .unwrap()
        .query_map([&tenure.coordinator_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        bindings,
        [
            ("p1".to_owned(), Some("p1-session".to_owned()), false),
            ("p2".to_owned(), Some("p1-session".to_owned()), true),
        ]
    );
    assert_eq!(
        events_of(&fixture, &tenure.coordinator_id, "coordinator_resumed").len(),
        1
    );
    // Seen again in the same pane, nothing moves.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_resume("p2", "p1-session", None)
            .unwrap(),
        None
    );

    // Another agent session in the old pane does not inherit it: no
    // tenure, no obligations, and its claim is refused while p2 holds it.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_resume("p1", "another-session", None)
            .unwrap(),
        None
    );
    assert!(fixture.supervisor.obligations(Some("p1")).is_empty());
    let refused = fixture
        .supervisor
        .coordinator_start("/repo", "p1", Some("another-session"))
        .unwrap_err();
    assert_eq!(refused.code(), "coordinator_active");
    // A worker the old pane starts now is not the tenure's.
    let later = start_owned(&fixture, "p1", "finish");
    let later = fixture.wait(&later, WorkerWaitUntil::TurnEnd);
    assert_eq!(later.owner_coordinator_id, None);
    assert!(fixture
        .supervisor
        .obligations(Some("p2"))
        .iter()
        .all(|obligation| obligation.worker_id != later.worker_id));
}

#[test]
fn a_session_resumed_after_its_agent_exited_gets_its_orphaned_tenure_back() {
    let fixture = Fixture::new("owner-reopen");
    let tenure = fixture
        .supervisor
        .coordinator_start("/repo", "p1", Some("p1-session"))
        .unwrap();
    let id = start_owned(&fixture, "p1", "finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    // `claude --resume` starts after the agent exited, here in another pane.
    fixture
        .supervisor
        .owner_event("p1", OwnerEvent::AgentExited, "");
    assert!(fixture
        .supervisor
        .coordinator_status(None)
        .unwrap()
        .is_empty());
    // A different session finds nothing to resume.
    assert_eq!(
        fixture
            .supervisor
            .coordinator_resume("p1", "another-session", None)
            .unwrap(),
        None
    );
    // `coordinator.start` from the resumed session resumes it.
    let resumed = fixture
        .supervisor
        .coordinator_start("/repo", "p3", Some("p1-session"))
        .unwrap();
    assert_eq!(resumed.coordinator_id, tenure.coordinator_id);
    assert_eq!(
        (resumed.ended_ms, resumed.end_reason, resumed.epoch),
        (None, None, tenure.epoch)
    );
    let resumed_events = events_of(&fixture, &tenure.coordinator_id, "coordinator_resumed");
    assert_eq!(resumed_events[0]["reopened"], true);
    let owed = obligation_of(&fixture, "p3", &id).unwrap();
    assert_eq!(owed.owner_pane_id, "p3");

    // Once the repository has another coordinator, an orphaned tenure
    // stays ended.
    fixture
        .supervisor
        .owner_event("p3", OwnerEvent::PaneClosed, "");
    let next = fixture
        .supervisor
        .coordinator_start("/repo", "p4", Some("p4-session"))
        .unwrap();
    assert_ne!(next.coordinator_id, tenure.coordinator_id);
    assert_eq!(
        fixture
            .supervisor
            .coordinator_resume("p5", "p1-session", None)
            .unwrap(),
        None
    );
    // Nor after the next one ended: it is not the repository's latest.
    fixture
        .supervisor
        .coordinator_end(&next.coordinator_id, "ended", None)
        .unwrap();
    assert_eq!(
        fixture
            .supervisor
            .coordinator_resume("p5", "p1-session", None)
            .unwrap(),
        None
    );
}

fn obligation_of(fixture: &Fixture, pane: &str, id: &str) -> Option<WorkerObligation> {
    fixture
        .supervisor
        .obligations(Some(pane))
        .into_iter()
        .find(|obligation| obligation.worker_id == id)
}

#[test]
fn a_worker_is_owned_by_the_pane_and_session_that_started_it() {
    let fixture = Fixture::new("owner");
    let id = start_owned(&fixture, "p1", "finish");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.owner_pane_id.as_deref(), Some("p1"));
    assert_eq!(worker.owner_session_id.as_deref(), Some("p1-session"));
    assert_eq!(worker.acked_seq, None);
    // A worker without an owner owes nobody.
    let unowned = fixture.start("finish");
    fixture.wait(&unowned, WorkerWaitUntil::TurnEnd);
    let all: Vec<String> = fixture
        .supervisor
        .obligations(None)
        .into_iter()
        .map(|obligation| obligation.worker_id)
        .collect();
    assert_eq!(all, std::slice::from_ref(&id));
    assert!(fixture.supervisor.obligations(Some("p2")).is_empty());

    // The owner survives a server restart: it is in the store.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    let worker = reopened.status(&id).unwrap();
    assert_eq!(worker.owner_pane_id.as_deref(), Some("p1"));
    assert_eq!(worker.owner_session_id.as_deref(), Some("p1-session"));
}

#[test]
fn obligations_appear_for_a_question_and_a_turn_end_and_vanish_once_acked() {
    let fixture = Fixture::new("obligations");
    let id = start_owned(&fixture, "p1", "finish");
    let (ended, _) = fixture.attention(&id, None, || {});
    assert_eq!(ended.reason, WorkerAttentionReason::TurnEnd);
    let turn_end = obligation_of(&fixture, "p1", &id).unwrap();
    assert_eq!(turn_end.reason, WorkerAttentionReason::TurnEnd);
    assert_eq!(turn_end.seq, ended.seq);
    assert_eq!(turn_end.owner_pane_id, "p1");

    let worker = fixture.supervisor.ack(&id, turn_end.seq).unwrap();
    assert_eq!(worker.acked_seq, Some(turn_end.seq));
    assert!(obligation_of(&fixture, "p1", &id).is_none());

    // A newer event, a question, brings it back.
    fixture
        .supervisor
        .prompt(&id, "classifier Bash git push origin master")
        .unwrap();
    let (asked, _) = fixture.attention(&id, Some(ended.seq), || {});
    assert_eq!(asked.reason, WorkerAttentionReason::Question);
    let question = obligation_of(&fixture, "p1", &id).unwrap();
    assert_eq!(question.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&question.questions), ["perm-1"]);
    fixture.supervisor.ack(&id, question.seq).unwrap();
    assert!(obligation_of(&fixture, "p1", &id).is_none());

    // Its answer ends the turn: a turn end after the ack.
    fixture
        .answer_request(&id, "perm-1", WorkerDecision::Allow)
        .unwrap();
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let next = obligation_of(&fixture, "p1", &id).unwrap();
    assert_eq!(next.reason, WorkerAttentionReason::TurnEnd);
    assert!(next.seq > question.seq);
    fixture.supervisor.ack(&id, next.seq).unwrap();

    // The worker's end is one too, until acknowledged.
    fixture.supervisor.stop(&id).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    let gone = obligation_of(&fixture, "p1", &id).unwrap();
    assert_eq!(gone.reason, WorkerAttentionReason::Gone);
    fixture.supervisor.ack(&id, gone.seq).unwrap();
    assert!(obligation_of(&fixture, "p1", &id).is_none());
}

#[test]
fn an_exit_the_owner_asked_for_creates_no_obligation() {
    let fixture = Fixture::new("owner-stop");
    let stop = |id: &str, caller: Option<&str>| {
        fixture
            .supervisor
            .stop_command(&WorkerCommandTarget {
                worker_id: id.to_owned(),
                caller_pane_id: caller.map(str::to_owned),
                command_id: None,
            })
            .unwrap();
        fixture.wait(id, WorkerWaitUntil::Exit)
    };

    // The owner stops it, with its turn end not acknowledged yet: the exit
    // acknowledges everything up to itself.
    let id = start_owned(&fixture, "p1", "finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert!(obligation_of(&fixture, "p1", &id).is_some());
    let worker = stop(&id, Some("p1"));
    assert!(obligation_of(&fixture, "p1", &id).is_none());
    assert_eq!(worker.acked_seq, worker.seq);
    // Acknowledged as it was recorded, so it survives a restart.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    assert!(reopened
        .obligations(Some("p1"))
        .iter()
        .all(|obligation| obligation.worker_id != id));

    // Another pane's stop, or one with no caller, is an end the owner has
    // to review.
    for caller in [Some("p2"), None] {
        let id = start_owned(&fixture, "p1", "finish");
        fixture.wait(&id, WorkerWaitUntil::TurnEnd);
        stop(&id, caller);
        let gone = obligation_of(&fixture, "p1", &id).unwrap();
        assert_eq!(gone.reason, WorkerAttentionReason::Gone, "{caller:?}");
    }

    // The owner's kill too.
    let id = start_owned(&fixture, "p1", "finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    fixture
        .supervisor
        .kill_command(&WorkerKillParams {
            worker_id: id.clone(),
            force: false,
            caller_pane_id: Some("p1".into()),
            command_id: None,
        })
        .unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    assert!(obligation_of(&fixture, "p1", &id).is_none());
    // Another pane's kill is not.
    let id = start_owned(&fixture, "p1", "finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    fixture
        .supervisor
        .kill_command(&WorkerKillParams {
            worker_id: id.clone(),
            force: false,
            caller_pane_id: Some("p2".into()),
            command_id: None,
        })
        .unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(
        obligation_of(&fixture, "p1", &id).unwrap().reason,
        WorkerAttentionReason::Gone
    );
}

#[test]
fn an_ack_is_idempotent_and_monotonic() {
    let fixture = Fixture::new("ack");
    let id = start_owned(&fixture, "p1", "finish");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let seq = worker.seq.unwrap();

    // Past the latest event: refused, it would hide what has not happened.
    let refused = fixture.supervisor.ack(&id, seq + 1).unwrap_err();
    assert_eq!(refused.code(), "invalid_request");

    let acked = fixture.supervisor.ack(&id, seq).unwrap();
    assert_eq!(acked.acked_seq, Some(seq));
    let after = acked.seq.unwrap();
    // The same ack, and an older one, record nothing and lower nothing.
    for again in [seq, seq - 1] {
        let worker = fixture.supervisor.ack(&id, again).unwrap();
        assert_eq!((worker.acked_seq, worker.seq), (Some(seq), Some(after)));
    }
    assert_eq!(fixture.herdr_events(&id, "acked").len(), 1);
    assert!(fixture.supervisor.ack("w99", 1).is_err());

    // The acknowledged seq survives a server restart.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    assert_eq!(reopened.status(&id).unwrap().acked_seq, Some(seq));
}

const ASKS: &str = "classifier Bash git push origin master";

/// Each pending question of `id` in the `?` list, with whether it is quiet
/// and why it was escalated.
fn shown_questions(supervisor: &WorkerSupervisor, id: &str) -> Vec<(String, bool, Option<String>)> {
    supervisor
        .pending_questions()
        .into_iter()
        .filter(|pending| pending.worker_id == id)
        .map(|pending| {
            (
                pending.question.request_id,
                pending.quiet,
                pending.question.escalated,
            )
        })
        .collect()
}

/// Starts a worker owned by `pane` that waits on question `perm-1`.
fn owned_question(fixture: &Fixture, pane: &str) -> String {
    let id = start_owned(fixture, pane, ASKS);
    fixture.wait_for_question(&id);
    id
}

#[test]
fn an_owned_question_is_quiet_and_an_unowned_one_is_loud() {
    let fixture = Fixture::new("quiet");
    let owned = owned_question(&fixture, "p1");
    assert_eq!(
        shown_questions(&fixture.supervisor, &owned),
        [("perm-1".to_owned(), true, None)]
    );
    let unowned = fixture.start(ASKS);
    fixture.wait_for_question(&unowned);
    assert_eq!(
        shown_questions(&fixture.supervisor, &unowned),
        [("perm-1".to_owned(), false, None)]
    );
    // Quiet or not, the user may answer it.
    fixture
        .answer_request(&owned, "perm-1", WorkerDecision::Allow)
        .unwrap();
    fixture.wait(&owned, WorkerWaitUntil::TurnEnd);
    assert!(fixture.herdr_events(&owned, "escalated").is_empty());
}

#[test]
fn escalate_makes_the_question_loud_for_good() {
    let fixture = Fixture::new("escalate");
    let id = owned_question(&fixture, "p1");
    let refused = fixture.supervisor.escalate(&id, "perm-9").unwrap_err();
    assert_eq!(refused.code(), "worker_no_question");

    let worker = fixture.supervisor.escalate(&id, "perm-1").unwrap();
    let cause = "its coordinator escalated it";
    assert_eq!(worker.questions[0].escalated.as_deref(), Some(cause));
    assert_eq!(
        shown_questions(&fixture.supervisor, &id),
        [("perm-1".to_owned(), false, Some(cause.to_owned()))]
    );
    let journaled = fixture.herdr_events(&id, "escalated");
    assert_eq!(journaled.len(), 1);
    assert_eq!(journaled[0]["cause"], cause);
    assert_eq!(journaled[0]["request_ids"], serde_json::json!(["perm-1"]));

    // Again, or the owner working again later: it stays loud, once journaled.
    fixture.supervisor.escalate(&id, "perm-1").unwrap();
    fixture
        .supervisor
        .owner_event("p1", OwnerEvent::Working, "");
    assert!(!shown_questions(&fixture.supervisor, &id)[0].1);
    assert_eq!(fixture.herdr_events(&id, "escalated").len(), 1);

    // It survives a restart: the store keeps why.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    assert_eq!(
        shown_questions(&reopened, &id),
        [("perm-1".to_owned(), false, Some(cause.to_owned()))]
    );
    drop(reopened);

    // The coordinator answers it after all: the first answer wins.
    fixture
        .answer_request(&id, "perm-1", WorkerDecision::Allow)
        .unwrap();
    let late = fixture.supervisor.escalate(&id, "perm-1").unwrap_err();
    assert_eq!(late.code(), "worker_question_gone");
}

#[test]
fn a_closed_owner_pane_or_exited_agent_hands_every_question_to_the_user() {
    for (event, cause) in [
        (OwnerEvent::PaneClosed, "the coordinator's pane closed"),
        (OwnerEvent::AgentExited, "the coordinator's agent exited"),
    ] {
        let fixture = Fixture::new("owner-gone");
        let id = owned_question(&fixture, "p1");
        let other = owned_question(&fixture, "p2");
        fixture.supervisor.owner_event("p1", event, "");
        assert_eq!(
            shown_questions(&fixture.supervisor, &id),
            [("perm-1".to_owned(), false, Some(cause.to_owned()))]
        );
        assert_eq!(fixture.herdr_events(&id, "owner_gone")[0]["cause"], cause);
        // Another owner's worker keeps waiting for its owner.
        assert!(shown_questions(&fixture.supervisor, &other)[0].1);
        assert_eq!(fixture.supervisor.owner_panes(), ["p2"]);

        // A later question of the same worker goes to the user at once.
        fixture
            .answer_request(&id, "perm-1", WorkerDecision::Allow)
            .unwrap();
        fixture.wait(&id, WorkerWaitUntil::TurnEnd);
        fixture.supervisor.prompt(&id, ASKS).unwrap();
        fixture.wait_for_question(&id);
        assert!(!shown_questions(&fixture.supervisor, &id)[0].1);
    }
}

#[test]
fn a_limited_or_blocked_owner_hands_its_questions_over_until_it_works_again() {
    for (event, cause) in [
        (
            OwnerEvent::Limited,
            "the coordinator hit a usage or credit limit",
        ),
        (
            OwnerEvent::Blocked,
            "the coordinator is blocked on its own question to the user",
        ),
    ] {
        let fixture = Fixture::new("owner-stuck");
        let id = owned_question(&fixture, "p1");
        fixture.supervisor.owner_event("p1", event, "");
        assert_eq!(
            shown_questions(&fixture.supervisor, &id),
            [("perm-1".to_owned(), false, Some(cause.to_owned()))]
        );
        // One asked while it is stuck goes to the user at once.
        let asked_meanwhile = owned_question(&fixture, "p1");
        fixture.wait_for_status(&asked_meanwhile, |status| {
            status
                .questions
                .iter()
                .all(|pending| pending.escalated.is_some())
        });
        assert_eq!(
            fixture.herdr_events(&asked_meanwhile, "escalated")[0]["cause"],
            cause
        );
        // Working again, it handles new questions itself.
        fixture
            .supervisor
            .owner_event("p1", OwnerEvent::Working, "");
        let later = owned_question(&fixture, "p1");
        assert!(shown_questions(&fixture.supervisor, &later)[0].1);
    }
}

#[test]
fn an_owner_ending_its_turn_hands_over_what_it_left_unanswered() {
    let fixture = Fixture::new("owner-idle");
    let id = owned_question(&fixture, "p1");
    // Acknowledging is not answering.
    let seq = fixture.supervisor.status(&id).unwrap().seq.unwrap();
    fixture.supervisor.ack(&id, seq).unwrap();
    assert!(shown_questions(&fixture.supervisor, &id)[0].1);
    fixture
        .supervisor
        .owner_event("p1", OwnerEvent::TurnEnded, "");
    assert_eq!(
        shown_questions(&fixture.supervisor, &id),
        [(
            "perm-1".to_owned(),
            false,
            Some("the coordinator ended its turn without answering".to_owned())
        )]
    );
    // One asked while it idles stays quiet: its wait wakes it.
    let asked_while_idle = owned_question(&fixture, "p1");
    assert!(shown_questions(&fixture.supervisor, &asked_while_idle)[0].1);
}

#[test]
fn agent_status_changes_map_to_owner_events() {
    use crate::api::schema::AgentStatus::{Blocked, Done, Idle, Unknown, Working};
    let map = OwnerEvent::from_agent_status;
    assert_eq!(map(Working, Idle, false), Some(OwnerEvent::TurnEnded));
    assert_eq!(map(Working, Done, false), Some(OwnerEvent::TurnEnded));
    assert_eq!(map(Blocked, Idle, false), Some(OwnerEvent::TurnEnded));
    assert_eq!(map(Unknown, Idle, false), Some(OwnerEvent::TurnEnded));
    // Seen or not is no turn's end.
    assert_eq!(map(Done, Idle, false), None);
    assert_eq!(map(Idle, Done, false), None);
    assert_eq!(map(Idle, Idle, false), None);
    assert_eq!(map(Idle, Working, false), Some(OwnerEvent::Working));
    assert_eq!(map(Working, Blocked, false), Some(OwnerEvent::Blocked));
    assert_eq!(map(Working, Unknown, false), None);
    assert_eq!(map(Working, Working, true), Some(OwnerEvent::AgentExited));
}

#[test]
fn herdr_re_evaluates_the_owners_when_it_starts() {
    let fixture = Fixture::new("owner-restart");
    let working = owned_question(&fixture, "p1");
    let closed = owned_question(&fixture, "p2");
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    reopened.owners_at_start(|pane| {
        Some(match pane {
            "p1" => OwnerEvent::Working,
            _ => OwnerEvent::PaneClosed,
        })
    });
    assert!(shown_questions(&reopened, &working)[0].1);
    assert_eq!(
        shown_questions(&reopened, &closed),
        [(
            "perm-1".to_owned(),
            false,
            Some("the coordinator's pane closed (found when herdr started)".to_owned())
        )]
    );
}

#[test]
fn a_worker_that_exits_resolves_its_questions_without_escalating() {
    let fixture = Fixture::new("owner-exit");
    let id = owned_question(&fixture, "p1");
    fixture.supervisor.stop(&id).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    assert!(shown_questions(&fixture.supervisor, &id).is_empty());
    assert!(fixture.herdr_events(&id, "escalated").is_empty());
    let settled = fixture.supervisor.status(&id).unwrap().settled_questions;
    assert_eq!(settled[0].how, "the worker exited");
    // Its owner leaving afterwards records nothing for it.
    fixture
        .supervisor
        .owner_event("p1", OwnerEvent::PaneClosed, "");
    assert!(fixture.herdr_events(&id, "owner_gone").is_empty());
}

// Fault injection (plan step 7 of "Make coordinating headless workers
// reliable", docs/headless-worker-fault-tests.md). Pass: no event lost,
// every question resolved or explicitly escalated, no effect twice.

/// The live pipe of a running worker.
fn live_of(fixture: &Fixture, worker_id: &str) -> Arc<Live> {
    let registry = lock(&fixture.supervisor.shared.registry);
    registry.workers[&worker_number(worker_id).unwrap()]
        .live
        .clone()
        .unwrap()
}

#[test]
fn a_coordinator_killed_mid_answer_retries_its_command_and_nothing_is_sent_twice() {
    let fixture = Fixture::new("fault-mid-answer");
    let id = owned_question(&fixture, "p1");
    let (reached_tx, reached) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    *lock(&live_of(&fixture, &id).hold_next_write) = Some((reached_tx, release_rx));
    let params = answer_params(&id, WorkerDecision::Allow, Some("a1"));
    std::thread::scope(|scope| {
        let first = scope.spawn(|| fixture.supervisor.answer(&params));
        // The answer's intent is stored, its write not done: the
        // coordinator that sent it dies here.
        reached.recv_timeout(HANG_GUARD).unwrap();
        assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answering");
        assert!(fixture.herdr_events(&id, "answer_sent").is_empty());
        // Handled as far as its owner is concerned, and off the `?` list.
        assert!(obligation_of(&fixture, "p1", &id).is_none());
        assert!(fixture.supervisor.pending_questions().is_empty());

        // A new coordinator that answers again without the id, or with a
        // new one, is refused: the answer is on its way.
        for command_id in [None, Some("a2")] {
            let refused = fixture
                .supervisor
                .answer(&answer_params(&id, WorkerDecision::Deny, command_id))
                .unwrap_err();
            assert_eq!(refused.code(), "worker_question_gone", "{command_id:?}");
            assert!(refused.to_string().contains("being sent"), "{refused}");
        }
        // Its retry with the same id gets the first call's outcome.
        let retry = scope.spawn(|| fixture.supervisor.answer(&params));
        release.send(()).unwrap();
        let first = first.join().unwrap().unwrap();
        assert_eq!(retry.join().unwrap().unwrap(), first);
    });
    // A retry after it settled replays the stored reply.
    fixture.supervisor.answer(&params).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(responses_to(&fixture, &id, "perm-1"), 1);
    assert_eq!(fixture.herdr_events(&id, "answer_intent").len(), 1);
    assert_eq!(fixture.herdr_events(&id, "answer_sent").len(), 1);
    assert_eq!(question_row_state(&fixture, &id, "perm-1"), "answered");
}

#[test]
fn a_turn_end_before_the_wait_is_re_armed_still_wakes_it() {
    let fixture = Fixture::new("fault-re-arm");
    let id = owned_question(&fixture, "p1");
    let (asked, _) = fixture.attention(&id, None, || {});
    assert_eq!(asked.reason, WorkerAttentionReason::Question);
    fixture
        .answer_request(&id, "perm-1", WorkerDecision::Allow)
        .unwrap();
    // The coordinator dies before it waits again; the turn ends meanwhile.
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let (ended, blocked) = fixture.attention(&id, Some(asked.seq), || {});
    assert_eq!((ended.reason, blocked), (WorkerAttentionReason::TurnEnd, 0));
    assert!(ended.seq > asked.seq);
    // And it is owed until acknowledged, to whichever session comes back.
    let owed = obligation_of(&fixture, "p1", &id).unwrap();
    assert_eq!(
        (owed.reason, owed.seq),
        (WorkerAttentionReason::TurnEnd, ended.seq)
    );
}

#[test]
fn two_questions_at_once_reach_the_wait_and_each_answer_lands_once() {
    let fixture = Fixture::new("fault-two-questions");
    let id = fixture.start("pair WebFetch https://example.com");
    fixture.wait_for(&id, |worker| worker.questions.len() == 2);
    let (asked, blocked) = fixture.attention(&id, None, || {});
    assert_eq!(
        (asked.reason, blocked),
        (WorkerAttentionReason::Question, 0)
    );
    assert_eq!(request_ids(&asked.questions), ["perm-1", "perm-2"]);
    // Answered at once, by two callers.
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| fixture.answer_request(&id, "perm-1", WorkerDecision::Allow));
        let two = scope.spawn(|| fixture.answer_request(&id, "perm-2", WorkerDecision::Deny));
        (one.join().unwrap(), two.join().unwrap())
    });
    one.unwrap();
    two.unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        worker.last_result.unwrap().text.as_deref(),
        Some("perm-1=allow perm-2=deny")
    );
    for request_id in ["perm-1", "perm-2"] {
        assert_eq!(responses_to(&fixture, &id, request_id), 1, "{request_id}");
        assert_eq!(question_row_state(&fixture, &id, request_id), "answered");
    }
}

#[test]
fn two_answers_without_a_request_id_send_one() {
    let fixture = Fixture::new("fault-two-answers");
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    let (allow, deny) = std::thread::scope(|scope| {
        let allow = scope.spawn(|| fixture.answer(&id, Some(WorkerDecision::Allow), &[]));
        let deny = scope.spawn(|| fixture.answer(&id, Some(WorkerDecision::Deny), &[]));
        (allow.join().unwrap(), deny.join().unwrap())
    });
    let (won, lost) = match (&allow, &deny) {
        (Ok(_), Err(lost)) => ("allow", lost),
        (Err(lost), Ok(_)) => ("deny", lost),
        _ => panic!("exactly one answer must win: {allow:?} {deny:?}"),
    };
    assert!(
        ["worker_no_question", "worker_question_gone"].contains(&lost.code()),
        "{lost}"
    );
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let text = worker.last_result.unwrap().text.unwrap();
    assert!(text.starts_with(won), "{won}: {text}");
    assert_eq!(responses_to(&fixture, &id, "perm-1"), 1);
    assert_eq!(fixture.herdr_events(&id, "answer_intent").len(), 1);
}

#[test]
fn a_wait_on_the_next_server_wakes_when_the_old_one_lets_its_worker_go() {
    let fixture = Fixture::new("fault-handoff-wait");
    let id = owned_question(&fixture, "p1");
    let (asked, _) = fixture.attention(&id, None, || {});
    // The next server shares the store, so the coordinator's `after` means
    // the same there.
    let next = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    let seen = next.status(&id).unwrap();
    assert_eq!(seen.seq, Some(asked.seq));
    assert_eq!(request_ids(&seen.questions), ["perm-1"]);
    // Re-armed on the next server while a forced handoff ends the worker
    // on the old one.
    let (gone, blocked) = attention_on(&next, &id, Some(asked.seq), || {
        assert_eq!(
            fixture.supervisor.prepare_for_handoff(true).unwrap(),
            std::slice::from_ref(&id)
        );
    });
    assert!(blocked >= 1);
    assert_eq!(gone.reason, WorkerAttentionReason::Gone);
    assert!(gone.seq > asked.seq);
    let worker = next.status(&id).unwrap();
    assert_eq!(worker.state, WorkerState::Exited);
    assert!(worker.questions.is_empty());
    assert_eq!(worker.settled_questions[0].how, "the worker exited");
    assert!(fixture.herdr_events(&id, "lost").is_empty());
    // Not an exit its owner asked for: the owner has to review it.
    assert_eq!(
        next.obligations(Some("p1"))[0].reason,
        WorkerAttentionReason::Gone
    );
}

#[test]
fn a_restart_with_an_answer_in_flight_to_a_live_process_says_so() {
    let fixture = Fixture::new("fault-restart-answering");
    let dir = fixture.root.join("workers");
    // The previous server died with its worker's process still running
    // and an answer not confirmed sent.
    let leader = spawn_session_leader();
    let question = question_from_request(
        "perm-1",
        &serde_json::json!({"tool_name": "Bash", "input": {"command": "ls"}}),
        "asked",
    );
    write_journal(
        &dir,
        "w3",
        &[
            serde_json::json!({"ts_ms": 1, "dir": "herdr", "event": {
                "type": "started", "cwd": "/repo", "name": "old", "pid": leader.id()}}),
            serde_json::json!({"ts_ms": 2, "dir": "herdr", "event": {
                "type": "question", "question": question, "input": {"command": "ls"}}}),
            serde_json::json!({"ts_ms": 3, "dir": "herdr", "event": {
                "type": "answer_intent", "request_id": "perm-1", "decision": "allow"}}),
        ],
    );
    let supervisor = WorkerSupervisor::open(dir, fixture.root.join("claude-stub"));
    let worker = supervisor.status("w3").unwrap();
    terminate(leader);
    assert_eq!(worker.state, WorkerState::Lost);
    // Not guessed at and not dropped: reported, the question still shown.
    let degraded = worker.degraded.unwrap();
    assert!(
        degraded.contains("may or may not have received it"),
        "{degraded}"
    );
    assert_eq!(request_ids(&worker.questions), ["perm-1"]);
    assert_eq!(worker.questions[0].state, WorkerQuestionState::Answering);
    // A wait answers at once: the worker is gone for this server.
    let (gone, blocked) = attention_on(&supervisor, "w3", None, || {});
    assert_eq!((gone.reason, blocked), (WorkerAttentionReason::Gone, 0));
}

#[test]
fn a_lost_ack_leaves_the_obligation_and_the_wait_returns_it_again() {
    let fixture = Fixture::new("fault-lost-ack");
    let id = start_owned(&fixture, "p1", "finish");
    let (ended, _) = fixture.attention(&id, None, || {});
    // The coordinator handled the turn end but died before its ack.
    let owed = obligation_of(&fixture, "p1", &id).unwrap();
    assert_eq!(owed.seq, ended.seq);
    // Its next session waits from scratch and gets the same event.
    let (again, blocked) = fixture.attention(&id, None, || {});
    assert_eq!(
        (again.reason, again.seq, blocked),
        (WorkerAttentionReason::TurnEnd, ended.seq, 0)
    );
    // Owed across a server restart too.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    let owed = reopened.obligations(Some("p1"));
    assert_eq!(owed.len(), 1);
    assert_eq!(
        (owed[0].reason, owed[0].seq),
        (WorkerAttentionReason::TurnEnd, ended.seq)
    );
    // The late ack clears it; repeated, it records nothing more.
    fixture.supervisor.ack(&id, ended.seq).unwrap();
    fixture.supervisor.ack(&id, ended.seq).unwrap();
    assert!(obligation_of(&fixture, "p1", &id).is_none());
    assert_eq!(fixture.herdr_events(&id, "acked").len(), 1);
}

#[test]
fn two_takeovers_of_an_exited_worker_at_once_claim_it_once() {
    let fixture = Fixture::new("fault-two-takeovers");
    let id = fixture.start("crash");
    fixture.wait(&id, WorkerWaitUntil::Exit);
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| fixture.supervisor.begin_takeover(&id, false));
        let two = scope.spawn(|| fixture.supervisor.begin_takeover(&id, false));
        (one.join().unwrap(), two.join().unwrap())
    });
    let refused = match (one, two) {
        (Ok(_), Err(refused)) | (Err(refused), Ok(_)) => refused,
        (one, two) => panic!(
            "exactly one takeover must win: {:?} {:?}",
            one.map(|takeover| takeover.session_id),
            two.map(|takeover| takeover.session_id)
        ),
    };
    assert_eq!(refused.code(), "worker_busy");
    assert_eq!(fixture.herdr_events(&id, "takeover").len(), 1);
}

#[test]
fn a_failed_journal_write_marks_the_worker_degraded_and_loses_no_event() {
    let fixture = Fixture::new("fault-journal");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    // The export can no longer be written: its open file is dropped and a
    // directory stands at its path.
    let journal = {
        let mut registry = lock(&fixture.supervisor.shared.registry);
        let entry = registry
            .workers
            .get_mut(&worker_number(&id).unwrap())
            .unwrap();
        entry.export = None;
        entry.journal_path.clone()
    };
    std::fs::remove_file(&journal).unwrap();
    std::fs::create_dir(&journal).unwrap();
    fixture.supervisor.prompt(&id, "finish").unwrap();
    let worker = fixture.wait_for(&id, |worker| worker.turns == 2);
    let degraded = worker.degraded.unwrap();
    assert!(
        degraded.contains("a worker journal write failed"),
        "{degraded}"
    );
    // The store, the worker's record, has both turns.
    let results: i64 = store_of(&fixture.supervisor)
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE worker_id = ?1 AND type = 'result'",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(results, 2);
    assert!(fixture
        .supervisor
        .summaries()
        .iter()
        .any(|summary| summary.worker_id == id));
}

#[test]
fn a_question_asked_while_the_store_fails_is_not_lost() {
    let fixture = Fixture::new("fault-store-question");
    let id = start_owned(&fixture, "p1", "finish");
    let (ended, _) = fixture.attention(&id, None, || {});
    store_of(&fixture.supervisor)
        .connection()
        .execute_batch(
            "CREATE TRIGGER forced BEFORE INSERT ON events
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    fixture.supervisor.prompt(&id, ASKS).unwrap();
    // The wait wakes on it, the owner owes it, the `?` list has it.
    let (asked, _) = fixture.attention(&id, Some(ended.seq), || {});
    assert_eq!(asked.reason, WorkerAttentionReason::Question);
    assert_eq!(request_ids(&asked.questions), ["perm-1"]);
    assert!(asked.seq > ended.seq);
    let degraded = asked.worker.degraded.unwrap();
    assert!(degraded.contains("disk full"), "{degraded}");
    assert_eq!(
        obligation_of(&fixture, "p1", &id).unwrap().reason,
        WorkerAttentionReason::Question
    );
    assert_eq!(shown_questions(&fixture.supervisor, &id).len(), 1);
    // It is answered as usual, once.
    fixture
        .answer_request(&id, "perm-1", WorkerDecision::Allow)
        .unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(responses_to(&fixture, &id, "perm-1"), 1);
    // The journal export kept what the store could not, without a seq.
    let question = fixture
        .journal(&id)
        .into_iter()
        .find(|record| record["event"]["type"] == "question")
        .unwrap();
    assert!(question.get("seq").is_none(), "{question}");
}

/// Makes `dir` a git repository; returns the path `repo` shows for it.
fn git_init(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let done = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(done.success());
    dir.canonicalize().unwrap().display().to_string()
}

#[test]
fn runs_are_grouped_by_item_and_repository_and_unassigned_ones_stay_apart() {
    const ITEM: &str = "t-abcd2345";
    let fixture = Fixture::new("runs");
    let repo = git_init(&fixture.repo);
    let other_repo = git_init(&fixture.root.join("other"));
    // The item's title comes from its repository's TODO.md.
    std::fs::write(
        fixture.repo.join("TODO.md"),
        format!("# TODO\n\n- [ ] Items popup for the coordinator [{ITEM}]\n  more\n"),
    )
    .unwrap();
    let start = |cwd: &Path, prompt: &str, item: Option<&str>| {
        fixture
            .supervisor
            .start(&WorkerStartParams {
                item: item.map(str::to_owned),
                ..start_params(cwd, prompt, None)
            })
            .unwrap()
    };
    let stop = |worker_id: &str| {
        fixture.supervisor.stop(worker_id).unwrap();
        fixture.wait(worker_id, WorkerWaitUntil::Exit);
    };

    // The first run of the item is sent back once: one run, two turns.
    let first = start(&fixture.repo, "done abc1234", Some(ITEM));
    assert_eq!(first.item.as_deref(), Some(ITEM));
    assert_eq!(first.repo.as_deref(), Some(repo.as_str()));
    fixture.wait(&first.worker_id, WorkerWaitUntil::TurnEnd);
    fixture
        .supervisor
        .prompt(&first.worker_id, "done DEF5678")
        .unwrap();
    fixture.wait(&first.worker_id, WorkerWaitUntil::TurnEnd);
    stop(&first.worker_id);
    // A second run of the same item fails.
    let second = start(&fixture.repo, "fail", Some(ITEM));
    fixture.wait(&second.worker_id, WorkerWaitUntil::TurnEnd);
    stop(&second.worker_id);
    // The same id in another repository is another item.
    let elsewhere = start(&fixture.root.join("other"), "finish", Some(ITEM));
    // Without an item, whatever its name: unassigned.
    let loose = start(&fixture.repo, "finish", None);
    fixture.wait(&loose.worker_id, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        fixture.supervisor.status(&loose.worker_id).unwrap().item,
        None
    );
    assert!(fixture
        .supervisor
        .list()
        .iter()
        .any(|worker| worker.item.as_deref() == Some(ITEM)));

    let (items, unassigned) = fixture
        .supervisor
        .runs(&WorkerRunsParams::default())
        .unwrap();
    assert_eq!(
        items
            .iter()
            .map(|group| (group.item.as_str(), group.repo.as_deref(), group.runs.len()))
            .collect::<Vec<_>>(),
        [
            (ITEM, Some(repo.as_str()), 2),
            (ITEM, Some(other_repo.as_str()), 1)
        ]
    );
    assert_eq!(
        items[0].title.as_deref(),
        Some("Items popup for the coordinator")
    );
    // No TODO.md there: no title.
    assert_eq!(items[1].title, None);
    let runs = &items[0].runs;
    assert_eq!(runs[0].worker_id, first.worker_id);
    assert_eq!(runs[0].outcome, WorkerRunOutcome::Finished);
    assert_eq!(runs[0].turns, 2);
    assert_eq!(runs[0].commits, ["abc1234", "def5678"]);
    assert_eq!(runs[0].questions, 0);
    let (started, ended) = (runs[0].started_ms.unwrap(), runs[0].ended_ms.unwrap());
    assert!(started <= ended, "{started} > {ended}");
    assert!(runs[0]
        .journal_path
        .ends_with(&format!("{}.jsonl", first.worker_id)));
    assert_eq!(runs[1].worker_id, second.worker_id);
    assert_eq!(runs[1].outcome, WorkerRunOutcome::Failed);
    assert!(runs[1].commits.is_empty());
    assert_eq!(items[1].runs[0].worker_id, elsewhere.worker_id);
    assert_eq!(
        unassigned
            .iter()
            .map(|run| (run.worker_id.as_str(), run.outcome, run.ended_ms))
            .collect::<Vec<_>>(),
        [(loose.worker_id.as_str(), WorkerRunOutcome::Running, None)]
    );

    // Filtered by item and repository (any directory in it names it).
    let (items, unassigned) = fixture
        .supervisor
        .runs(&WorkerRunsParams {
            item: Some(ITEM.into()),
            repo: Some(fixture.root.join("other").display().to_string()),
        })
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].runs[0].worker_id, elsewhere.worker_id);
    assert!(unassigned.is_empty());
    let (items, unassigned) = fixture
        .supervisor
        .runs(&WorkerRunsParams {
            item: Some("t-zzzz2222".into()),
            repo: None,
        })
        .unwrap();
    assert!(items.is_empty() && unassigned.is_empty());

    // The runs survive a restart: a new supervisor reads them from the store.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    let (items, _) = reopened.runs(&WorkerRunsParams::default()).unwrap();
    assert_eq!(items[0].runs[0].commits, ["abc1234", "def5678"]);
    assert_eq!(items[0].runs[0].outcome, WorkerRunOutcome::Finished);
}

#[test]
fn an_item_that_is_not_a_todo_id_is_refused() {
    let fixture = Fixture::new("bad-item");
    for bad in [
        "",
        "t-abc",
        "t-ABCD2345",
        "t-abcd2389",
        "x-abcd2345",
        "t-abcd23456",
    ] {
        let refused = fixture.supervisor.start(&WorkerStartParams {
            item: Some(bad.into()),
            ..start_params(&fixture.repo, "finish", None)
        });
        assert!(matches!(refused, Err(WorkerError::Invalid(_))), "{bad:?}");
        let refused = fixture.supervisor.runs(&WorkerRunsParams {
            item: Some(bad.into()),
            repo: None,
        });
        assert!(matches!(refused, Err(WorkerError::Invalid(_))), "{bad:?}");
    }
    assert!(fixture.supervisor.list().is_empty());
}

#[test]
fn done_commits_are_read_from_worker_done_lines() {
    assert_eq!(
        done_commits("x\nWORKER-DONE 3ecc4be6 | summary\n  WORKER-DONE abcdef0123 | more"),
        ["3ecc4be6", "abcdef0123"]
    );
    assert!(
        done_commits("WORKER-DONE <sha> | x\nWORKER-BLOCKED no\nWORKER-DONE abc | x").is_empty()
    );
}

#[test]
fn an_ended_worker_is_listed_until_its_owner_acknowledges_the_end() {
    const ITEM: &str = "t-abcd2345";
    let fixture = Fixture::new("listed");
    let repo = git_init(&fixture.repo);
    let start = |prompt: &str, owner: Option<&str>, item: Option<&str>| {
        fixture
            .supervisor
            .start(&WorkerStartParams {
                owner_pane_id: owner.map(str::to_owned),
                item: item.map(str::to_owned),
                ..start_params(&fixture.repo, prompt, None)
            })
            .unwrap()
            .worker_id
    };
    let listed = |id: &str| {
        fixture
            .supervisor
            .summaries()
            .into_iter()
            .find(|summary| summary.worker_id == id)
            .unwrap()
            .listed
    };
    let owned = start("finish", Some("p1"), Some(ITEM));
    let crashed = start("crash", Some("p1"), Some("t-qrst6723"));
    let loose = start("finish", None, None);
    // Another repository's item counts only there.
    let elsewhere_repo = git_init(&fixture.root.join("other"));
    let elsewhere = fixture
        .supervisor
        .start(&WorkerStartParams {
            owner_pane_id: Some("p1".into()),
            item: Some(ITEM.into()),
            ..start_params(&fixture.root.join("other"), "finish", None)
        })
        .unwrap()
        .worker_id;
    fixture.wait(&elsewhere, WorkerWaitUntil::TurnEnd);
    assert_eq!(
        fixture.supervisor.item_counts(&elsewhere_repo),
        ItemCounts {
            in_progress: 1,
            attention: 0
        }
    );
    fixture.wait(&owned, WorkerWaitUntil::TurnEnd);
    fixture.wait(&crashed, WorkerWaitUntil::Exit);
    assert!(listed(&owned), "a running worker is listed");
    // The crash is not hidden while its owner has not handled it.
    assert!(listed(&crashed));
    assert_eq!(
        fixture.supervisor.item_counts(&repo),
        ItemCounts {
            in_progress: 1,
            attention: 1
        }
    );
    let gone = obligation_of(&fixture, "p1", &crashed).unwrap();
    fixture.supervisor.ack(&crashed, gone.seq).unwrap();
    assert!(!listed(&crashed), "acknowledged: it leaves the sidebar");

    // An end its owner did not ask for stays until acknowledged.
    fixture.supervisor.stop(&owned).unwrap();
    fixture.wait(&owned, WorkerWaitUntil::Exit);
    assert!(listed(&owned));
    assert_eq!(
        fixture.supervisor.item_counts(&repo),
        ItemCounts {
            in_progress: 0,
            attention: 1
        }
    );
    let gone = obligation_of(&fixture, "p1", &owned).unwrap();
    fixture.supervisor.ack(&owned, gone.seq).unwrap();
    assert!(!listed(&owned));
    assert_eq!(fixture.supervisor.item_counts(&repo), ItemCounts::default());

    // Nobody acknowledges a worker without an owner: it leaves at its end.
    fixture.wait(&loose, WorkerWaitUntil::TurnEnd);
    assert!(listed(&loose));
    fixture.supervisor.stop(&loose).unwrap();
    fixture.wait(&loose, WorkerWaitUntil::Exit);
    assert!(!listed(&loose));
}

#[test]
fn a_run_keeps_its_items_title_from_its_start_after_the_item_leaves_the_todo() {
    const ITEM: &str = "t-abcd2345";
    let fixture = Fixture::new("item-title");
    git_init(&fixture.repo);
    let todo = fixture.repo.join("TODO.md");
    let start = || {
        let worker = fixture
            .supervisor
            .start(&WorkerStartParams {
                item: Some(ITEM.into()),
                ..start_params(&fixture.repo, "finish", None)
            })
            .unwrap()
            .worker_id;
        fixture.wait(&worker, WorkerWaitUntil::TurnEnd);
        worker
    };
    let title = || {
        let (items, _) = fixture
            .supervisor
            .runs(&WorkerRunsParams::default())
            .unwrap();
        items[0].title.clone()
    };

    // Started while TODO.md had no such item: nothing stored, so the title
    // is read from TODO.md once it has one.
    start();
    std::fs::write(&todo, format!("- [ ] Read later [{ITEM}]\n")).unwrap();
    assert_eq!(title().as_deref(), Some("Read later"));

    // Started while TODO.md had it: stored with the run, kept after the
    // finished item is deleted, and across a restart.
    std::fs::write(&todo, format!("- [ ] Items popup [{ITEM}]\n")).unwrap();
    let stored = start();
    let stored_title = {
        let registry = lock(&fixture.supervisor.shared.registry);
        registry.workers[&worker_number(&stored).unwrap()]
            .status
            .item_title
            .clone()
    };
    assert_eq!(stored_title.as_deref(), Some("Items popup"));
    std::fs::write(&todo, "# TODO\n").unwrap();
    assert_eq!(title().as_deref(), Some("Items popup"));
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    let (items, _) = reopened.runs(&WorkerRunsParams::default()).unwrap();
    assert_eq!(items[0].title.as_deref(), Some("Items popup"));
}

#[test]
fn herdr_verifies_a_workers_commit_and_its_run_keeps_the_verdict() {
    use crate::api::schema::{WorkerCheckOutcome, WorkerVerdict, WorkerVerifyParams};
    let fixture = Fixture::new("verify");
    let repo = &fixture.repo;
    git_in(repo, &["init", "-q", "-b", "master"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git_in(repo, &["add", "."]);
    git_in(repo, &["commit", "-q", "-m", "init"]);
    let base = git_in(repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    // The worker's commit.
    std::fs::write(repo.join("a.txt"), "b\n").unwrap();
    git_in(repo, &["commit", "-q", "-am", "feat: b"]);
    let params = WorkerVerifyParams {
        worker_id: id.clone(),
        base,
        expected_message: "feat: b".into(),
        allowed_paths: vec!["*.txt".into()],
        // The caller's environment, not the server's, reaches the command.
        command: Some("test \"$(cat a.txt)\" = \"$VERIFY_EXPECTED\"".into()),
        generated: Vec::new(),
        env: Some(
            [
                ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
                ("VERIFY_EXPECTED".to_owned(), "b".to_owned()),
            ]
            .into(),
        ),
    };

    // While its process runs, the commit is not ready.
    let running = fixture.supervisor.verify(&params).unwrap();
    assert_eq!(running.verdict, WorkerVerdict::Failed);
    let processes = running
        .checks
        .iter()
        .find(|check| check.check == "processes")
        .unwrap();
    assert_eq!(processes.outcome, WorkerCheckOutcome::Failed);
    assert!(processes.detail.contains("stop it first"), "{processes:?}");

    fixture.supervisor.stop(&id).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    let verified = fixture.supervisor.verify(&params).unwrap();
    assert_eq!(verified.verdict, WorkerVerdict::Verified, "{verified:#?}");
    // Journaled as the worker's event, the latest one kept with its run.
    let events = fixture.herdr_events(&id, "verification");
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["verification"]["verdict"], "verified");
    let (_, unassigned) = fixture
        .supervisor
        .runs(&WorkerRunsParams::default())
        .unwrap();
    assert_eq!(unassigned[0].verification.as_ref(), Some(&verified));

    // It survives a restart, read from the store.
    let reopened = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    let (_, unassigned) = reopened.runs(&WorkerRunsParams::default()).unwrap();
    assert_eq!(unassigned[0].verification.as_ref(), Some(&verified));

    let missing = fixture
        .supervisor
        .verify(&WorkerVerifyParams {
            worker_id: "w999".into(),
            ..params
        })
        .unwrap_err();
    assert_eq!(missing.code(), "worker_not_found");
}

/// Polls `done` until it holds: a process's end, which sends this test no
/// event. The hang guard only fails a broken test.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < HANG_GUARD, "{what} hung");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `worker.wait` on another supervisor than the fixture's.
fn wait_on(supervisor: &WorkerSupervisor, worker_id: &str, until: WorkerWaitUntil) -> WorkerInfo {
    let started = Instant::now();
    supervisor
        .wait(worker_id, until, Duration::from_millis(100), || {
            assert!(started.elapsed() < HANG_GUARD, "worker {worker_id} hung");
            true
        })
        .unwrap()
        .unwrap()
}

fn broker_of(supervisor: &WorkerSupervisor, worker_id: &str) -> BrokerRecord {
    let number = worker_number(worker_id).unwrap();
    lock(&supervisor.shared.registry).workers[&number]
        .status
        .broker
        .clone()
        .unwrap()
}

/// A pre-tool check that denies a Bash `sleep 5` and allows everything else.
const SLEEP_CHECK: &str = r#"#!/usr/bin/env python3
import json, sys
call = json.load(sys.stdin)
if "sleep 5" in call.get("tool_input", {}).get("command", ""):
    json.dump({"hookSpecificOutput": {"hookEventName": "PreToolUse",
               "permissionDecision": "deny",
               "permissionDecisionReason": "sleep 5 waits for nothing"}}, sys.stdout)
"#;

impl Fixture {
    /// Configures [`SLEEP_CHECK`] as the only pre-tool check.
    fn with_sleep_check(self) -> Self {
        let check = self.root.join("sleep-check.py");
        std::fs::write(&check, SLEEP_CHECK).unwrap();
        std::fs::set_permissions(&check, std::fs::Permissions::from_mode(0o755)).unwrap();
        *lock(&self.supervisor.shared.pre_tool_checks) = vec![vec![check.display().to_string()]];
        self
    }
}

fn hook_denial(text: &str) -> Option<String> {
    let answer: Value = serde_json::from_str(text).ok()?;
    let output = &answer["hookSpecificOutput"];
    (output["permissionDecision"] == "deny").then(|| {
        output["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    })
}

#[test]
fn pre_tool_checks_deny_sleep_5_and_allow_ls() {
    let fixture = Fixture::new("pre-tool-checks").with_sleep_check();

    let id = fixture.start("hook Bash sleep 5 && ls");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let text = worker.last_result.unwrap().text.unwrap();
    assert_eq!(
        hook_denial(&text).as_deref(),
        Some("sleep 5 waits for nothing"),
        "{text}"
    );
    let decisions = fixture.herdr_events(&id, "pre_tool_check");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["decision"], "deny");
    assert_eq!(decisions[0]["tool_name"], "Bash");
    assert_eq!(decisions[0]["tool_use_id"], "toolu-1");
    assert!(fixture
        .herdr_events(&id, "pre_tool_check_failed")
        .is_empty());
    let log: Vec<String> = fixture
        .journal(&id)
        .iter()
        .flat_map(|record| log::log_lines(&record.to_string()))
        .collect();
    assert!(
        log.iter()
            .any(|line| line.contains("Bash denied by a pre-tool check: sleep 5 waits for nothing")),
        "{log:#?}"
    );

    // No objection is an empty answer, never an `allow` that would skip
    // herdr's policy.
    fixture.supervisor.prompt(&id, "hook Bash ls").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("{}"));
    let decisions = fixture.herdr_events(&id, "pre_tool_check");
    assert_eq!(decisions[1]["decision"], "allow");

    // Registered for the edit tools too, not for reads.
    for (tool, registered) in [
        ("Write", true),
        ("Edit", true),
        ("MultiEdit", true),
        ("Read", false),
    ] {
        fixture
            .supervisor
            .prompt(&id, &format!("hook {tool} x"))
            .unwrap();
        let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
        let text = worker.last_result.unwrap().text.unwrap();
        assert_eq!(text != "unregistered", registered, "{tool}: {text}");
    }
}

#[test]
fn without_pre_tool_checks_the_hook_objects_to_nothing_and_runs_nothing() {
    let fixture = Fixture::new("no-pre-tool-checks");
    let id = fixture.start("hook Bash sleep 5");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("{}"));
    assert!(fixture.herdr_events(&id, "pre_tool_check").is_empty());
    assert!(fixture.herdr_events(&id, "permission").is_empty());
}

#[test]
fn headless_workers_may_not_wait_in_the_background() {
    // Denied with no checks configured, and before a check that would allow.
    for fixture in [
        Fixture::new("background-waits"),
        Fixture::new("background-waits-checked").with_sleep_check(),
    ] {
        let id = fixture.start("hook-bg Bash just test");
        for (prompt, tool) in [
            (None, "Bash"),
            (Some("hook Monitor tail -f log"), "Monitor"),
        ] {
            if let Some(prompt) = prompt {
                fixture.supervisor.prompt(&id, prompt).unwrap();
            }
            let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
            let text = worker.last_result.unwrap().text.unwrap();
            assert_eq!(
                hook_denial(&text).as_deref(),
                Some(policy::BACKGROUND_WAIT_DENIAL),
                "{tool}: {text}"
            );
            let denials = fixture.herdr_events(&id, "permission");
            let last = denials.last().unwrap();
            assert_eq!(last["tool_name"], tool);
            assert_eq!(last["decision"], "deny");
        }
        assert!(fixture.herdr_events(&id, "pre_tool_check").is_empty());

        // The same command in the foreground goes on.
        fixture
            .supervisor
            .prompt(&id, "hook Bash just test")
            .unwrap();
        let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
        assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("{}"));
        assert_eq!(fixture.herdr_events(&id, "permission").len(), 2);
    }
}

#[test]
fn a_pre_tool_check_that_cannot_run_is_reported_and_lets_the_call_go_on() {
    let fixture = Fixture::new("pre-tool-check-missing");
    let missing = fixture.root.join("missing-check").display().to_string();
    *lock(&fixture.supervisor.shared.pre_tool_checks) = vec![vec![missing.clone()], Vec::new()];
    let id = fixture.start("hook Bash sleep 5");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("{}"));
    let failures = fixture.herdr_events(&id, "pre_tool_check_failed");
    // The empty argv when the worker started, the missing program at the call.
    assert_eq!(failures.len(), 2, "{failures:#?}");
    assert!(failures[0]["error"]
        .as_str()
        .unwrap()
        .contains("no program"));
    assert_eq!(failures[1]["check"], serde_json::json!([missing]));
    assert!(failures[1]["error"]
        .as_str()
        .unwrap()
        .starts_with("cannot start"));
    assert_eq!(
        fixture.herdr_events(&id, "pre_tool_check")[0]["decision"],
        "allow"
    );
}

#[test]
fn a_new_server_runs_the_pre_tool_checks_the_worker_started_with() {
    let fixture = Fixture::with_broker().with_sleep_check();
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(broker_of(&fixture.supervisor, &id).pre_tool_checks.len(), 1);
    let (_, live, _) = fixture.supervisor.live(&id).unwrap();
    live.sever();

    // The new server has no checks configured: the worker's record has them.
    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let number = worker_number(&id).unwrap();
    {
        let started = Instant::now();
        let mut registry = lock(&next.shared.registry);
        while registry.workers[&number].live.is_none() {
            assert!(started.elapsed() < HANG_GUARD, "the re-attach hung");
            registry = next
                .shared
                .changed
                .wait_timeout(registry, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }
    next.prompt(&id, "hook Bash sleep 5").unwrap();
    let worker = wait_on(&next, &id, WorkerWaitUntil::TurnEnd);
    let text = worker.last_result.unwrap().text.unwrap();
    assert_eq!(
        hook_denial(&text).as_deref(),
        Some("sleep 5 waits for nothing"),
        "{text}"
    );
    next.stop(&id).unwrap();
    wait_on(&next, &id, WorkerWaitUntil::Exit);
}

#[test]
fn through_a_broker_a_turn_runs_and_the_workers_exit_ends_the_broker() {
    let fixture = Fixture::with_broker();
    let id = fixture.start("finish");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    let pid = worker.pid.unwrap();
    let broker = broker_of(&fixture.supervisor, &id);
    assert_ne!(broker.pid, pid);
    assert_eq!(
        broker.socket,
        fixture.root.join("workers").join(format!("{id}.sock"))
    );
    assert!(broker.socket.exists());
    // The worker runs in its own process group, outside the broker's.
    assert!(crate::platform::process_group_alive(pid));
    assert!(crate::platform::process_group_alive(broker.pid));
    // The broker's spec does not reach the worker.
    let init = fixture
        .journal(&id)
        .into_iter()
        .find(|record| record["dir"] == "out" && record["event"]["subtype"] == "init")
        .unwrap();
    assert_eq!(init["event"]["herdr_env"], serde_json::json!([]));
    // Stored, so a later server finds the broker.
    let store = store::Store::open(&fixture.root.join("workers").join(store::STORE_FILE)).unwrap();
    assert_eq!(
        store.load(&id).unwrap().unwrap().broker,
        Some(broker.clone())
    );

    fixture.supervisor.prompt(&id, "finish").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.turns, 2);

    fixture.supervisor.prompt(&id, "crash").unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.exit_code, Some(3));
    wait_until("the broker's end", || {
        !crate::platform::process_group_alive(broker.pid)
    });
    assert!(!broker.socket.exists());
    // Nothing went wrong, so its empty log is gone too.
    assert!(!fixture
        .root
        .join("workers")
        .join(format!("{id}.broker.log"))
        .exists());
}

#[test]
fn a_broker_test_leaves_no_broker_behind_even_when_it_fails() {
    let (sender, receiver) = std::sync::mpsc::channel();
    // `move`: the closure owns the sender, so a panic before its send (no
    // broker could start) is seen below instead of a `recv` that never ends.
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let fixture = Fixture::with_broker();
        let id = fixture.start("block");
        let (_, live, _) = fixture.supervisor.live(&id).unwrap();
        sender
            .send(broker_of(&fixture.supervisor, &id).pid)
            .unwrap();
        // A server gone mid-turn: the fixture's kill no longer reaches the
        // worker, and its broker would serve it forever.
        live.sever();
        panic!("a failing broker test");
    }));
    let Err(panic) = failed else {
        panic!("the failing broker test passed");
    };
    // The closure has returned: its send happened or never will.
    let Ok(broker) = receiver.try_recv() else {
        // It failed before its broker started: report that failure.
        std::panic::resume_unwind(panic);
    };
    // Reaped: not even a zombie is left.
    assert_ne!(unsafe { libc::kill(broker as libc::pid_t, 0) }, 0);
}

#[test]
fn a_server_gone_mid_turn_leaves_the_worker_running_and_a_new_server_reattaches() {
    let fixture = Fixture::with_broker();
    let id = fixture.start("block");
    let (_, live, status) = fixture.supervisor.live(&id).unwrap();
    let pid = status.pid.unwrap();
    let broker = broker_of(&fixture.supervisor, &id);
    // The server dies mid-turn: its side of the broker's socket goes away
    // and its reader ends without recording anything.
    live.sever();

    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let number = worker_number(&id).unwrap();
    {
        let started = Instant::now();
        let mut registry = lock(&next.shared.registry);
        while registry.workers[&number].live.is_none() {
            assert!(started.elapsed() < HANG_GUARD, "the re-attach hung");
            registry = next
                .shared
                .changed
                .wait_timeout(registry, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }
    assert!(crate::platform::process_group_alive(pid));
    assert!(crate::platform::process_group_alive(broker.pid));
    // The CLI's `system/init` is stored once, whichever server read it: the
    // broker replays what the gone server did not store.
    wait_on_status(&next, &id, |status| status.session_id.is_some());
    let worker = next.status(&id).unwrap();
    assert_eq!(worker.state, WorkerState::Working);
    assert_eq!(worker.session_id.as_deref(), Some("stub-session"));
    let inits = fixture
        .journal(&id)
        .into_iter()
        .filter(|record| record["dir"] == "out" && record["event"]["subtype"] == "init")
        .count();
    assert_eq!(inits, 1);

    // The turn the gone server began ends through the new one.
    next.interrupt(&WorkerInterruptParams {
        worker_id: id.clone(),
        turn: None,
        command_id: None,
    })
    .unwrap();
    let worker = wait_on(&next, &id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Interrupted);
    next.prompt(&id, "finish").unwrap();
    let worker = wait_on(&next, &id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.pid, Some(pid));
    assert_eq!(fixture.herdr_events(&id, "reattached").len(), 1);
    assert!(fixture.herdr_events(&id, "continuity_gap").is_empty());
    assert!(fixture.herdr_events(&id, "lost").is_empty());

    next.stop(&id).unwrap();
    let worker = wait_on(&next, &id, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Exited);
    wait_until("the broker's end", || {
        !crate::platform::process_group_alive(broker.pid)
    });
    assert!(!broker.socket.exists());
}

/// Folds `events` into a new status as the store does, without one.
fn folded(events: &[(Direction, Value)]) -> Status {
    let mut status = Status::new("w1".into());
    for (seq, (direction, event)) in (1..).zip(events) {
        let before = status.before();
        status.apply(*direction, event);
        status.mark_seq(seq, 0, &before, *direction, &store::Recorded::Event(event));
    }
    status
}

/// Waits, woken by state changes, until the internal status of a worker
/// of `supervisor` satisfies `done`.
fn wait_on_status(supervisor: &WorkerSupervisor, worker_id: &str, done: impl Fn(&Status) -> bool) {
    let started = Instant::now();
    let number = worker_number(worker_id).unwrap();
    let mut registry = lock(&supervisor.shared.registry);
    while !registry
        .workers
        .get(&number)
        .is_some_and(|entry| done(&entry.status))
    {
        assert!(started.elapsed() < HANG_GUARD, "worker {worker_id} hung");
        registry = supervisor
            .shared
            .changed
            .wait_timeout(registry, Duration::from_millis(100))
            .unwrap()
            .0;
    }
}

#[test]
fn a_reattach_after_a_lost_init_waits_for_the_replayed_init() {
    let started = (
        Direction::Herdr,
        json!({"type": "started", "cwd": "/repo", "pid": 4242,
               "broker": {"pid": 4241, "socket": "/state/w1.sock", "cwd": "/repo"}}),
    );
    let prompt = (Direction::In, user_message("block"));
    let reattached = (
        Direction::Herdr,
        json!({"type": "reattached", "broker_pid": 4241}),
    );
    let init = (
        Direction::Out,
        json!({"type": "system", "subtype": "init", "session_id": "s"}),
    );

    // The gone server took the CLI's `system/init` and stored nothing: the
    // re-attach itself changes nothing.
    let status = folded(&[started.clone(), prompt.clone(), reattached.clone()]);
    assert_eq!(status.state, WorkerState::Starting);
    assert_eq!(status.session_id, None);
    // The broker replays the init after the re-attach: the turn works and
    // the session is known, as a takeover needs.
    let status = folded(&[started.clone(), prompt.clone(), reattached.clone(), init]);
    assert_eq!(status.state, WorkerState::Working);
    assert_eq!(status.session_id.as_deref(), Some("s"));

    // Without the init, because continuity is not proven, nothing is made
    // up: the worker stays starting, with no session, degraded.
    let gap = (
        Direction::Herdr,
        json!({"type": "continuity_gap", "reason": "lines 2..=4 are missing"}),
    );
    let status = folded(&[started, prompt, reattached, gap]);
    assert_eq!(status.state, WorkerState::Starting);
    assert_eq!(status.session_id, None);
    assert_eq!(
        status.continuity_gap.as_deref(),
        Some("lines 2..=4 are missing")
    );
    assert_eq!(
        status.degraded.as_deref(),
        Some("output continuity not proven: lines 2..=4 are missing")
    );
}

#[test]
fn a_worker_whose_spool_does_not_continue_the_store_takes_no_prompt_or_takeover() {
    let fixture = Fixture::new("gap");
    let id = fixture.start("finish");
    let before = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let number = worker_number(&id).unwrap();
    // The store holds up to seq 2; the spool goes on at 5.
    let event = json!({"type": "rate_limit_event"});
    assert_eq!(
        fixture.supervisor.record_spooled(
            number,
            2,
            Direction::Out,
            store::Recorded::Event(&event)
        ),
        Spooled::Stored
    );
    let spool = fixture.root.join("w.spool");
    std::fs::write(&spool, "o 5 {\"type\":\"rate_limit_event\"}\n").unwrap();
    assert_eq!(fixture.supervisor.replay_spool(number, &spool), None);

    let gaps = fixture.herdr_events(&id, "continuity_gap");
    assert_eq!(
        gaps[0]["reason"],
        "the spool goes on at seq 5 after seq 2: lines 3..=4 are missing"
    );
    let worker = fixture.supervisor.status(&id).unwrap();
    assert!(worker.degraded.unwrap().contains("lines 3..=4 are missing"));
    // Nothing synthesized: the state is what the stored lines made it.
    assert_eq!(worker.state, before.state);
    assert_eq!(worker.session_id, before.session_id);

    let refused = fixture.supervisor.prompt(&id, "finish").unwrap_err();
    assert_eq!(refused.code(), "worker_continuity_gap");
    assert!(
        refused.to_string().contains("lines 3..=4 are missing"),
        "{refused}"
    );
    let refused = fixture.supervisor.begin_takeover(&id, false).err().unwrap();
    assert_eq!(refused.code(), "worker_continuity_gap");

    // The gap is stored: a later server refuses too.
    fixture.supervisor.stop(&id).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let refused = next.begin_takeover(&id, false).err().unwrap();
    assert_eq!(refused.code(), "worker_continuity_gap");
}

#[test]
fn a_broker_gone_without_a_spool_or_with_a_cut_one_is_a_gap() {
    let fixture = Fixture::new("nospool");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let number = worker_number(&id).unwrap();
    assert_eq!(
        fixture
            .supervisor
            .replay_spool(number, &fixture.root.join("missing.spool")),
        None
    );
    assert_eq!(
        fixture.herdr_events(&id, "continuity_gap")[0]["reason"],
        "the worker's broker is gone and left no output spool"
    );

    let fixture = Fixture::new("cutspool");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let number = worker_number(&id).unwrap();
    let spool = fixture.root.join("w.spool");
    std::fs::write(&spool, "o 1 {\"type\":\"rate_limit_event\"}\no 2 {\"ty").unwrap();
    assert_eq!(fixture.supervisor.replay_spool(number, &spool), None);
    assert_eq!(
        fixture.herdr_events(&id, "continuity_gap")[0]["reason"],
        "the spool ends in a record cut short"
    );
    assert_eq!(
        fixture.supervisor.prompt(&id, "finish").unwrap_err().code(),
        "worker_continuity_gap"
    );
}

#[test]
fn lines_written_while_no_server_is_attached_are_replayed_once_after_a_reattach() {
    let fixture = Fixture::with_broker();
    let gate = fixture.root.join("gate.fifo");
    let gate_c = std::ffi::CString::new(gate.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(gate_c.as_ptr(), 0o600) }, 0);
    let id = fixture.start(&format!("gate {}", gate.display()));
    // The init, the rate limit and the tool use are stored.
    fixture.wait_for_status(&id, |status| status.broker_seq >= 3);
    let (_, live, _) = fixture.supervisor.live(&id).unwrap();
    let broker = broker_of(&fixture.supervisor, &id);
    let spool = broker::spool_path(&broker.socket);
    live.sever();

    // With no server attached, the worker ends its turn.
    std::fs::write(&gate, b"go").unwrap();
    // The broker tells this test nothing: watch its spool.
    wait_until("the detached turn's end in the spool", || {
        std::fs::read_to_string(&spool).is_ok_and(|spooled| spooled.contains("\"gated\""))
    });

    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let worker = wait_on(&next, &id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.session_id.as_deref(), Some("stub-session"));
    let journal = fixture.journal(&id);
    let texts: Vec<&str> = journal
        .iter()
        .filter(|record| record["dir"] == "out")
        .filter_map(|record| record["event"]["message"]["content"][0]["text"].as_str())
        .collect();
    assert_eq!(texts, ["line 0", "line 1", "line 2"]);
    let results = journal
        .iter()
        .filter(|record| record["dir"] == "out" && record["event"]["type"] == "result")
        .count();
    assert_eq!(results, 1);
    assert!(fixture.herdr_events(&id, "continuity_gap").is_empty());

    next.stop(&id).unwrap();
    wait_on(&next, &id, WorkerWaitUntil::Exit);
    let stderr: Vec<Value> = fixture
        .journal(&id)
        .into_iter()
        .filter(|record| record["dir"] == "err")
        .map(|record| record["raw"].clone())
        .collect();
    assert_eq!(stderr, [json!("gated stderr")]);
    wait_until("the broker's end", || {
        !crate::platform::process_group_alive(broker.pid)
    });
    // The exit is stored, so the server removes the spool, right after
    // storing it.
    wait_until("the spool's removal", || !spool.exists());
}

#[test]
fn a_line_sent_again_after_a_crash_between_commit_and_ack_is_stored_once() {
    let fixture = Fixture::new("spooled");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let number = worker_number(&id).unwrap();
    let text = |n: u32| {
        json!({"type": "assistant", "message": {"content": [
        {"type": "text", "text": format!("spooled {n}")}]}})
    };
    let spooled = |seq: u64, event: &Value| {
        fixture.supervisor.record_spooled(
            number,
            seq,
            Direction::Out,
            store::Recorded::Event(event),
        )
    };
    assert_eq!(spooled(7, &text(7)), Spooled::Stored);
    // The server committed 7 and died before its ack: the broker sends 7
    // again to the next one.
    assert_eq!(spooled(7, &text(7)), Spooled::Duplicate);
    assert!(Spooled::Duplicate.is_stored());
    let store = store::Store::open(&fixture.root.join("workers").join(store::STORE_FILE)).unwrap();
    assert_eq!(store.load(&id).unwrap().unwrap().broker_seq, 7);

    // A broker gone with its spool: what the store holds is skipped, the
    // rest recorded once, and the exit found.
    let spool = fixture.root.join("w.spool");
    std::fs::write(
        &spool,
        format!(
            "o 7 {}\no 8 {}\ne 9 warning\nl 10 2\nx 11 {{\"type\":\"exited\",\"code\":0}}\n",
            text(7),
            text(8)
        ),
    )
    .unwrap();
    let exit = fixture.supervisor.replay_spool(number, &spool);
    assert_eq!(exit, Some((11, json!({"type": "exited", "code": 0}))));
    // Replayed twice (a second crash): nothing more.
    fixture.supervisor.replay_spool(number, &spool);
    assert_eq!(store.load(&id).unwrap().unwrap().broker_seq, 10);
    let journal = fixture.journal(&id);
    let texts: Vec<&str> = journal
        .iter()
        .filter_map(|record| record["event"]["message"]["content"][0]["text"].as_str())
        .collect();
    assert_eq!(texts, ["spooled 7", "spooled 8"]);
    assert_eq!(fixture.herdr_events(&id, "output_lost")[0]["lines"], 2);
    let warnings = journal
        .iter()
        .filter(|record| record["dir"] == "err" && record["raw"] == "warning")
        .count();
    assert_eq!(warnings, 1);
    // The spool continued the store's seq: continuity is proven.
    assert!(fixture.herdr_events(&id, "continuity_gap").is_empty());
    assert_eq!(fixture.supervisor.status(&id).unwrap().degraded, None);

    // A new server reads the seq from the store and skips what it holds.
    fixture.supervisor.stop(&id).unwrap();
    fixture.wait(&id, WorkerWaitUntil::Exit);
    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(
        next.record_spooled(number, 10, Direction::Out, store::Recorded::Raw("again")),
        Spooled::Duplicate
    );
}

#[test]
fn killing_the_broker_ends_its_worker() {
    let fixture = Fixture::with_broker();
    // From then on the stub ignores SIGTERM and its closed input: only
    // SIGKILL, the broker's guard's, ends it.
    let id = fixture.start("ignore-term");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let pid = worker.pid.unwrap();
    let broker = broker_of(&fixture.supervisor, &id);

    assert!(crate::platform::signal_process_group(broker.pid, Signal::Kill).unwrap());
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.exit_code, None);
    let exited = fixture.herdr_events(&id, "exited");
    assert_eq!(
        exited[0]["error"],
        "the worker's broker ended before the worker's exit"
    );
    wait_until("the worker's end", || {
        !crate::platform::process_group_alive(pid)
    });
}

#[test]
fn a_server_finds_a_broker_gone_and_marks_its_worker_lost() {
    let fixture = Fixture::with_broker();
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let (_, live, status) = fixture.supervisor.live(&id).unwrap();
    let pid = status.pid.unwrap();
    let broker = broker_of(&fixture.supervisor, &id);
    live.sever();
    assert!(crate::platform::signal_process_group(broker.pid, Signal::Kill).unwrap());
    wait_until("the worker's end", || {
        !crate::platform::process_group_alive(pid)
    });

    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    let worker = wait_on(&next, &id, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(fixture.herdr_events(&id, "lost").len(), 1);
}

/// A FIFO at `path`, which a stub's `gate` command waits on.
fn fifo(path: &Path) {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

/// Waits until a server re-attached to the worker.
fn wait_reattached(supervisor: &WorkerSupervisor, worker_id: &str) {
    let number = worker_number(worker_id).unwrap();
    let started = Instant::now();
    let mut registry = lock(&supervisor.shared.registry);
    while registry.workers[&number].live.is_none() {
        assert!(started.elapsed() < HANG_GUARD, "the re-attach hung");
        registry = supervisor
            .shared
            .changed
            .wait_timeout(registry, Duration::from_millis(100))
            .unwrap()
            .0;
    }
}

/// The answer the stub's `perm WebFetch https://example.com` gets when the
/// user allows it.
fn allowed_fetch() -> Value {
    json!({"behavior": "allow", "updatedInput": {
        "file_path": "https://example.com", "command": "https://example.com"}})
}

/// The `answer_intent` a server stores before it writes the answer.
fn intent(response: &Value) -> Value {
    json!({"type": "answer_intent", "request_id": "perm-1", "tool_name": "WebFetch",
           "decision": "allow", "answers": [], "by": "user", "response": response})
}

/// The `input_written` events of the line with `id`, as `again` flags.
fn written(fixture: &Fixture, worker_id: &str, id: &str) -> Vec<bool> {
    fixture
        .herdr_events(worker_id, "input_written")
        .into_iter()
        .filter(|event| event["id"] == id)
        .map(|event| event["again"].as_bool().unwrap())
        .collect()
}

#[test]
fn every_stdin_line_goes_through_the_broker_once_with_its_id() {
    let fixture = Fixture::with_broker();
    let id = fixture.start("finish");
    let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    let first = worker.turn_seq.unwrap();
    fixture
        .supervisor
        .prompt_command(&WorkerPromptParams {
            worker_id: id.clone(),
            text: "finish".into(),
            command_id: Some("cmd 1".into()),
        })
        .unwrap();
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    // The start's prompt goes by its seq, a command by its receipt.
    wait_until("both writes recorded", || {
        written(&fixture, &id, "c:cmd 1") == [false]
            && written(&fixture, &id, &format!("s:{first}")) == [false]
    });
    assert!(fixture.supervisor.status(&id).unwrap().survives_handoff);
}

#[test]
fn an_answer_written_before_a_crash_is_not_written_again() {
    let fixture = Fixture::with_broker();
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    let number = worker_number(&id).unwrap();
    let (_, live, _) = fixture.supervisor.live(&id).unwrap();
    // The gone server stored the answer and wrote it, and died before it
    // recorded it sent: the worker took it and ended its turn.
    let response = allowed_fetch();
    fixture
        .supervisor
        .record(number, Direction::Herdr, &intent(&response));
    live.resend(&control_response("perm-1", response), "r:perm-1")
        .unwrap();
    fixture.wait_for_status(&id, |status| status.state == WorkerState::Finished);
    live.sever();

    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    wait_reattached(&next, &id);
    // Sent again by the new server, the broker only acknowledges it.
    wait_on_status(&next, &id, |status| status.questions.is_empty());
    wait_until("the second acknowledgement", || {
        written(&fixture, &id, "r:perm-1").len() == 2
    });
    assert_eq!(written(&fixture, &id, "r:perm-1"), [false, true]);
    let worker = next.status(&id).unwrap();
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(worker.settled_questions[0].how, "answered");
    assert_eq!(worker.degraded, None);
    assert_eq!(fixture.herdr_events(&id, "answer_sent")[0]["resent"], true);

    next.stop(&id).unwrap();
    wait_on(&next, &id, WorkerWaitUntil::Exit);
}

#[test]
fn an_answer_not_written_before_a_crash_is_written_once_after_the_reattach() {
    let fixture = Fixture::with_broker();
    let id = fixture.start("perm WebFetch https://example.com");
    fixture.wait_for_question(&id);
    let number = worker_number(&id).unwrap();
    let (_, live, _) = fixture.supervisor.live(&id).unwrap();
    // The gone server stored the answer and died before it wrote it.
    fixture
        .supervisor
        .record(number, Direction::Herdr, &intent(&allowed_fetch()));
    live.sever();

    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    // The turn's end and the answer's `answer_sent` come in either order.
    wait_on_status(&next, &id, |status| {
        status.turn_ended() && status.questions.is_empty()
    });
    let worker = next.status(&id).unwrap();
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(worker.settled_questions[0].how, "answered");
    assert_eq!(worker.degraded, None);
    wait_until("the write recorded", || {
        !written(&fixture, &id, "r:perm-1").is_empty()
    });
    assert_eq!(written(&fixture, &id, "r:perm-1"), [false]);

    next.stop(&id).unwrap();
    wait_on(&next, &id, WorkerWaitUntil::Exit);
}

#[test]
fn a_permission_request_stored_before_a_crash_is_asked_by_the_next_server() {
    let fixture = Fixture::with_broker();
    let gate = fixture.root.join("gate.fifo");
    fifo(&gate);
    let id = fixture.start(&format!(
        "gate-perm {} WebFetch https://example.com",
        gate.display()
    ));
    // The init, the prompt's write and the rate limit are stored.
    fixture.wait_for_status(&id, |status| status.broker_seq >= 3);
    let number = worker_number(&id).unwrap();
    let (_, live, _) = fixture.supervisor.live(&id).unwrap();
    let spool = broker::spool_path(&broker_of(&fixture.supervisor, &id).socket);
    live.sever();
    std::fs::write(&gate, b"go").unwrap();
    wait_until("the request in the spool", || {
        std::fs::read_to_string(&spool).is_ok_and(|spooled| spooled.contains("can_use_tool"))
    });
    // The gone server stored the request and died before it asked it.
    let (seq, request) = std::fs::read_to_string(&spool)
        .unwrap()
        .lines()
        .find_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let (tag, seq, payload) = (parts.next()?, parts.next()?, parts.next()?);
            (tag == "o" && payload.contains("can_use_tool")).then(|| {
                (
                    seq.parse::<u64>().unwrap(),
                    serde_json::from_str::<Value>(payload).unwrap(),
                )
            })
        })
        .unwrap();
    assert_eq!(
        fixture.supervisor.record_spooled(
            number,
            seq,
            Direction::Out,
            store::Recorded::Event(&request)
        ),
        Spooled::Stored
    );

    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    wait_on_status(&next, &id, |status| !status.questions.is_empty());
    next.answer(&WorkerAnswerParams {
        worker_id: id.clone(),
        request_id: Some("perm-1".into()),
        decision: Some(WorkerDecision::Allow),
        answers: Vec::new(),
        message: None,
        command_id: None,
    })
    .unwrap();
    let worker = wait_on(&next, &id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("allow"));
    assert_eq!(fixture.herdr_events(&id, "question").len(), 1);
    wait_until("the write recorded", || {
        !written(&fixture, &id, "r:perm-1").is_empty()
    });
    assert_eq!(written(&fixture, &id, "r:perm-1"), [false]);
    assert!(fixture.herdr_events(&id, "continuity_gap").is_empty());

    next.stop(&id).unwrap();
    wait_on(&next, &id, WorkerWaitUntil::Exit);
}

#[test]
fn a_handoff_keeps_a_brokered_worker_mid_turn_and_the_new_server_ends_its_turn() {
    let fixture = Fixture::with_broker();
    let gate = fixture.root.join("gate.fifo");
    fifo(&gate);
    let id = fixture.start(&format!("gate {}", gate.display()));
    fixture.wait_for_status(&id, |status| status.broker_seq >= 3);
    let pid = fixture.supervisor.status(&id).unwrap().pid.unwrap();
    // A worker without a broker still blocks a handoff, a brokered one
    // does not, and the drain does not wait for its turn.
    let plain = Fixture::new("handoff-plain");
    let blocked = plain.start("block");
    plain.wait_for(&blocked, |worker| worker.state == WorkerState::Working);
    assert!(plain.supervisor.prepare_for_handoff(false).is_err());
    assert!(!plain.supervisor.status(&blocked).unwrap().survives_handoff);
    plain.supervisor.stop(&blocked).unwrap();
    assert_eq!(
        fixture.supervisor.prepare_for_handoff(false),
        Ok(Vec::new())
    );
    let drain = fixture.supervisor.drain(WorkerDrainAction::Status, None);
    assert!(drain.in_turn.is_empty());

    // The new server starts while the old one still runs the worker.
    let next = WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
    assert_eq!(fixture.supervisor.detach_for_handoff(), [id.as_str()]);
    wait_reattached(&next, &id);
    // The turn goes on, its output read by the new server.
    std::fs::write(&gate, b"go").unwrap();
    let worker = wait_on(&next, &id, WorkerWaitUntil::TurnEnd);
    assert_eq!(worker.state, WorkerState::Finished);
    assert_eq!(worker.pid, Some(pid));
    assert_eq!(worker.last_result.unwrap().text.as_deref(), Some("gated"));
    assert!(fixture.herdr_events(&id, "continuity_gap").is_empty());
    assert!(fixture.herdr_events(&id, "lost").is_empty());
    assert!(fixture.herdr_events(&id, "exited").is_empty());
    next.prompt(&id, "finish").unwrap();
    assert_eq!(wait_on(&next, &id, WorkerWaitUntil::TurnEnd).turns, 2);

    next.stop(&id).unwrap();
    wait_on(&next, &id, WorkerWaitUntil::Exit);
    plain.wait(&blocked, WorkerWaitUntil::Exit);
}

mod todo_runs {
    use super::*;
    use crate::api::schema::{
        HistoryEventKind, TodoAction, TodoEventKind, TodoLandingSource, TodoResumeParams,
        TodoRunEvent, TodoRunInfo, TodoRunParams, TodoRunStatus, TodoStep, TodoWaitParams,
        WorkerCheckOutcome, WorkerVerdict,
    };

    const ITEM: &str = "t-abcd2345";
    const SUBJECT: &str = "feat: add a";

    /// A repository on `master` with the item in `TODO.md` and the checks
    /// `ok` (an argv a shell would break: `$X;false` expanded and split),
    /// `b` (b.txt exists), `never`, `tests` (src/api/a.txt exists), which
    /// the verify adds to a diff under `src/api/`, and `maintenance`
    /// (docs/next/a.txt exists), which it adds to a diff under `docs/next/`.
    fn todo_repo(name: &str) -> Fixture {
        let fixture = Fixture::new(name);
        let repo = &fixture.repo;
        git_in(repo, &["init", "-q", "-b", "master"]);
        for (key, value) in [
            ("user.name", "t"),
            ("user.email", "t@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git_in(repo, &["config", key, value]);
        }
        std::fs::write(
            repo.join("TODO.md"),
            format!("# TODO\n\n- [ ] The driven item [{ITEM}]\n"),
        )
        .unwrap();
        std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
        std::fs::create_dir_all(repo.join(".herdr")).unwrap();
        std::fs::write(
            repo.join(".herdr/checks.toml"),
            "[checks]\n\
             ok = [\"test\", \"$X;false\", \"=\", \"$X;false\"]\n\
             b = [\"test\", \"-f\", \"b.txt\"]\n\
             never = [\"false\"]\n\
             tests = [\"test\", \"-f\", \"src/api/a.txt\"]\n\
             maintenance = [\"test\", \"-f\", \"docs/next/a.txt\"]\n\
             [preflight]\nmin_free_gib = 0\n",
        )
        .unwrap();
        git_in(repo, &["add", "."]);
        git_in(repo, &["commit", "-q", "-m", "init"]);
        fixture
    }

    fn params(fixture: &Fixture, task: &str, check: &str) -> TodoRunParams {
        TodoRunParams {
            cwd: fixture.repo.display().to_string(),
            item: ITEM.into(),
            task: task.into(),
            message: SUBJECT.into(),
            paths: vec!["*.txt".into()],
            checks: check.split(' ').map(str::to_owned).collect(),
            owner_pane_id: Some("p-coordinator".into()),
            owner_session_id: None,
            workspace_id: Some("ws-coordinator".into()),
            env: Some(caller_env()),
            ignore_usage: false,
            auto_review: false,
            auto_answer: false,
        }
    }

    /// The environment a coordinator's CLI sends: this process's, without
    /// `HERDR_*`.
    fn caller_env() -> std::collections::HashMap<String, String> {
        std::env::vars()
            .filter(|(key, _)| !key.starts_with("HERDR_"))
            .collect()
    }

    /// The run's next event; for a run that waits or ended, only once its
    /// driver let go, so the test's next resume or planned crash meets
    /// one driver. A `still_alive` event is raised beside the driver, which
    /// goes on waiting for the worker's exit.
    fn wait(fixture: &Fixture, run_id: &str, after: Option<i64>) -> (TodoRunEvent, TodoRunInfo) {
        let (event, run) = next_event(fixture, run_id, after);
        if run.status != TodoRunStatus::Running && event.kind != TodoEventKind::StillAlive {
            runs::wait_undriven(run_id, HANG_GUARD);
        }
        (event, run)
    }

    /// The run's next event as `todo.wait` returns it, without waiting for
    /// its driver to let go (one waiting for the store to take a write
    /// holds on to the run).
    fn next_event(
        fixture: &Fixture,
        run_id: &str,
        after: Option<i64>,
    ) -> (TodoRunEvent, TodoRunInfo) {
        let started = Instant::now();
        let (event, run) = fixture
            .supervisor
            .todo_wait(
                &TodoWaitParams {
                    run_id: run_id.to_owned(),
                    after,
                },
                Duration::from_millis(100),
                || {
                    assert!(started.elapsed() < HANG_GUARD, "run {run_id} hung");
                    true
                },
            )
            .unwrap()
            .unwrap();
        (event, run)
    }

    fn resume(
        fixture: &Fixture,
        run_id: &str,
        event: i64,
        action: TodoAction,
        task: Option<&str>,
    ) -> Result<TodoRunInfo, WorkerError> {
        fixture.supervisor.todo_resume(TodoResumeParams {
            run_id: run_id.to_owned(),
            action,
            event,
            task: task.map(str::to_owned),
            request_id: None,
            decision: None,
            answers: Vec::new(),
            message: None,
            env: Some(caller_env()),
            ignore_usage: false,
            keep_open: false,
            note: None,
            close: None,
            next: None,
            stop_reason: None,
            caller_pane_id: Some("p-coordinator".into()),
            caller_session_id: None,
            caller_workspace_id: Some("ws-coordinator".into()),
        })
    }

    fn master_subjects(fixture: &Fixture) -> Vec<String> {
        git_in(&fixture.repo, &["log", "--format=%s", "master"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn approve_to_done(fixture: &Fixture, run_id: &str) -> (TodoRunEvent, TodoRunInfo) {
        let (review, _) = wait(fixture, run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        resume(fixture, run_id, review.event_id, TodoAction::Approve, None).unwrap();
        wait(fixture, run_id, Some(review.event_id))
    }

    /// Aborts the run at `event` with the reason `superseded` and waits for
    /// it to end: `aborted`, final, its event taking nothing more.
    fn abort(fixture: &Fixture, run_id: &str, event: i64) -> (TodoRunEvent, TodoRunInfo) {
        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                message: Some("superseded".into()),
                ..resume_params(run_id, event, TodoAction::Abort)
            })
            .unwrap();
        let (aborted, ended) = wait(fixture, run_id, Some(event));
        assert_eq!(aborted.kind, TodoEventKind::Aborted, "{aborted:#?}");
        assert!(aborted.actions.is_empty(), "{aborted:#?}");
        assert_eq!(
            (ended.status, ended.step, ended.pending_event),
            (TodoRunStatus::Aborted, TodoStep::Abort, None)
        );
        assert_eq!(ended.error, aborted.error);
        // A second abort of the same event is stale: the run has ended.
        let again = resume(fixture, run_id, event, TodoAction::Abort, None).unwrap_err();
        assert_eq!(again.code(), "todo_event_stale", "{again}");
        // An ended run returns its last event to every wait.
        assert_eq!(
            wait(fixture, run_id, Some(aborted.event_id)).0.event_id,
            aborted.event_id
        );
        assert!(!fixture.supervisor.run_lock_path(run_id).exists());
        (aborted, ended)
    }

    /// Claude's usage as the provider answers it: one window per
    /// `(id, used)`.
    fn claude_usage(windows: &[(&str, u8)]) -> crate::api::schema::ProviderUsage {
        let mut usage = runs::low_usage_for_test();
        usage.windows = windows
            .iter()
            .map(|(id, used)| crate::api::schema::UsageWindow {
                id: (*id).into(),
                label: (*id).into(),
                used_percent: *used,
                resets_at: None,
                observed_at: None,
                freshness: None,
                error_kind: None,
            })
            .collect();
        usage
    }

    fn fresh_usage(five_hour: u8, weekly: u8) -> Result<crate::api::schema::ProviderUsage, String> {
        Ok(claude_usage(&[
            ("five_hour", five_hour),
            ("seven_day", weekly),
        ]))
    }

    /// A run claimed outside a pane while a headless item coordinator
    /// (`todo.next`) holds the repository's tenure is that tenure's.
    #[test]
    fn a_run_claimed_outside_a_pane_belongs_to_the_headless_coordinator() {
        let fixture = todo_repo("todo-headless-owner");
        let repo = repository_of(&fixture.repo).unwrap();
        store_of(&fixture.supervisor)
            .transaction(|tx| {
                let tenure = store::NewTenure {
                    id: "c-headless",
                    repo: &repo,
                    pane_id: None,
                    session_id: None,
                };
                tx.coordinator_started(&tenure, 1)
            })
            .unwrap();
        let mut claimed = params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok");
        claimed.owner_pane_id = None;
        claimed.workspace_id = None;
        let run = fixture.supervisor.todo_run(claimed).unwrap();
        let stored = fixture.supervisor.load_run(&run.run_id).unwrap();
        assert_eq!(stored.owner_coordinator.as_deref(), Some("c-headless"));
        assert_eq!(stored.owner_pane, None);
        let history = fixture.supervisor.history_item(ITEM, Some(&repo)).unwrap();
        assert_eq!(
            history.events[0].coordinator_id.as_deref(),
            Some("c-headless")
        );
        let (review, _) = wait(&fixture, &run.run_id, None);
        abort(&fixture, &run.run_id, review.event_id);
    }

    fn usage_refusal(fixture: &Fixture) -> String {
        let refused = fixture
            .supervisor
            .todo_run(params(fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap_err();
        assert_eq!(refused.code(), "usage_gate", "{refused}");
        refused.to_string()
    }

    #[test]
    fn the_usage_gate_refuses_at_90_and_reopens_only_below_80_across_a_restart() {
        let fixture = todo_repo("todo-usage-gate");
        let repo = repository_of(&fixture.repo).unwrap();
        fixture.supervisor.set_usage_for_test(fresh_usage(40, 91));
        let message = usage_refusal(&fixture);
        for part in ["seven_day", "91% used"] {
            assert!(message.contains(part), "{part:?} not in {message}");
        }
        // A refusal starts nothing.
        assert!(fixture
            .supervisor
            .todo_runs(None, None)
            .unwrap()
            .0
            .is_empty());
        assert!(fixture.supervisor.list().is_empty());

        // Closed: 89 is not enough to reopen, after a restart too.
        fixture.supervisor.set_usage_for_test(fresh_usage(89, 10));
        assert!(usage_refusal(&fixture).contains("below 80%"));
        let restarted =
            WorkerSupervisor::open(fixture.root.join("workers"), PathBuf::from("unused"));
        restarted.set_usage_for_test(fresh_usage(80, 10));
        assert_eq!(
            restarted.usage_gate(&repo, false).unwrap_err().code(),
            "usage_gate"
        );
        restarted.set_usage_for_test(fresh_usage(79, 79));
        let admitted = restarted.usage_gate(&repo, false).unwrap();
        assert_eq!(admitted["decision"], "admit");
        assert_eq!(admitted["reopened"], true);
        // Open again: 89 admits, 90 closes it.
        restarted.set_usage_for_test(fresh_usage(89, 89));
        assert_eq!(
            restarted.usage_gate(&repo, false).unwrap()["reopened"],
            false
        );
        restarted.set_usage_for_test(fresh_usage(90, 0));
        assert!(restarted.usage_gate(&repo, false).is_err());
        restarted.set_usage_for_test(fresh_usage(85, 0));
        assert!(restarted.usage_gate(&repo, false).is_err());
    }

    #[test]
    fn a_failed_read_or_an_answer_without_windows_refuses_a_run() {
        let fixture = todo_repo("todo-usage-unknown");
        for (answer, expected) in [
            (
                Err("Claude login expired; run Claude Code to renew it".to_owned()),
                "Claude login expired",
            ),
            (Ok(claude_usage(&[])), "no window"),
        ] {
            fixture.supervisor.set_usage_for_test(answer);
            let message = usage_refusal(&fixture);
            assert!(message.contains(expected), "{expected:?} not in {message}");
        }
        assert!(fixture
            .supervisor
            .todo_runs(None, None)
            .unwrap()
            .0
            .is_empty());
    }

    #[test]
    fn ignore_usage_starts_a_run_and_a_retry_and_is_recorded() {
        let fixture = todo_repo("todo-usage-ignored");
        fixture.supervisor.set_usage_for_test(fresh_usage(95, 10));
        let run = fixture
            .supervisor
            .todo_run(TodoRunParams {
                ignore_usage: true,
                ..params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok")
            })
            .unwrap();
        let created = run_events(&fixture, &run.run_id, "run_created");
        assert_eq!(created[0]["ignore_usage"], true, "{created:#?}");
        assert_eq!(created[0]["usage_gate"]["decision"], "ignored");
        assert_eq!(created[0]["usage_gate"]["windows"][0]["used_percent"], 95);

        let (review, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        // A retry starts a new attempt: the gate refuses it, and the run
        // keeps waiting on the same event.
        let refused = resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Retry,
            Some(&format!("commit b.txt {SUBJECT}")),
        )
        .unwrap_err();
        assert_eq!(refused.code(), "usage_gate", "{refused}");
        let waiting = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(
            (waiting.status, waiting.pending_event, waiting.attempt),
            (TodoRunStatus::Waiting, Some(review.event_id), 1)
        );
        let retried = fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                task: Some(format!("commit b.txt {SUBJECT}")),
                ignore_usage: true,
                ..resume_params(&run.run_id, review.event_id, TodoAction::Retry)
            })
            .unwrap();
        assert_eq!(retried.step, TodoStep::Restart);
        let resumed = run_events(&fixture, &run.run_id, "run_resumed");
        let last = resumed.last().unwrap();
        assert_eq!(last["ignore_usage"], true, "{resumed:#?}");
        assert_eq!(last["usage_gate"]["decision"], "ignored");
        let (review, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        abort(&fixture, &run.run_id, review.event_id);
    }

    #[test]
    fn a_run_goes_from_preflight_to_the_cherry_pick() {
        let fixture = todo_repo("todo-full");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        assert!(run.run_id.starts_with("r-"), "{run:?}");
        assert_eq!((run.attempt, run.step), (1, TodoStep::Start));
        assert_eq!(
            run.branch.as_deref(),
            Some(format!("todo/t-abcd2345-{}-1", run.run_id).as_str())
        );

        let (review, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review);
        assert_eq!(
            review.actions,
            [TodoAction::Approve, TodoAction::Retry, TodoAction::Abort]
        );
        assert_eq!(review.commits.len(), 1, "{review:#?}");
        assert!(
            review
                .diff_stat
                .as_deref()
                .unwrap_or_default()
                .contains("a.txt"),
            "{review:#?}"
        );
        assert!(
            review
                .result_text
                .as_deref()
                .unwrap_or_default()
                .contains("WORKER-DONE"),
            "{review:#?}"
        );
        assert_eq!(
            (waiting.status, waiting.step, waiting.pending_event),
            (
                TodoRunStatus::Waiting,
                TodoStep::Review,
                Some(review.event_id)
            )
        );
        // The worker runs with the run's item, in the slot, on its branch.
        let worker_id = waiting.worker_id.clone().unwrap();
        let worker = fixture.supervisor.status(&worker_id).unwrap();
        assert_eq!(worker.item.as_deref(), Some(ITEM));
        assert!(
            worker.cwd.ends_with("herdr-worktrees/worker"),
            "{}",
            worker.cwd
        );
        // Its task is the coordinator's text with the run's contract: the
        // exact subject without body or trailers, the paths, the last line.
        let task = fixture
            .journal(&worker_id)
            .into_iter()
            .find(|record| record["dir"] == "in" && record["event"]["type"] == "user")
            .and_then(|record| {
                record["event"]["message"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap();
        assert!(
            task.starts_with(&format!("commit a.txt {SUBJECT}\n")),
            "{task}"
        );
        for part in [
            &format!("\n{SUBJECT}\n"),
            "no body and no trailers",
            "*.txt",
            "`WORKER-DONE <sha> | <summary>`",
            "`WORKER-BLOCKED <reason>`",
            // The checks the verify runs, by name and argv, and the one it
            // adds for an API change.
            "- `ok`: `[\"test\",\"$X;false\",\"=\",\"$X;false\"]`\n",
            "- `tests`: `[\"test\",\"-f\",\"src/api/a.txt\"]`, added by the verify when \
             your diff touches `src/api/` or `tests/fixtures/`",
            "- `maintenance`: `[\"test\",\"-f\",\"docs/next/a.txt\"]`, added by the verify \
             when your diff touches `src/config/` or `docs/next/` or `scripts/` or `plugins/`",
            "run every one of them you can in your sandbox (`windows-lint` works there) and \
             report each one's result, or the sandbox error that stopped it",
            "Advertised client methods keep their v1 shape: add a new method instead of \
             changing one (AGENTS.md, Stable client endpoint contract).\n",
        ] {
            assert!(task.contains(part), "{part:?} missing from {task}");
        }
        // Until the coordinator resumes the run, its pane owes the review.
        let owed = fixture.supervisor.obligations(Some("p-coordinator"));
        assert_eq!(owed.len(), 1, "{owed:#?}");
        assert_eq!(owed[0].worker_id, worker_id);
        assert_eq!(owed[0].reason, WorkerAttentionReason::TurnEnd);

        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        assert!(done.actions.is_empty());
        assert_eq!(
            (finished.status, finished.step),
            (TodoRunStatus::Done, TodoStep::Done)
        );
        let master = git_in(&fixture.repo, &["rev-parse", "master"]);
        assert_eq!(finished.picked.as_deref(), Some(master.trim()));
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        assert!(fixture.repo.join("a.txt").is_file());
        // The worker was stopped, by its owner: no obligation is left.
        assert_eq!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );
        assert!(fixture
            .supervisor
            .obligations(Some("p-coordinator"))
            .is_empty());
        // The verify ran the check as its argv: a shell would have failed it.
        let verification = &fixture.herdr_events(&worker_id, "verification")[0];
        assert_eq!(verification["verification"]["verdict"], "verified");

        // Every step is an event of the run, intent before result.
        let types: Vec<String> = fixture
            .supervisor
            .shared
            .store
            .as_ref()
            .unwrap()
            .run_event_types(&run.run_id)
            .unwrap()
            .into_iter()
            .map(|(_, kind)| kind)
            .collect();
        assert_eq!(
            types,
            [
                "run_created",
                "run_start_intent",
                "run_started",
                "run_event",
                "run_resumed",
                "run_stop_intent",
                "run_stopped",
                "run_verify_intent",
                "run_verified",
                "run_cherry_pick_intent",
                "run_picked",
                "run_install_skipped",
                "run_todo_skipped",
                "run_push_skipped",
                "run_cleaned",
                "run_event",
            ]
        );
        // A diff outside `src/api/` verifies with the named checks only.
        assert_eq!(finished.checks, ["ok"]);
        // The slot still has the run's branch checked out: it is kept for
        // a later run's cleanup.
        assert_eq!(
            finished.kept_branches,
            [format!("todo/t-abcd2345-{}-1", run.run_id)]
        );
        // A done run returns its last event to every wait.
        let (again, _) = wait(&fixture, &run.run_id, Some(done.event_id));
        assert_eq!(again.event_id, done.event_id);
        assert_eq!(
            fixture.supervisor.todo_runs(None, None).unwrap().0[0].run_id,
            run.run_id
        );
    }

    #[test]
    fn a_diff_under_src_api_adds_the_tests_check_to_the_verify() {
        let fixture = todo_repo("todo-contract-check");
        let api = fixture.repo.join("src/api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("keep.txt"), "").unwrap();
        git_in(&fixture.repo, &["add", "."]);
        git_in(&fixture.repo, &["commit", "-q", "-m", "api"]);
        let run = fixture
            .supervisor
            .todo_run(TodoRunParams {
                paths: vec!["src/api/*.txt".into()],
                ..params(&fixture, &format!("commit src/api/a.txt {SUBJECT}"), "ok")
            })
            .unwrap();
        assert_eq!(run.checks, ["ok"]);
        let (done, finished) = approve_to_done(&fixture, &run.run_id);
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        assert_eq!(finished.checks, ["ok", "tests"]);
        let added = run_events(&fixture, &run.run_id, "run_check_added");
        assert_eq!(added.len(), 1, "{added:#?}");
        assert_eq!(added[0]["check"]["name"], "tests");
        assert_eq!(
            added[0]["check"]["argv"],
            serde_json::json!(["test", "-f", "src/api/a.txt"])
        );
        assert!(
            added[0]["reason"].as_str().unwrap().contains("src/api/"),
            "{added:#?}"
        );
        // The verify ran it, after the named check.
        let worker_id = finished.worker_id.unwrap();
        let verification = &fixture.herdr_events(&worker_id, "verification")[0];
        let checks: Vec<&str> = verification["verification"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|check| check["name"].as_str())
            .collect();
        assert!(checks.ends_with(&["ok", "tests"]), "{verification:#?}");
    }

    #[test]
    fn a_diff_under_docs_next_adds_the_maintenance_check_to_the_verify() {
        let fixture = todo_repo("todo-maintenance-check");
        let docs = fixture.repo.join("docs/next");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("keep.txt"), "").unwrap();
        git_in(&fixture.repo, &["add", "."]);
        git_in(&fixture.repo, &["commit", "-q", "-m", "docs"]);
        let run = fixture
            .supervisor
            .todo_run(TodoRunParams {
                paths: vec!["docs/next/*.txt".into()],
                ..params(&fixture, &format!("commit docs/next/a.txt {SUBJECT}"), "ok")
            })
            .unwrap();
        assert_eq!(run.checks, ["ok"]);
        let (done, finished) = approve_to_done(&fixture, &run.run_id);
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        // Only the check whose paths the diff touches is added.
        assert_eq!(finished.checks, ["ok", "maintenance"]);
        let added = run_events(&fixture, &run.run_id, "run_check_added");
        assert_eq!(added.len(), 1, "{added:#?}");
        assert_eq!(added[0]["check"]["name"], "maintenance");
        assert_eq!(
            added[0]["check"]["argv"],
            serde_json::json!(["test", "-f", "docs/next/a.txt"])
        );
        assert!(
            added[0]["reason"].as_str().unwrap().contains("docs/next/"),
            "{added:#?}"
        );
        let worker_id = finished.worker_id.unwrap();
        let verification = &fixture.herdr_events(&worker_id, "verification")[0];
        let checks: Vec<&str> = verification["verification"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|check| check["name"].as_str())
            .collect();
        assert!(
            checks.ends_with(&["ok", "maintenance"]),
            "{verification:#?}"
        );
    }

    #[test]
    fn a_question_becomes_an_event_that_resume_answers_and_a_stale_event_is_refused() {
        let fixture = todo_repo("todo-question");
        let run = fixture
            .supervisor
            .todo_run(params(
                &fixture,
                &format!("perm-commit a.txt {SUBJECT}"),
                "ok",
            ))
            .unwrap();
        let (question, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(question.kind, TodoEventKind::Question, "{question:#?}");
        assert_eq!(question.actions, [TodoAction::Answer, TodoAction::Abort]);
        assert_eq!(question.questions.len(), 1);
        assert_eq!(question.questions[0].tool_name, "WebFetch");
        // Not an action of a question.
        let refused = resume(
            &fixture,
            &run.run_id,
            question.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");

        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                run_id: run.run_id.clone(),
                action: TodoAction::Answer,
                event: question.event_id,
                task: None,
                request_id: Some(question.questions[0].request_id.clone()),
                decision: Some(WorkerDecision::Allow),
                answers: Vec::new(),
                message: None,
                env: Some(caller_env()),
                ignore_usage: false,
                keep_open: false,
                note: None,
                close: None,
                next: None,
                stop_reason: None,
                caller_pane_id: Some("p-coordinator".into()),
                caller_session_id: None,
                caller_workspace_id: Some("ws-coordinator".into()),
            })
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, Some(question.event_id));
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        // The answered question's event is stale now.
        let stale = resume(
            &fixture,
            &run.run_id,
            question.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap_err();
        assert_eq!(stale.code(), "todo_event_stale", "{stale}");
        assert!(
            stale.to_string().contains(&review.event_id.to_string()),
            "{stale}"
        );
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        // A resume of an ended run is stale too.
        let ended = resume(
            &fixture,
            &run.run_id,
            done.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap_err();
        assert_eq!(ended.code(), "todo_event_stale");
        assert_eq!(
            resume(&fixture, "r-nosuchid", 1, TodoAction::Approve, None)
                .unwrap_err()
                .code(),
            "todo_run_not_found"
        );
    }

    #[test]
    fn a_failed_verify_asks_for_a_retry_and_the_retry_passes() {
        let fixture = todo_repo("todo-retry");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "b"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (failed, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(failed.kind, TodoEventKind::VerifyFailed, "{failed:#?}");
        assert_eq!(
            failed.actions,
            [TodoAction::Retry, TodoAction::Verify, TodoAction::Abort]
        );
        let verification = failed.verification.as_ref().unwrap();
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        assert!(verification
            .checks
            .iter()
            .any(|check| check.check == "command" && check.outcome == WorkerCheckOutcome::Failed));
        assert_eq!(waiting.step, TodoStep::Verify);
        // A retry needs its task text.
        let refused = resume(
            &fixture,
            &run.run_id,
            failed.event_id,
            TodoAction::Retry,
            None,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "invalid_request");
        let first_commit = failed.commits[0].clone();

        // The retry's text is the review of attempt 1.
        let review_text = format!("amend b.txt {SUBJECT}");
        let retried = resume(
            &fixture,
            &run.run_id,
            failed.event_id,
            TodoAction::Retry,
            Some(&review_text),
        )
        .unwrap();
        assert_eq!(retried.step, TodoStep::Restart);
        let (review, second) = wait(&fixture, &run.run_id, Some(failed.event_id));
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        // The driver cherry-picked attempt 1's commit onto attempt 2's
        // branch, which the worker amended: one commit with both changes.
        let carried = run_events(&fixture, &run.run_id, "run_carried");
        assert_eq!(carried.len(), 1, "{carried:#?}");
        assert_eq!(carried[0]["from_attempt"], 1);
        assert_eq!(carried[0]["from_commit"], first_commit.as_str());
        assert_eq!(review.commits.len(), 1, "{review:#?}");
        let stat = review.diff_stat.as_deref().unwrap_or_default();
        assert!(stat.contains("a.txt") && stat.contains("b.txt"), "{stat}");
        // Its task is attempt 1's with the review, and says where it starts.
        let task = format!("commit a.txt {SUBJECT}\n\nReview of attempt 1:\n{review_text}");
        assert_eq!(second.task, task);
        let prompt = first_prompt(&fixture, second.worker_id.as_deref().unwrap());
        assert!(prompt.starts_with(&task), "{prompt}");
        assert!(
            prompt.contains(&format!("attempt 1's commit {first_commit}")),
            "{prompt}"
        );
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(finished.attempt, 2);
        assert_eq!(
            finished.branch.as_deref(),
            Some(format!("todo/t-abcd2345-{}-2", run.run_id).as_str())
        );
        assert!(fixture.repo.join("b.txt").is_file());
        assert!(fixture.repo.join("a.txt").is_file());
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);

        // One row per attempt, with the coordinator's decisions.
        let store = fixture.supervisor.shared.store.as_ref().unwrap();
        let attempts = store.attempts(&run.run_id).unwrap();
        assert_eq!(attempts.len(), 2, "{attempts:#?}");
        let (one, two) = (&attempts[0], &attempts[1]);
        assert_eq!((one.number, two.number), (1, 2));
        assert_eq!(one.task, format!("commit a.txt {SUBJECT}"));
        assert_eq!(one.attempt.review_event, Some(failed.event_id));
        assert_eq!(one.attempt.review_decision.as_deref(), Some("retry"));
        assert_eq!(
            one.attempt.review_text.as_deref(),
            Some(review_text.as_str())
        );
        assert_eq!(one.attempt.commit.as_deref(), Some(first_commit.as_str()));
        assert!(one
            .attempt
            .verification
            .as_deref()
            .unwrap_or_default()
            .contains("\"failed\""));
        assert_ne!(one.worker_id, two.worker_id);
        assert_eq!(two.task, task);
        assert_eq!(
            (two.attempt.from_attempt, two.attempt.from_commit.as_deref()),
            (Some(1), Some(first_commit.as_str()))
        );
        assert_eq!(two.attempt.review_decision.as_deref(), Some("approve"));
        assert_eq!(two.attempt.commit, review.commits.last().cloned());
        assert_eq!(two.attempt.base, finished.base);
        assert!(two
            .attempt
            .verification
            .as_deref()
            .unwrap_or_default()
            .contains("\"verified\""));
    }

    #[test]
    fn a_landed_commit_carries_trailers_and_names_its_run() {
        let fixture = todo_repo("todo-landing");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (done, finished) = approve_to_done(&fixture, &run.run_id);
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        let picked = finished.picked.clone().unwrap();
        let repo = fixture.repo.display().to_string();
        // The landed commit carries the trailers; the worker's stays
        // subject-only, as its verify required.
        let message = git_in(&fixture.repo, &["log", "-1", "--format=%B", &picked]);
        assert_eq!(
            message.trim_end(),
            format!(
                "{SUBJECT}\n\nHerdr-Item: {ITEM}\nHerdr-Run: {}/1",
                run.run_id
            )
        );
        let branch = finished.branch.clone().unwrap();
        let worker_commit = git_in(&fixture.repo, &["rev-parse", &branch])
            .trim()
            .to_owned();
        assert_eq!(
            git_in(&fixture.repo, &["log", "-1", "--format=%B", &worker_commit]).trim_end(),
            SUBJECT
        );
        let picked_event = &run_events(&fixture, &run.run_id, "run_picked")[0];
        assert_eq!(picked_event["trailers"], true, "{picked_event:#}");
        assert_eq!(picked_event["worker_commit"], worker_commit.as_str());

        // The landing names the run, by the landed commit or the worker's.
        for commit in [&picked[..10], picked.as_str(), &worker_commit[..12]] {
            let (runs, landing) = fixture
                .supervisor
                .todo_runs(Some(&repo), Some(commit))
                .unwrap();
            assert_eq!(runs.len(), 1, "{commit}: {runs:#?}");
            assert_eq!(runs[0].run_id, run.run_id);
            let landing = landing.unwrap();
            assert_eq!(landing.source, TodoLandingSource::Store);
            assert_eq!(
                (
                    landing.landed_sha.as_str(),
                    landing.attempt,
                    landing.item.as_str(),
                    landing.worker_commit.as_deref(),
                    landing.worker_id.as_deref(),
                ),
                (
                    picked.as_str(),
                    1,
                    ITEM,
                    Some(worker_commit.as_str()),
                    finished.worker_id.as_deref()
                )
            );
        }
        // The upstream rebase changes the sha, not the trailers: the run is
        // found by them.
        git_in(
            &fixture.repo,
            &[
                "commit",
                "--quiet",
                "--amend",
                "--no-edit",
                "--date",
                "2001-01-01T00:00:00",
            ],
        );
        let rebased = git_in(&fixture.repo, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        assert_ne!(rebased, picked);
        let (runs, landing) = fixture
            .supervisor
            .todo_runs(Some(&repo), Some(&rebased))
            .unwrap();
        assert_eq!(runs[0].run_id, run.run_id);
        let landing = landing.unwrap();
        assert_eq!(landing.source, TodoLandingSource::Trailers);
        assert_eq!(
            (
                landing.landed_sha.as_str(),
                landing.run_id.as_str(),
                landing.attempt,
                landing.item.as_str(),
                landing.worker_id.as_deref()
            ),
            (
                rebased.as_str(),
                run.run_id.as_str(),
                1,
                ITEM,
                finished.worker_id.as_deref()
            )
        );
        // Without a repository there are no trailers to read.
        let unknown = fixture
            .supervisor
            .todo_runs(None, Some(&rebased))
            .unwrap_err();
        assert_eq!(unknown.code(), "todo_run_not_found", "{unknown}");
        // A commit no run landed, and a word that is no sha.
        let init = git_in(&fixture.repo, &["rev-parse", "HEAD~1"]);
        let unknown = fixture
            .supervisor
            .todo_runs(Some(&repo), Some(init.trim()))
            .unwrap_err();
        assert_eq!(unknown.code(), "todo_run_not_found", "{unknown}");
        assert!(
            unknown.to_string().contains("no Herdr-Run trailer"),
            "{unknown}"
        );
        let bad = fixture
            .supervisor
            .todo_runs(Some(&repo), Some("master"))
            .unwrap_err();
        assert_eq!(bad.code(), "invalid_request", "{bad}");
    }

    #[test]
    fn an_approval_is_bound_to_the_commit_it_saw() {
        let fixture = todo_repo("todo-bound");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        let seen = review.commits[0].clone();
        // The branch moves after the review: still one commit with the
        // subject, so the verify passes, but not the one approved.
        let slot = PathBuf::from(
            fixture
                .supervisor
                .status(waiting.worker_id.as_deref().unwrap())
                .unwrap()
                .cwd,
        );
        git_in(
            &slot,
            &[
                "commit",
                "--quiet",
                "--amend",
                "--no-edit",
                "--date",
                "2001-01-01T00:00:00",
            ],
        );
        let moved = git_in(&slot, &["rev-parse", "HEAD"]).trim().to_owned();
        assert_ne!(moved, seen);
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (again, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(again.kind, TodoEventKind::Review, "{again:#?}");
        assert_eq!(
            again.actions,
            [TodoAction::Approve, TodoAction::Retry, TodoAction::Abort]
        );
        assert_eq!(again.commits, [moved.as_str()]);
        let why = again.error.as_deref().unwrap_or_default();
        assert!(why.contains(&seen) && why.contains(&moved), "{why}");
        assert_eq!(
            (waiting.status, waiting.step, waiting.picked.as_deref()),
            (TodoRunStatus::Waiting, TodoStep::Review, None)
        );
        assert_eq!(master_subjects(&fixture), ["init"]);
        // Approved again, the commit it now names lands.
        resume(
            &fixture,
            &run.run_id,
            again.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (done, finished) = wait(&fixture, &run.run_id, Some(again.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        let store = fixture.supervisor.shared.store.as_ref().unwrap();
        let attempt = &store.attempts(&run.run_id).unwrap()[0].attempt;
        assert_eq!(
            (
                attempt.commit.as_deref(),
                attempt.review_event,
                attempt.base.as_deref()
            ),
            (
                Some(moved.as_str()),
                Some(again.event_id),
                finished.base.as_deref()
            )
        );
        let landing = store
            .landing_of(finished.picked.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            landing.unwrap().worker_commit.as_deref(),
            Some(moved.as_str())
        );
    }

    #[test]
    fn a_retry_whose_commit_does_not_pick_asks_and_may_start_from_the_base() {
        let fixture = todo_repo("todo-retry-conflict");
        // Two commits, the second changing what the first added: the tip
        // alone does not pick onto the base.
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit2 a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.commits.len(), 2, "{review:#?}");
        let tip = review.commits[1].clone();
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Retry,
            Some(&format!("commit c.txt {SUBJECT}")),
        )
        .unwrap();
        let (conflict, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(conflict.kind, TodoEventKind::RetryConflict, "{conflict:#?}");
        assert_eq!(conflict.actions, [TodoAction::Retry, TodoAction::Abort]);
        assert_eq!(conflict.commits, [tip.as_str()]);
        let why = conflict.error.as_deref().unwrap_or_default();
        assert!(why.contains("a.txt") && why.contains(&tip), "{why}");
        assert_eq!(
            (
                waiting.status,
                waiting.step,
                waiting.attempt,
                waiting.worker_id
            ),
            (TodoRunStatus::Waiting, TodoStep::Start, 2, None)
        );
        // The retry keeps its review: no new task text.
        let refused = resume(
            &fixture,
            &run.run_id,
            conflict.event_id,
            TodoAction::Retry,
            Some("other"),
        )
        .unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");
        resume(
            &fixture,
            &run.run_id,
            conflict.event_id,
            TodoAction::Retry,
            None,
        )
        .unwrap();
        let (second, waiting) = wait(&fixture, &run.run_id, Some(conflict.event_id));
        assert_eq!(second.kind, TodoEventKind::Review, "{second:#?}");
        assert_eq!(second.commits.len(), 1, "{second:#?}");
        let prompt = first_prompt(&fixture, waiting.worker_id.as_deref().unwrap());
        assert!(prompt.contains("starts from the base"), "{prompt}");
        assert!(run_events(&fixture, &run.run_id, "run_carried").is_empty());
        let store = fixture.supervisor.shared.store.as_ref().unwrap();
        let attempts = store.attempts(&run.run_id).unwrap();
        assert_eq!(attempts[0].attempt.commit.as_deref(), Some(tip.as_str()));
        assert_eq!(
            (
                attempts[1].attempt.from_attempt,
                attempts[1].attempt.from_commit.as_deref()
            ),
            (Some(1), None)
        );
        resume(
            &fixture,
            &run.run_id,
            second.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(second.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert!(fixture.repo.join("c.txt").is_file());
        assert!(!fixture.repo.join("a.txt").exists());
    }

    /// The first user message a worker got: its task.
    fn first_prompt(fixture: &Fixture, worker_id: &str) -> String {
        fixture
            .journal(worker_id)
            .into_iter()
            .find(|record| record["dir"] == "in" && record["event"]["type"] == "user")
            .and_then(|record| {
                record["event"]["message"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap()
    }

    #[test]
    fn attempts_are_capped_then_the_run_is_blocked() {
        let fixture = todo_repo("todo-capped");
        let task = format!("commit a.txt {SUBJECT}");
        // Each later attempt amends the commit it starts from.
        let review_text = format!("amend a.txt {SUBJECT}");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &task, "never"))
            .unwrap();
        let mut after = None;
        for attempt in 1..=3 {
            let (review, _) = wait(&fixture, &run.run_id, after);
            assert_eq!(review.kind, TodoEventKind::Review, "{attempt}: {review:#?}");
            resume(
                &fixture,
                &run.run_id,
                review.event_id,
                TodoAction::Approve,
                None,
            )
            .unwrap();
            let (failed, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
            assert_eq!(failed.kind, TodoEventKind::VerifyFailed, "{failed:#?}");
            assert_eq!(waiting.attempt, attempt);
            resume(
                &fixture,
                &run.run_id,
                failed.event_id,
                TodoAction::Retry,
                Some(&review_text),
            )
            .unwrap();
            after = Some(failed.event_id);
        }
        let (blocked, run) = wait(&fixture, &run.run_id, after);
        assert_eq!(blocked.kind, TodoEventKind::Blocked, "{blocked:#?}");
        assert!(
            blocked
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("3 attempts"),
            "{blocked:#?}"
        );
        assert_eq!((run.status, run.attempt), (TodoRunStatus::Blocked, 3));
        assert_eq!(master_subjects(&fixture), ["init"]);
        // The repository is free for the next run.
        let store = fixture.supervisor.shared.store.as_ref().unwrap();
        assert!(store.active_run(&run.repo).unwrap().is_none());
        // The item's history ends the claim with the block and its reason.
        let item = fixture
            .supervisor
            .history_item(ITEM, Some(&run.repo))
            .unwrap();
        let kinds: Vec<_> = item.events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            [HistoryEventKind::Claimed, HistoryEventKind::Blocked]
        );
        assert_eq!(item.events[1].text, blocked.error);
        assert_eq!(item.events[1].attempt, Some(3));
    }

    #[test]
    fn a_run_cut_off_between_steps_resumes_at_its_step() {
        let fixture = todo_repo("todo-crash");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        let worker_id = waiting.worker_id.clone().unwrap();
        // The server ends after the approval is recorded, before the stop.
        runs::crash_before(&run.repo, TodoStep::Stop);
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        runs::wait_crashed(&run.repo, HANG_GUARD);
        let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(
            (cut.status, cut.step),
            (TodoRunStatus::Running, TodoStep::Stop)
        );
        assert_ne!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );

        // The next start: the worker still runs, so it is stopped; then
        // the server ends again before the pick, which a person (or the
        // ended server) already made.
        runs::crash_before(&run.repo, TodoStep::CherryPick);
        fixture.supervisor.resume_runs();
        runs::wait_crashed(&run.repo, HANG_GUARD);
        assert_eq!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );
        let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(cut.step, TodoStep::CherryPick);
        let branch = cut.branch.clone().unwrap();
        git_in(&fixture.repo, &["cherry-pick", &branch]);

        fixture.supervisor.resume_runs();
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        // Not picked twice.
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        let master = git_in(&fixture.repo, &["rev-parse", "master"]);
        assert_eq!(finished.picked.as_deref(), Some(master.trim()));
    }

    fn event_types(fixture: &Fixture, run_id: &str) -> Vec<String> {
        fixture
            .supervisor
            .shared
            .store
            .as_ref()
            .unwrap()
            .run_event_types(run_id)
            .unwrap()
            .into_iter()
            .map(|(_, kind)| kind)
            .collect()
    }

    fn command_checks(event: &TodoRunEvent) -> Vec<(Option<String>, WorkerCheckOutcome)> {
        event
            .verification
            .as_ref()
            .unwrap()
            .checks
            .iter()
            .filter(|check| check.check == "command")
            .map(|check| (check.name.clone(), check.outcome))
            .collect()
    }

    #[test]
    fn a_check_without_the_callers_environment_is_unavailable_until_a_resume_sends_it() {
        let fixture = todo_repo("todo-env");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit b.txt {SUBJECT}"), "ok b"))
            .unwrap();
        assert_eq!(run.checks, ["ok", "b"]);
        let (review, _) = wait(&fixture, &run.run_id, None);
        // The server ends after the approval, before the verify, and the
        // next one never had the environment.
        runs::crash_before(&run.repo, TodoStep::Verify);
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        runs::wait_crashed(&run.repo, HANG_GUARD);
        runs::forget_env(&run.run_id);
        fixture.supervisor.resume_runs();
        let (failed, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(failed.kind, TodoEventKind::VerifyFailed, "{failed:#?}");
        assert_eq!(
            failed.actions,
            [TodoAction::Retry, TodoAction::Verify, TodoAction::Abort]
        );
        assert_eq!(
            failed.verification.as_ref().unwrap().verdict,
            WorkerVerdict::Unavailable
        );
        assert_eq!(
            command_checks(&failed),
            [
                (Some("ok".into()), WorkerCheckOutcome::Unavailable),
                (Some("b".into()), WorkerCheckOutcome::Unavailable),
            ]
        );
        assert_eq!(waiting.attempt, 1);

        // A resume that sends no environment leaves the server none.
        let unsent = fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                env: None,
                ..resume_params(&run.run_id, failed.event_id, TodoAction::Verify)
            })
            .unwrap();
        assert_eq!(unsent.step, TodoStep::Verify);
        let (again, _) = wait(&fixture, &run.run_id, Some(failed.event_id));
        assert_eq!(again.kind, TodoEventKind::VerifyFailed, "{again:#?}");
        assert_eq!(
            again.verification.as_ref().unwrap().verdict,
            WorkerVerdict::Unavailable
        );

        // One that sends it verifies with it: both checks pass.
        resume(
            &fixture,
            &run.run_id,
            again.event_id,
            TodoAction::Verify,
            None,
        )
        .unwrap();
        let (done, finished) = wait(&fixture, &run.run_id, Some(again.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        assert_eq!(finished.attempt, 1);
        let worker_id = finished.worker_id.unwrap();
        let verification = fixture.herdr_events(&worker_id, "verification");
        let last = &verification.last().unwrap()["verification"];
        assert_eq!(last["verdict"], "verified");
        let names: Vec<&str> = last["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|check| check["name"].as_str())
            .collect();
        assert_eq!(names, ["ok", "b"]);
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
    }

    /// A resume of `event` sent from `pane` in `workspace`.
    fn resume_from(
        run_id: &str,
        event: i64,
        action: TodoAction,
        pane: &str,
        workspace: &str,
    ) -> TodoResumeParams {
        TodoResumeParams {
            caller_pane_id: Some(pane.into()),
            caller_session_id: Some(format!("{pane}-session")),
            caller_workspace_id: Some(workspace.into()),
            ..resume_params(run_id, event, action)
        }
    }

    #[test]
    fn a_runs_worker_belongs_to_the_coordinators_pane_workspace_and_tenure() {
        let fixture = todo_repo("todo-owner");
        let tenure = fixture
            .supervisor
            .coordinator_start(
                &fixture.repo.display().to_string(),
                "p-coordinator",
                Some("s-coordinator"),
            )
            .unwrap();
        let run = fixture
            .supervisor
            .todo_run(TodoRunParams {
                owner_session_id: Some("s-coordinator".into()),
                ..params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok")
            })
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        let worker_id = waiting.worker_id.clone().unwrap();
        let worker = fixture.supervisor.status(&worker_id).unwrap();
        assert_eq!(worker.workspace_id.as_deref(), Some("ws-coordinator"));
        assert_eq!(worker.owner_pane_id.as_deref(), Some("p-coordinator"));
        assert_eq!(worker.owner_session_id.as_deref(), Some("s-coordinator"));
        assert_eq!(
            worker.owner_coordinator_id.as_deref(),
            Some(tenure.coordinator_id.as_str())
        );
        assert!(fixture
            .supervisor
            .list()
            .iter()
            .any(|listed| listed.worker_id == worker_id
                && listed.workspace_id.as_deref() == Some("ws-coordinator")));
        // The review is the coordinator pane's obligation.
        let owed = fixture.supervisor.obligations(Some("p-coordinator"));
        assert_eq!(owed.len(), 1, "{owed:#?}");
        assert_eq!(
            (owed[0].worker_id.as_str(), owed[0].reason),
            (worker_id.as_str(), WorkerAttentionReason::TurnEnd)
        );

        // Another pane that is there cannot resume it, nor can a caller
        // outside a pane.
        let refused = fixture
            .supervisor
            .todo_resume(resume_from(
                &run.run_id,
                review.event_id,
                TodoAction::Approve,
                "p-other",
                "ws-other",
            ))
            .unwrap_err();
        assert_eq!(refused.code(), "run_owned_elsewhere", "{refused}");
        assert!(refused.to_string().contains("p-coordinator"), "{refused}");
        let outside = fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                caller_pane_id: None,
                ..resume_params(&run.run_id, review.event_id, TodoAction::Approve)
            })
            .unwrap_err();
        assert_eq!(outside.code(), "run_owned_elsewhere", "{outside}");
        assert_eq!(
            fixture.supervisor.obligations(Some("p-coordinator")).len(),
            1
        );

        // Its owner's resume settles the obligation.
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert!(fixture
            .supervisor
            .obligations(Some("p-coordinator"))
            .is_empty());
        assert!(run_events(&fixture, &run.run_id, "run_owner_taken").is_empty());
    }

    #[test]
    fn a_handoff_moves_the_tenures_item_run_and_worker_to_the_next_tenure() {
        let fixture = todo_repo("todo-handoff");
        let repo = fixture.repo.display().to_string();
        let tenure = fixture
            .supervisor
            .coordinator_start(&repo, "p-coordinator", Some("s-coordinator"))
            .unwrap();
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        // The run's item is the tenure's current item.
        let status = fixture.supervisor.coordinator_status(Some(&repo)).unwrap();
        assert_eq!(status[0].item.as_deref(), Some(ITEM));
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        let worker_id = waiting.worker_id.clone().unwrap();
        assert_eq!(
            fixture.supervisor.obligations(Some("p-coordinator")).len(),
            1
        );

        // A pane that coordinates already cannot take it, nor can its own.
        fixture
            .supervisor
            .coordinator_start("/elsewhere", "p-busy", None)
            .unwrap();
        for (to, code) in [
            ("p-busy", "coordinator_active"),
            ("p-coordinator", "invalid_request"),
        ] {
            let refused = fixture
                .supervisor
                .coordinator_handoff(None, Some("p-coordinator"), to, None, None)
                .unwrap_err();
            assert_eq!(refused.code(), code, "{refused}");
        }
        assert_eq!(
            fixture
                .supervisor
                .coordinator_handoff(None, Some("p-nobody"), "p-next", None, None)
                .unwrap_err()
                .code(),
            "coordinator_not_found"
        );

        let next = fixture
            .supervisor
            .coordinator_handoff(
                None,
                Some("p-coordinator"),
                "p-next",
                Some("s-next"),
                Some("ws-next"),
            )
            .unwrap();
        assert_ne!(next.coordinator_id, tenure.coordinator_id);
        assert_eq!(next.epoch, tenure.epoch + 1);
        assert_eq!(
            (
                next.pane_id.as_deref(),
                next.session_id.as_deref(),
                next.item.as_deref()
            ),
            (Some("p-next"), Some("s-next"), Some(ITEM))
        );
        let store = fixture.supervisor.shared.store.as_ref().unwrap();
        let old = store.coordinator(&tenure.coordinator_id).unwrap().unwrap();
        assert_eq!(
            old.end_reason.as_deref(),
            Some(crate::workers::coordinators::HANDED_OFF)
        );
        let handoff = run_events(&fixture, &tenure.coordinator_id, "handoff");
        assert_eq!(handoff.len(), 1, "{handoff:#?}");
        assert_eq!(
            handoff[0]["to_coordinator_id"],
            next.coordinator_id.as_str()
        );
        assert_eq!(handoff[0]["epoch"], next.epoch);
        // The worker and its obligation, and the run, are the next
        // tenure's in its pane.
        let worker = fixture.supervisor.status(&worker_id).unwrap();
        assert_eq!(
            (
                worker.owner_pane_id.as_deref(),
                worker.owner_session_id.as_deref(),
                worker.owner_coordinator_id.as_deref()
            ),
            (
                Some("p-next"),
                Some("s-next"),
                Some(next.coordinator_id.as_str())
            )
        );
        assert!(fixture
            .supervisor
            .obligations(Some("p-coordinator"))
            .is_empty());
        assert_eq!(fixture.supervisor.obligations(Some("p-next")).len(), 1);
        let moved = run_events(&fixture, &run.run_id, "run_owner_moved");
        assert_eq!(moved.len(), 1, "{moved:#?}");
        assert_eq!(moved[0]["to_workspace"], "ws-next");
        assert_eq!(moved[0]["to_coordinator"], next.coordinator_id.as_str());

        // The old pane cannot resume the run any more; the next one can,
        // and the run's end clears the tenure's item.
        let refused = fixture
            .supervisor
            .todo_resume(resume_from(
                &run.run_id,
                review.event_id,
                TodoAction::Approve,
                "p-coordinator",
                "ws-coordinator",
            ))
            .unwrap_err();
        assert_eq!(refused.code(), "run_owned_elsewhere", "{refused}");
        fixture
            .supervisor
            .todo_resume(resume_from(
                &run.run_id,
                review.event_id,
                TodoAction::Approve,
                "p-next",
                "ws-next",
            ))
            .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        let status = fixture.supervisor.coordinator_status(Some(&repo)).unwrap();
        assert_eq!(status[0].coordinator_id, next.coordinator_id);
        assert_eq!(status[0].item, None);
        let items = run_events(&fixture, &next.coordinator_id, "coordinator_item");
        assert_eq!(items.len(), 2, "{items:#?}");
        assert_eq!(items[1]["item"], serde_json::Value::Null);
    }

    #[test]
    fn a_resumed_coordinator_resumes_its_run_from_its_new_pane() {
        let fixture = todo_repo("todo-resumed-owner");
        let repo = fixture.repo.display().to_string();
        fixture
            .supervisor
            .coordinator_start(&repo, "p-coordinator", Some("s-coordinator"))
            .unwrap();
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        fixture
            .supervisor
            .coordinator_resume("p-resumed", "s-coordinator", Some("ws-resumed"))
            .unwrap()
            .unwrap();
        let moved = run_events(&fixture, &run.run_id, "run_owner_moved");
        assert_eq!(moved[0]["to_pane"], "p-resumed");
        // The driver's own writes keep the moved owner.
        fixture
            .supervisor
            .todo_resume(resume_from(
                &run.run_id,
                review.event_id,
                TodoAction::Approve,
                "p-resumed",
                "ws-resumed",
            ))
            .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        let stored = fixture
            .supervisor
            .shared
            .store
            .as_ref()
            .unwrap()
            .run(&run.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner_pane.as_deref(), Some("p-resumed"));
        assert_eq!(stored.workspace.as_deref(), Some("ws-resumed"));
        assert!(run_events(&fixture, &run.run_id, "run_owner_taken").is_empty());
    }

    #[test]
    fn after_its_owner_pane_is_gone_a_resume_takes_the_run_over() {
        let fixture = todo_repo("todo-takeover");
        let task = format!("commit a.txt {SUBJECT}");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &task, "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        // The coordinator's pane closes: its runs are listed as owners
        // herdr checks when panes close.
        assert!(fixture
            .supervisor
            .owner_panes()
            .contains(&"p-coordinator".to_owned()));
        fixture
            .supervisor
            .owner_event("p-coordinator", OwnerEvent::PaneClosed, "");

        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                // The review: amend the commit attempt 2 starts from.
                task: Some(format!("amend a.txt {SUBJECT}")),
                ..resume_from(
                    &run.run_id,
                    review.event_id,
                    TodoAction::Retry,
                    "p-new",
                    "ws-new",
                )
            })
            .unwrap();
        let taken = run_events(&fixture, &run.run_id, "run_owner_taken");
        assert_eq!(taken.len(), 1, "{taken:#?}");
        assert_eq!(taken[0]["from_pane"], "p-coordinator");
        assert_eq!(taken[0]["to_pane"], "p-new");
        assert_eq!(taken[0]["to_workspace"], "ws-new");
        assert!(
            taken[0]["cause"]
                .as_str()
                .unwrap_or_default()
                .contains("p-coordinator"),
            "{taken:#?}"
        );

        // The next attempt's worker belongs to the pane that took it over.
        let (second, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(second.kind, TodoEventKind::Review, "{second:#?}");
        assert_eq!(waiting.attempt, 2);
        let worker = fixture
            .supervisor
            .status(waiting.worker_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(worker.owner_pane_id.as_deref(), Some("p-new"));
        assert_eq!(worker.owner_session_id.as_deref(), Some("p-new-session"));
        assert_eq!(worker.workspace_id.as_deref(), Some("ws-new"));
        assert_eq!(fixture.supervisor.obligations(Some("p-new")).len(), 1);

        // The new owner is there now: another pane is refused.
        let refused = fixture
            .supervisor
            .todo_resume(resume_from(
                &run.run_id,
                second.event_id,
                TodoAction::Approve,
                "p-coordinator",
                "ws-coordinator",
            ))
            .unwrap_err();
        assert_eq!(refused.code(), "run_owned_elsewhere", "{refused}");
        assert!(refused.to_string().contains("p-new"), "{refused}");
        fixture
            .supervisor
            .todo_resume(resume_from(
                &run.run_id,
                second.event_id,
                TodoAction::Approve,
                "p-new",
                "ws-new",
            ))
            .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(second.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert!(fixture.supervisor.obligations(Some("p-new")).is_empty());
    }

    fn resume_params(run_id: &str, event: i64, action: TodoAction) -> TodoResumeParams {
        TodoResumeParams {
            run_id: run_id.to_owned(),
            action,
            event,
            task: None,
            request_id: None,
            decision: None,
            answers: Vec::new(),
            message: None,
            env: Some(caller_env()),
            ignore_usage: false,
            keep_open: false,
            note: None,
            close: None,
            next: None,
            stop_reason: None,
            caller_pane_id: Some("p-coordinator".into()),
            caller_session_id: None,
            caller_workspace_id: Some("ws-coordinator".into()),
        }
    }

    #[test]
    fn a_run_another_server_holds_is_driven_only_after_it_lets_go() {
        let fixture = todo_repo("todo-lock");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        let worker_id = waiting.worker_id.clone().unwrap();
        runs::crash_before(&run.repo, TodoStep::Stop);
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        runs::wait_crashed(&run.repo, HANG_GUARD);

        // The old server of a handoff still holds the run.
        let held = std::fs::File::open(fixture.supervisor.run_lock_path(&run.run_id)).unwrap();
        held.lock().unwrap();
        fixture.supervisor.resume_runs();
        wait_until("the driver's wait for the run lock", || {
            event_types(&fixture, &run.run_id).contains(&"run_lock_wait".to_owned())
        });
        // It did nothing while it waited.
        let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(
            (cut.status, cut.step),
            (TodoRunStatus::Running, TodoStep::Stop)
        );
        assert_ne!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );

        // The old server lets go: the driver takes the run on.
        drop(held);
        let (done, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        let types = event_types(&fixture, &run.run_id);
        let at = |kind: &str| types.iter().position(|seen| seen == kind).unwrap();
        assert!(at("run_lock_wait") < at("run_lock_taken"), "{types:?}");
        assert!(at("run_lock_taken") < at("run_stop_intent"), "{types:?}");
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        // An ended run leaves no lock file.
        assert!(!fixture.supervisor.run_lock_path(&run.run_id).exists());
    }

    #[test]
    fn after_a_handoff_the_old_server_lets_go_of_its_runs_without_blocking_them() {
        let fixture = todo_repo("todo-let-go");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, "block", "ok"))
            .unwrap();
        // The driver waits for the worker's turn, which does not end.
        let status = || fixture.supervisor.todo_status(&run.run_id).unwrap();
        wait_until("the run's attention step", || {
            status().step == TodoStep::Attention
        });
        let worker_id = status().worker_id.unwrap();
        fixture.wait_for(&worker_id, |worker| worker.state == WorkerState::Working);
        fixture.supervisor.let_go_of_runs();
        wait_until("the old driver letting go", || {
            event_types(&fixture, &run.run_id).contains(&"run_let_go".to_owned())
        });
        let left = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(
            (left.status, left.step, left.error),
            (TodoRunStatus::Running, TodoStep::Attention, None)
        );
        // Its lock is free for the new server.
        let lock = std::fs::File::open(fixture.supervisor.run_lock_path(&run.run_id)).unwrap();
        lock.try_lock().unwrap();
        fixture.supervisor.kill(&worker_id, false).unwrap();
        fixture.wait(&worker_id, WorkerWaitUntil::Exit);
    }

    #[test]
    fn a_worker_alive_after_its_stop_waits_for_a_force_stop() {
        let fixture = todo_repo("todo-still-alive");
        let run = fixture
            .supervisor
            .todo_run(params(
                &fixture,
                &format!("stubborn-commit a.txt {SUBJECT}"),
                "ok",
            ))
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        let worker_id = waiting.worker_id.clone().unwrap();
        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        // The stop is sent; the driver waits for the exit with no deadline,
        // and nothing reports the worker stuck until the coordinator asks.
        wait_until("the stop's SIGTERM", || {
            !fixture.herdr_events(&worker_id, "signal").is_empty()
        });
        assert_eq!(
            event_types(&fixture, &run.run_id)
                .iter()
                .filter(|kind| *kind == "run_event")
                .count(),
            1,
            "only the review so far"
        );
        let asked = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(asked.status, TodoRunStatus::Waiting, "{asked:#?}");
        let (alive, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(alive.kind, TodoEventKind::StillAlive, "{alive:#?}");
        assert_eq!(asked.pending_event, Some(alive.event_id));
        // Asking again raises nothing new.
        assert_eq!(
            fixture
                .supervisor
                .todo_status(&run.run_id)
                .unwrap()
                .pending_event,
            Some(alive.event_id)
        );
        assert_eq!(alive.actions, [TodoAction::ForceStop, TodoAction::Abort]);
        assert!(
            alive
                .error
                .as_deref()
                .unwrap_or_default()
                .contains(&worker_id),
            "{alive:#?}"
        );
        assert_eq!(
            (waiting.status, waiting.step),
            (TodoRunStatus::Waiting, TodoStep::Stop)
        );
        // Nothing killed it: it still runs.
        assert_ne!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );
        assert!(fixture
            .herdr_events(&worker_id, "signal")
            .iter()
            .all(|signal| signal["signal"] == "SIGTERM"));

        resume(
            &fixture,
            &run.run_id,
            alive.event_id,
            TodoAction::ForceStop,
            None,
        )
        .unwrap();
        let (done, _) = wait(&fixture, &run.run_id, Some(alive.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert!(fixture
            .herdr_events(&worker_id, "signal")
            .iter()
            .any(|signal| signal["signal"] == "SIGKILL"));
        assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        // The run's owner asked for that end: it owes nothing.
        assert!(fixture
            .supervisor
            .obligations(Some("p-coordinator"))
            .is_empty());
    }

    /// Commits `.herdr/operations.toml` with `text` on `master`.
    fn commit_operations(fixture: &Fixture, text: &str) {
        std::fs::write(fixture.repo.join(".herdr/operations.toml"), text).unwrap();
        git_in(&fixture.repo, &["add", ".herdr/operations.toml"]);
        git_in(&fixture.repo, &["commit", "-q", "-m", "operations"]);
    }

    fn grant_params(fixture: &Fixture, hash: &str) -> crate::api::schema::TodoGrantParams {
        crate::api::schema::TodoGrantParams {
            cwd: fixture.repo.display().to_string(),
            operation: "prepare".into(),
            hash: hash.into(),
        }
    }

    /// The hash a `grant_required` refusal shows.
    fn shown_hash(message: &str) -> String {
        let (_, rest) = message.split_once("--hash ").expect(message);
        rest.split_whitespace()
            .next()
            .unwrap()
            .trim_end_matches('`')
            .to_owned()
    }

    const OPERATIONS: &str = "version = 1\n[prepare]\nargv = [\"true\"]\n\
                              [prepare.capabilities]\nenv = { names = [\"PATH\"] }\n";

    #[test]
    fn a_prepare_runs_only_under_the_users_grant_of_its_base_definition() {
        let fixture = todo_repo("todo-prepare-grant");
        let repo = fixture.repo.clone();
        let base = || git_in(&repo, &["rev-parse", "master"]).trim().to_owned();
        let task = format!("commit a.txt {SUBJECT}");
        // The worker's tree is never read: an uncommitted file asks nothing.
        std::fs::write(repo.join(".herdr/operations.toml"), OPERATIONS).unwrap();
        assert_eq!(
            fixture.supervisor.plan_operations(&repo, &base()).unwrap(),
            None
        );
        commit_operations(&fixture, OPERATIONS);
        let refused = fixture
            .supervisor
            .todo_run(params(&fixture, &task, "ok"))
            .unwrap_err();
        if !crate::platform::CONFINED_JOB_SUPPORTED {
            assert_eq!(refused.code(), "capability_unsupported", "{refused}");
            return;
        }
        assert_eq!(refused.code(), "grant_required", "{refused}");
        assert!(fixture
            .supervisor
            .todo_runs(None, None)
            .unwrap()
            .0
            .is_empty());
        let hash = shown_hash(&refused.to_string());

        // The grant names the exact definition; another hash is refused.
        assert_eq!(
            fixture
                .supervisor
                .todo_grant(grant_params(&fixture, "0000"))
                .unwrap_err()
                .code(),
            "invalid_request"
        );
        let grant = fixture
            .supervisor
            .todo_grant(grant_params(&fixture, &hash))
            .unwrap();
        assert_eq!(
            (grant.operation.as_str(), grant.hash.as_str()),
            ("prepare", hash.as_str())
        );
        assert_eq!(grant.definition["argv"], serde_json::json!(["true"]));
        let plan = fixture
            .supervisor
            .plan_operations(&repo, &base())
            .unwrap()
            .unwrap();
        assert_eq!(
            (plan.hash.as_str(), plan.granted_ms),
            (hash.as_str(), grant.granted_ms)
        );

        // A changed request is a new question; the old grant stays.
        commit_operations(
            &fixture,
            &OPERATIONS.replace("[\"PATH\"]", "[\"PATH\", \"HOME\"]"),
        );
        let changed = fixture
            .supervisor
            .todo_run(params(&fixture, &task, "ok"))
            .unwrap_err();
        assert_eq!(changed.code(), "grant_required", "{changed}");
        assert_ne!(shown_hash(&changed.to_string()), hash);
        let grants = fixture
            .supervisor
            .todo_grants(crate::api::schema::TodoGrantsParams {
                cwd: Some(repo.display().to_string()),
            })
            .unwrap();
        assert_eq!(grants.len(), 1);

        // A request no adapter here enforces refuses the run, typed.
        for unsupported in [
            "version = 1\n[worker.capabilities]\npty = {}\n",
            "version = 1\n[prepare]\nargv = [\"true\"]\n[prepare.capabilities]\n\
             \"exec.unsandboxed\" = { argv = [\"x\"] }\n",
        ] {
            commit_operations(&fixture, unsupported);
            let error = fixture
                .supervisor
                .todo_run(params(&fixture, &task, "ok"))
                .unwrap_err();
            assert_eq!(error.code(), "capability_unsupported", "{error}");
        }

        // Granted again as first defined, the run records what it runs with.
        commit_operations(&fixture, OPERATIONS);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &task, "ok"))
            .unwrap();
        let created = run_events(&fixture, &run.run_id, "run_created");
        assert_eq!(created[0]["grants"]["hash"], hash.as_str(), "{created:#?}");
        assert_eq!(
            fixture
                .supervisor
                .load_run(&run.run_id)
                .unwrap()
                .finish
                .prepare
                .map(|plan| plan.hash),
            Some(hash.clone())
        );
        let (event, _) = wait(&fixture, &run.run_id, None);
        let started = run_events(&fixture, &run.run_id, "run_prepare_started");
        assert_eq!(started[0]["grant"]["hash"], hash.as_str(), "{started:#?}");
        let prepared = run_events(&fixture, &run.run_id, "run_prepared");
        if event.kind == TodoEventKind::Blocked {
            // Inside another sandbox (an agent's), the job cannot be
            // confined: the run blocks rather than run it unconfined.
            assert_eq!(prepared[0]["status"], "failed", "{prepared:#?}");
            assert!(event.error.unwrap_or_default().contains("prepare failed"));
            return;
        }
        assert_eq!(prepared[0]["status"], "done", "{prepared:#?}");
        assert_eq!(event.kind, TodoEventKind::Review, "{event:#?}");
        abort(&fixture, &run.run_id, event.event_id);
    }

    #[test]
    fn preflight_refuses_what_a_run_cannot_do() {
        let fixture = todo_repo("todo-preflight");
        let task = format!("commit a.txt {SUBJECT}");
        let refused = |params: TodoRunParams| {
            let error = fixture.supervisor.todo_run(params).unwrap_err();
            (error.code(), error.to_string())
        };
        let (code, message) = refused(TodoRunParams {
            message: "Feat: Add a".into(),
            ..params(&fixture, &task, "ok")
        });
        assert_eq!(code, "todo_preflight_failed", "{message}");
        let (code, message) = refused(TodoRunParams {
            item: "t-zzzzzzzz".into(),
            ..params(&fixture, &task, "ok")
        });
        assert_eq!(code, "todo_preflight_failed");
        assert!(message.contains("TODO.md"), "{message}");
        let (code, message) = refused(params(&fixture, &task, "nosuch"));
        assert_eq!(code, "todo_preflight_failed");
        assert!(message.contains("b, maintenance, never, ok"), "{message}");
        let (code, _) = refused(TodoRunParams {
            paths: vec!["/abs".into()],
            ..params(&fixture, &task, "ok")
        });
        assert_eq!(code, "todo_preflight_failed");
        let (code, message) = refused(params(&fixture, &task, "ok ok"));
        assert_eq!(code, "todo_preflight_failed");
        assert!(message.contains("twice"), "{message}");
        let (code, message) = refused(params(&fixture, &task, "ok nosuch"));
        assert_eq!(code, "todo_preflight_failed");
        assert!(message.contains("nosuch"), "{message}");
        assert!(fixture
            .supervisor
            .todo_runs(None, None)
            .unwrap()
            .0
            .is_empty());

        // One run in progress per repository.
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &task, "ok"))
            .unwrap();
        let (code, message) = refused(params(&fixture, &task, "ok"));
        assert_eq!(code, "todo_run_active");
        assert!(message.contains(&run.run_id), "{message}");
        let (review, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review);
    }

    const ITEM2: &str = "t-bcde3456";

    /// Gives the repository what the steps after the cherry-pick use: an
    /// `origin` (a bare repository with its `master`), the stub install
    /// `install.sh` (failing while `INSTALL_FAIL` is set, else writing
    /// `HEAD` as the build `build_id` prints), `scripts/todo_edit.py`,
    /// `DECISIONS.md` and a second item. Returns the bare repository.
    fn with_finish(fixture: &Fixture) -> PathBuf {
        let repo = &fixture.repo;
        std::fs::write(
            repo.join("install.sh"),
            "if [ -n \"$INSTALL_FAIL\" ]; then echo \"install broke\"; exit 1; fi\n\
             mkdir -p target\n\
             git rev-parse HEAD > target/installed\n\
             echo installed\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/todo_edit.py"),
            repo.join("scripts/todo_edit.py"),
        )
        .unwrap();
        std::fs::write(repo.join("DECISIONS.md"), "# Decisions\n\nIntro.\n").unwrap();
        std::fs::write(
            repo.join("TODO.md"),
            format!("# TODO\n\n- [ ] The driven item [{ITEM}]\n  Its text.\n\n- [ ] The second item [{ITEM2}]\n"),
        )
        .unwrap();
        let checks = repo.join(".herdr/checks.toml");
        let mut text = std::fs::read_to_string(&checks).unwrap();
        text.push_str(
            "[install]\ncommand = [\"sh\", \"install.sh\"]\nbuild_id = [\"cat\", \"target/installed\"]\n",
        );
        std::fs::write(&checks, text).unwrap();
        git_in(repo, &["add", "."]);
        git_in(repo, &["commit", "-q", "-m", "finish setup"]);
        let remote = fixture.root.join("origin.git");
        git_in(
            &fixture.root,
            &["init", "-q", "--bare", "-b", "master", "origin.git"],
        );
        git_in(
            repo,
            &["remote", "add", "origin", &remote.display().to_string()],
        );
        git_in(repo, &["push", "-q", "origin", "master"]);
        remote
    }

    fn rev(dir: &Path, rev: &str) -> String {
        git_in(dir, &["rev-parse", rev]).trim().to_owned()
    }

    fn approve_with(
        fixture: &Fixture,
        run_id: &str,
        event: i64,
        note: Option<&str>,
        close: Option<&str>,
    ) {
        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                note: note.map(str::to_owned),
                close: close.map(str::to_owned),
                stop_reason: close.map(|_| STOP_REASON.to_owned()),
                ..resume_params(run_id, event, TodoAction::Approve)
            })
            .unwrap();
    }

    /// The stop reason [`approve_with`] closes an item with.
    const STOP_REASON: &str = "the next item waits on the user";

    /// `todo.resume`'s next run of [`ITEM2`]: `b.txt` with the run's checks.
    fn next_of_item2() -> crate::api::schema::TodoNextRun {
        crate::api::schema::TodoNextRun {
            item: ITEM2.into(),
            task: "commit b.txt feat: add b".into(),
            message: "feat: add b".into(),
            paths: vec!["*.txt".into()],
            checks: Vec::new(),
        }
    }

    fn close_with_next(fixture: &Fixture, run_id: &str, event: i64) {
        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                close: Some("## Closed\n\n- Chosen.\n".into()),
                next: Some(next_of_item2()),
                ..resume_params(run_id, event, TodoAction::Approve)
            })
            .unwrap();
    }

    #[test]
    fn a_close_needs_a_next_item_or_a_stop_reason_which_the_history_and_a_notice_get() {
        let fixture = todo_repo("todo-close-stop");
        with_finish(&fixture);
        let repo = repo_of(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        let refused = |params: TodoResumeParams| {
            let error = fixture.supervisor.todo_resume(params).unwrap_err();
            assert_eq!(error.code(), "invalid_request", "{error}");
            error.to_string()
        };
        let approve = || resume_params(&run.run_id, review.event_id, TodoAction::Approve);
        let close = Some("## Closed\n".to_owned());
        // A close names what comes after it.
        let message = refused(TodoResumeParams {
            close: close.clone(),
            ..approve()
        });
        assert!(
            message.contains("--next") && message.contains("--stop-reason"),
            "{message}"
        );
        // Not both, not without a close, not blank, not the closed item.
        refused(TodoResumeParams {
            close: close.clone(),
            next: Some(next_of_item2()),
            stop_reason: Some("why".into()),
            ..approve()
        });
        refused(TodoResumeParams {
            note: Some("a note".into()),
            stop_reason: Some("why".into()),
            ..approve()
        });
        refused(TodoResumeParams {
            next: Some(next_of_item2()),
            ..approve()
        });
        refused(TodoResumeParams {
            close: close.clone(),
            stop_reason: Some("  ".into()),
            ..approve()
        });
        refused(TodoResumeParams {
            close: close.clone(),
            next: Some(crate::api::schema::TodoNextRun {
                item: ITEM.into(),
                ..next_of_item2()
            }),
            ..approve()
        });
        refused(TodoResumeParams {
            close: close.clone(),
            next: Some(crate::api::schema::TodoNextRun {
                message: "Add b".into(),
                ..next_of_item2()
            }),
            ..approve()
        });
        // The refusals left the run waiting on its review.
        let waiting = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(waiting.pending_event, Some(review.event_id));

        approve_with(
            &fixture,
            &run.run_id,
            review.event_id,
            None,
            close.as_deref(),
        );
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(finished.stop_reason.as_deref(), Some(STOP_REASON));
        assert_eq!((finished.next_item, finished.next_run_id), (None, None));
        // The item's history has the reason after its close; the close
        // stays the item's latest record.
        let item = fixture.supervisor.history_item(ITEM, Some(&repo)).unwrap();
        assert_eq!(
            history_kinds(&item),
            [
                HistoryEventKind::Claimed,
                HistoryEventKind::Closed,
                HistoryEventKind::Stopped
            ]
        );
        assert_eq!(item.events[2].text.as_deref(), Some(STOP_REASON));
        assert_eq!(item.events[2].run_id.as_deref(), Some(run.run_id.as_str()));
        let list = fixture.supervisor.history_list(Some(&repo)).unwrap();
        assert_eq!(list[0].last.kind, HistoryEventKind::Closed);
        // The user is notified once; a restart does not notify again.
        let notices = crate::workers::take_user_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].title.contains(ITEM), "{notices:?}");
        assert_eq!(notices[0].body, STOP_REASON);
        assert_eq!(
            run_events(&fixture, &run.run_id, "run_stop_notified").len(),
            1
        );
        fixture.supervisor.resume_runs();
        assert!(crate::workers::take_user_notices().is_empty());
        // No next run started.
        assert_eq!(fixture.supervisor.todo_runs(None, None).unwrap().0.len(), 1);
    }

    #[test]
    fn a_close_with_a_next_item_starts_its_run_once_the_run_is_done() {
        let fixture = todo_repo("todo-close-next");
        with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        close_with_next(&fixture, &run.run_id, review.event_id);
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(master_subjects(&fixture)[0], "docs(todo): close t-abcd2345");
        assert_eq!(finished.next_item.as_deref(), Some(ITEM2));
        // The wait returned once the next run started.
        assert!(finished.next_run_id.is_some(), "{finished:#?}");
        // The driver settled the next start before it let go of the run.
        let closed = fixture.supervisor.todo_status(&run.run_id).unwrap();
        let next_id = closed.next_run_id.clone().expect("the next run started");
        assert_eq!(closed.next_refusal, None);
        let types = event_types(&fixture, &run.run_id);
        let at = |kind: &str| types.iter().position(|seen| seen == kind).unwrap();
        assert!(at("run_cleaned") < at("run_next_intent"), "{types:?}");
        assert!(at("run_next_intent") < at("run_next_started"), "{types:?}");
        let started = run_events(&fixture, &run.run_id, "run_next_started");
        assert_eq!(started[0]["run_id"], serde_json::json!(next_id));
        assert_eq!(started[0]["already"], false);
        // The next run is the item's, with this run's owner and checks.
        let next = fixture.supervisor.load_run(&next_id).unwrap();
        assert_eq!(
            (next.info.item.as_str(), next.info.message.as_str()),
            (ITEM2, "feat: add b")
        );
        assert_eq!(next.info.checks, ["ok"]);
        assert_eq!(next.owner_pane.as_deref(), Some("p-coordinator"));
        assert_eq!(next.workspace.as_deref(), Some("ws-coordinator"));
        let (review, _) = wait(&fixture, &next_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        assert!(
            review
                .diff_stat
                .as_deref()
                .unwrap_or_default()
                .contains("b.txt"),
            "{review:#?}"
        );
        abort(&fixture, &next_id, review.event_id);
    }

    #[test]
    fn a_refused_next_start_is_an_event_on_the_done_run() {
        let fixture = todo_repo("todo-close-next-refused");
        with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        // Claude's usage rises while the run finishes: the gate refuses the
        // next start.
        fixture.supervisor.set_usage_for_test(fresh_usage(95, 10));
        crate::workers::take_user_notices();
        close_with_next(&fixture, &run.run_id, review.event_id);
        // The wait on the run returns the refusal as its result, not the
        // `done` event before it.
        let (refused, ended) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(refused.kind, TodoEventKind::NextRefused, "{refused:#?}");
        assert!(refused.actions.is_empty(), "{refused:#?}");
        let why = refused.error.clone().unwrap_or_default();
        assert!(
            why.contains(ITEM2) && why.contains("usage_gate") && why.contains("95% used"),
            "{why}"
        );
        assert_eq!(
            (
                ended.status,
                ended.next_refusal.as_deref(),
                ended.next_run_id
            ),
            (TodoRunStatus::Done, Some(why.as_str()), None)
        );
        // The user is told, once.
        let notices = crate::workers::take_user_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].title.contains(ITEM2), "{notices:?}");
        assert_eq!(notices[0].body, why);
        // Settled: a restart does not try again.
        fixture.supervisor.resume_runs();
        assert!(crate::workers::take_user_notices().is_empty());
        assert_eq!(fixture.supervisor.todo_runs(None, None).unwrap().0.len(), 1);
        assert_eq!(
            run_events(&fixture, &run.run_id, "run_next_intent").len(),
            1
        );
    }

    /// The next start's preflight finds the disk short; the folder slot's
    /// sweep (a stand-in that frees the space) runs, and preflight looks
    /// once more and starts the next run.
    #[test]
    fn a_next_start_short_of_disk_sweeps_the_slot_and_starts() {
        let fixture = todo_repo("todo-close-next-disk");
        let free = fixture.repo.parent().unwrap().join("free-gib-for-test");
        std::fs::create_dir_all(fixture.repo.join("scripts")).unwrap();
        std::fs::write(
            fixture.repo.join("scripts/target_sweep.py"),
            format!(
                "import pathlib, sys\n\
                 free = pathlib.Path({:?})\n\
                 if sys.argv[1] == 'slot' and free.exists():\n\
                 \x20   free.write_text('100')\n",
                free.display().to_string()
            ),
        )
        .unwrap();
        with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        // The disk fills while the run finishes.
        std::fs::write(&free, "-1").unwrap();
        crate::workers::take_user_notices();
        close_with_next(&fixture, &run.run_id, review.event_id);
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(finished.next_refusal, None, "{finished:#?}");
        let next_id = finished.next_run_id.clone().expect("the next run started");
        assert_eq!(std::fs::read_to_string(&free).unwrap(), "100");
        assert!(crate::workers::take_user_notices().is_empty());
        let (review, _) = wait(&fixture, &next_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        abort(&fixture, &next_id, review.event_id);
    }

    #[test]
    fn a_restart_between_done_and_the_next_start_starts_it() {
        let fixture = todo_repo("todo-close-next-restart");
        with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        // The server ends after the intent, before the start.
        runs::crash_before(&run.repo, TodoStep::Done);
        close_with_next(&fixture, &run.run_id, review.event_id);
        let (done, _) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!((cut.next_run_id, cut.next_refusal), (None, None));
        assert_eq!(
            run_events(&fixture, &run.run_id, "run_next_intent").len(),
            1
        );
        assert_eq!(fixture.supervisor.todo_runs(None, None).unwrap().0.len(), 1);

        // The server that starts settles it from the recorded intent.
        fixture.supervisor.resume_runs();
        wait_until("the next run's start", || {
            fixture
                .supervisor
                .todo_status(&run.run_id)
                .unwrap()
                .next_run_id
                .is_some()
        });
        let next_id = fixture
            .supervisor
            .todo_status(&run.run_id)
            .unwrap()
            .next_run_id
            .unwrap();
        assert_eq!(
            run_events(&fixture, &run.run_id, "run_next_intent").len(),
            1
        );
        // Started once: another start finds nothing left to settle.
        fixture.supervisor.resume_runs();
        let runs = fixture.supervisor.todo_runs(None, None).unwrap().0;
        assert_eq!(
            runs.iter().map(|run| run.item.as_str()).collect::<Vec<_>>(),
            [ITEM, ITEM2]
        );
        let (review, _) = wait(&fixture, &next_id, None);
        abort(&fixture, &next_id, review.event_id);
    }

    fn run_events(fixture: &Fixture, run_id: &str, kind: &str) -> Vec<serde_json::Value> {
        fixture
            .supervisor
            .shared
            .store
            .as_ref()
            .unwrap()
            .run_events_of(run_id, kind)
            .unwrap()
    }

    #[test]
    fn a_run_installs_notes_the_item_pushes_and_cleans_up() {
        let fixture = todo_repo("todo-finish");
        let remote = with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        // Only an approval takes a note, and not with a closing decision.
        let refused = fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                note: Some("x".into()),
                close: Some("y".into()),
                ..resume_params(&run.run_id, review.event_id, TodoAction::Approve)
            })
            .unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");
        approve_with(
            &fixture,
            &run.run_id,
            review.event_id,
            Some("Done by the driver.\n"),
            None,
        );
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        let master = rev(&fixture.repo, "master");
        let picked = finished.picked.clone().unwrap();
        assert_eq!(
            master_subjects(&fixture),
            [
                "docs(todo): note on t-abcd2345",
                SUBJECT,
                "finish setup",
                "init"
            ]
        );
        // The install ran on the picked commit, and the build is recorded.
        assert_eq!(finished.installed_build.as_deref(), Some(picked.as_str()));
        // The note went under the item, committed by path.
        assert_eq!(finished.todo_commit.as_deref(), Some(master.as_str()));
        let todo = std::fs::read_to_string(fixture.repo.join("TODO.md")).unwrap();
        assert!(
            todo.contains(&format!(
                "- [ ] The driven item [{ITEM}]\n  Its text.\n  Done by the driver.\n"
            )),
            "{todo}"
        );
        assert_eq!(
            git_in(
                &fixture.repo,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            ""
        );
        // Pushed as a fast-forward.
        assert_eq!(rev(&remote, "master"), master);
        assert_eq!(finished.pushed.as_deref(), Some(master.as_str()));
        assert_eq!(done.commits, [picked, master]);
        // The slot has the branch checked out: kept, and recorded.
        assert_eq!(
            finished.kept_branches,
            [format!("todo/t-abcd2345-{}-1", run.run_id)]
        );
        let types = event_types(&fixture, &run.run_id);
        let after_pick: Vec<&str> = types
            .iter()
            .skip_while(|kind| *kind != "run_picked")
            .map(String::as_str)
            .collect();
        assert_eq!(
            after_pick,
            [
                "run_picked",
                "run_install_intent",
                "run_installed",
                "run_todo_intent",
                "run_todo_committed",
                "run_push_intent",
                "run_pushed",
                "run_cleaned",
                "run_event",
            ]
        );
    }

    #[test]
    fn a_failed_install_waits_for_the_coordinator_and_its_retry_installs() {
        let fixture = todo_repo("todo-install-fail");
        let remote = with_finish(&fixture);
        let before = rev(&remote, "master");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        // The approval's environment breaks the install.
        let mut failing = caller_env();
        failing.insert("INSTALL_FAIL".into(), "1".into());
        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                env: Some(failing),
                ..resume_params(&run.run_id, review.event_id, TodoAction::Approve)
            })
            .unwrap();
        let (failed, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(failed.kind, TodoEventKind::InstallFailed, "{failed:#?}");
        assert_eq!(
            failed.actions,
            [
                TodoAction::RetryInstall,
                TodoAction::SkipInstall,
                TodoAction::Abort
            ]
        );
        let error = failed.error.clone().unwrap_or_default();
        assert!(
            error.contains("install broke") && error.contains("exited 1"),
            "{error}"
        );
        assert_eq!(
            (waiting.status, waiting.step, waiting.installed_build),
            (TodoRunStatus::Waiting, TodoStep::Install, None)
        );
        // Nothing after the install happened.
        assert_eq!(rev(&remote, "master"), before);
        assert_eq!(master_subjects(&fixture), [SUBJECT, "finish setup", "init"]);

        resume(
            &fixture,
            &run.run_id,
            failed.event_id,
            TodoAction::RetryInstall,
            None,
        )
        .unwrap();
        let (done, finished) = wait(&fixture, &run.run_id, Some(failed.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(finished.installed_build, finished.picked);
        assert_eq!(
            event_types(&fixture, &run.run_id)
                .iter()
                .filter(|kind| *kind == "run_install_intent")
                .count(),
            2
        );
        // No note: the TODO is left alone, master is pushed.
        assert_eq!(finished.todo_commit, None);
        assert_eq!(rev(&remote, "master"), rev(&fixture.repo, "master"));
    }

    #[test]
    fn a_push_that_is_not_a_fast_forward_is_refused() {
        let fixture = todo_repo("todo-push-refused");
        let remote = with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        // Another clone pushes to origin meanwhile.
        git_in(
            &fixture.root,
            &["clone", "-q", &remote.display().to_string(), "other"],
        );
        let other = fixture.root.join("other");
        std::fs::write(other.join("elsewhere.md"), "x\n").unwrap();
        git_in(&other, &["add", "."]);
        git_in(&other, &["commit", "-q", "-m", "docs: elsewhere"]);
        git_in(&other, &["push", "-q", "origin", "master"]);
        let theirs = rev(&remote, "master");

        let (review, _) = wait(&fixture, &run.run_id, None);
        approve_with(&fixture, &run.run_id, review.event_id, None, None);
        let (failed, waiting) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(failed.kind, TodoEventKind::PushFailed, "{failed:#?}");
        assert_eq!(failed.actions, [TodoAction::RetryPush, TodoAction::Abort]);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("not be a fast-forward"),
            "{failed:#?}"
        );
        assert_eq!(waiting.step, TodoStep::Push);
        assert_eq!(rev(&remote, "master"), theirs);
        assert!(event_types(&fixture, &run.run_id)
            .iter()
            .all(|kind| kind != "run_push_intent"));

        // A retry refuses again; an abort ends the run, the commit on master.
        resume(
            &fixture,
            &run.run_id,
            failed.event_id,
            TodoAction::RetryPush,
            None,
        )
        .unwrap();
        let (again, _) = wait(&fixture, &run.run_id, Some(failed.event_id));
        assert_eq!(again.kind, TodoEventKind::PushFailed, "{again:#?}");
        let (aborted, ended) = abort(&fixture, &run.run_id, again.event_id);
        assert!(
            aborted
                .error
                .as_deref()
                .unwrap_or_default()
                .starts_with("the coordinator aborted the run at its push step: superseded"),
            "{aborted:#?}"
        );
        let picked = ended.picked.clone().unwrap();
        assert!(
            aborted
                .error
                .as_deref()
                .unwrap_or_default()
                .contains(&format!("commit {picked} stays on master")),
            "{aborted:#?}"
        );
        assert_eq!(rev(&remote, "master"), theirs);
        assert_eq!(master_subjects(&fixture)[0], SUBJECT);
    }

    #[test]
    fn a_run_blocked_at_its_start_is_aborted_and_a_stale_event_is_refused() {
        let fixture = todo_repo("todo-abort-blocked");
        let repo = repository_of(&fixture.repo).unwrap();
        // The driver ends before the start; then the folder slot cannot be
        // made (a file is in its place), so the next driver blocks there.
        runs::crash_before(&repo, TodoStep::Start);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        runs::wait_crashed(&repo, HANG_GUARD);
        let slot = fixture.root.join("herdr-worktrees/worker");
        std::fs::create_dir_all(slot.parent().unwrap()).unwrap();
        std::fs::write(&slot, "not a directory\n").unwrap();
        fixture.supervisor.resume_runs();
        let (blocked, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(blocked.kind, TodoEventKind::Blocked, "{blocked:#?}");
        assert_eq!(
            (waiting.status, waiting.step, waiting.worker_id.clone()),
            (TodoRunStatus::Blocked, TodoStep::Start, None)
        );
        // A blocked run's last event takes only an abort.
        assert_eq!(blocked.actions, [TodoAction::Abort]);
        let refused = resume(
            &fixture,
            &run.run_id,
            blocked.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");
        let stale = resume(
            &fixture,
            &run.run_id,
            blocked.event_id - 1,
            TodoAction::Abort,
            None,
        )
        .unwrap_err();
        assert_eq!(stale.code(), "todo_event_stale", "{stale}");
        assert_eq!(
            fixture.supervisor.todo_status(&run.run_id).unwrap().status,
            TodoRunStatus::Blocked
        );

        let (aborted, _) = abort(&fixture, &run.run_id, blocked.event_id);
        let error = aborted.error.unwrap_or_default();
        assert!(
            error.starts_with(
                "the coordinator aborted the run blocked at its start step: superseded"
            ),
            "{error}"
        );
        assert!(error.contains("the run has no branch left"), "{error}");
        assert!(aborted.commits.is_empty());
        std::fs::remove_file(slot).unwrap();
    }

    #[test]
    fn a_run_waiting_on_its_review_is_aborted_after_its_worker_exits() {
        let fixture = todo_repo("todo-abort-review");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        let worker_id = waiting.worker_id.clone().unwrap();
        assert_ne!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );
        let (aborted, _) = abort(&fixture, &run.run_id, review.event_id);
        // The worker was stopped and its exit seen before the run ended.
        assert_eq!(
            fixture.supervisor.status(&worker_id).unwrap().state,
            WorkerState::Exited
        );
        let types = event_types(&fixture, &run.run_id);
        let at = |kind: &str| types.iter().rposition(|seen| seen == kind).unwrap();
        assert!(at("run_stop_intent") < at("run_event"), "{types:?}");
        assert!(fixture
            .supervisor
            .obligations(Some("p-coordinator"))
            .is_empty());
        // The branch and its commit stay; master has nothing of it.
        let branch = format!("todo/{ITEM}-{}-1", run.run_id);
        let error = aborted.error.clone().unwrap_or_default();
        assert!(
            error.starts_with("the coordinator aborted the run at its review step: superseded"),
            "{error}"
        );
        assert!(
            error.contains(&format!("branches left as they are: {branch} at "))
                && error.contains("(1 commit since the base)"),
            "{error}"
        );
        assert_eq!(aborted.commits, review.commits);
        assert_eq!(
            rev(&fixture.repo, &branch),
            *aborted.commits.last().unwrap()
        );
        assert_eq!(master_subjects(&fixture), ["init"]);
        // The repository is free for the next run.
        let store = fixture.supervisor.shared.store.as_ref().unwrap();
        assert!(store.active_run(&run.repo).unwrap().is_none());
    }

    #[test]
    fn a_run_cut_off_after_its_push_does_not_push_again() {
        let fixture = todo_repo("todo-push-crash");
        let remote = with_finish(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        // The server ends right after the push, before recording it.
        runs::crash_after(&run.repo, TodoStep::Push);
        approve_with(&fixture, &run.run_id, review.event_id, None, None);
        runs::wait_crashed(&run.repo, HANG_GUARD);
        let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(
            (cut.status, cut.step, cut.pushed),
            (TodoRunStatus::Running, TodoStep::Push, None)
        );
        assert_eq!(rev(&remote, "master"), rev(&fixture.repo, "master"));

        fixture.supervisor.resume_runs();
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        assert_eq!(
            finished.pushed.as_deref(),
            Some(rev(&fixture.repo, "master").as_str())
        );
        assert_eq!(
            run_events(&fixture, &run.run_id, "run_push_intent").len(),
            1
        );
        let pushed = run_events(&fixture, &run.run_id, "run_pushed");
        assert_eq!(pushed.len(), 1, "{pushed:?}");
        assert_eq!(pushed[0]["already"], true);
    }

    #[test]
    fn a_closed_item_moves_to_decisions_and_a_later_run_deletes_the_kept_branch() {
        let fixture = todo_repo("todo-close");
        let remote = with_finish(&fixture);
        let first = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &first.run_id, None);
        approve_with(
            &fixture,
            &first.run_id,
            review.event_id,
            None,
            Some("## Driven items close (2026-10-08)\n\n- Chosen: the driver.\n"),
        );
        let (done, finished) = wait(&fixture, &first.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        assert_eq!(master_subjects(&fixture)[0], "docs(todo): close t-abcd2345");
        let todo = std::fs::read_to_string(fixture.repo.join("TODO.md")).unwrap();
        assert!(!todo.contains(ITEM), "{todo}");
        assert!(todo.contains(ITEM2), "{todo}");
        let decisions = std::fs::read_to_string(fixture.repo.join("DECISIONS.md")).unwrap();
        assert!(
            decisions.ends_with(
                "Intro.\n\n## Driven items close (2026-10-08)\n\n- Chosen: the driver.\n"
            ),
            "{decisions}"
        );
        let first_branch = format!("todo/t-abcd2345-{}-1", first.run_id);
        assert_eq!(finished.kept_branches, std::slice::from_ref(&first_branch));

        // The next run detaches the slot from the branch the first one kept
        // and deletes it before its worker starts.
        let second = fixture
            .supervisor
            .todo_run(TodoRunParams {
                item: ITEM2.into(),
                message: "feat: add b".into(),
                ..params(&fixture, "commit b.txt feat: add b", "ok")
            })
            .unwrap();
        let (review, _) = wait(&fixture, &second.run_id, None);
        approve_with(&fixture, &second.run_id, review.event_id, None, None);
        let (done, finished) = wait(&fixture, &second.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        let second_branch = format!("todo/t-bcde3456-{}-1", second.run_id);
        let branches = git_in(&fixture.repo, &["branch", "--format=%(refname:short)"]);
        assert!(!branches.contains(&first_branch), "{branches}");
        assert!(branches.contains(&second_branch), "{branches}");
        assert_eq!(finished.kept_branches, [second_branch]);
        assert!(fixture
            .supervisor
            .todo_status(&first.run_id)
            .unwrap()
            .kept_branches
            .is_empty());
        let released = run_events(&fixture, &second.run_id, "run_slot_released");
        assert_eq!(released.len(), 1, "{released:?}");
        assert_eq!(released[0]["detached"], serde_json::json!(first_branch));
        assert_eq!(released[0]["deleted"], serde_json::json!([first_branch]));
        let cleaned = run_events(&fixture, &second.run_id, "run_cleaned");
        assert_eq!(cleaned[0]["deleted"], serde_json::json!([]));
        assert_eq!(rev(&remote, "master"), rev(&fixture.repo, "master"));
    }

    /// The repository as runs record it.
    fn repo_of(fixture: &Fixture) -> String {
        crate::workers::repository_of(&fixture.repo).unwrap()
    }

    fn history_kinds(item: &crate::api::schema::HistoryItem) -> Vec<HistoryEventKind> {
        item.events.iter().map(|event| event.kind).collect()
    }

    #[test]
    fn an_items_history_records_its_claim_close_and_follow_ups_and_reconcile_reports_gaps() {
        let fixture = todo_repo("history-close");
        with_finish(&fixture);
        let repo = repo_of(&fixture);
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &run.run_id, None);
        // The coordinator files a follow-up while the run works.
        let todo = fixture.repo.join("TODO.md");
        let mut text = std::fs::read_to_string(&todo).unwrap();
        text.push_str("\n- [ ] A follow-up [t-cdef4567]\n");
        std::fs::write(&todo, text).unwrap();
        git_in(
            &fixture.repo,
            &[
                "commit",
                "-q",
                "-m",
                "docs(todo): follow-up",
                "--",
                "TODO.md",
            ],
        );
        let decision = "## Closed by the driver\n\n- Chosen.\n";
        approve_with(&fixture, &run.run_id, review.event_id, None, Some(decision));
        let (done, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");

        let item = fixture.supervisor.history_item(ITEM, Some(&repo)).unwrap();
        assert_eq!(
            history_kinds(&item),
            [
                HistoryEventKind::Claimed,
                HistoryEventKind::Closed,
                HistoryEventKind::Stopped
            ]
        );
        assert_eq!(item.title.as_deref(), Some("The driven item"));
        let item_text = format!("- [ ] The driven item [{ITEM}]\n  Its text.\n");
        let (claimed, closed) = (&item.events[0], &item.events[1]);
        assert_eq!(claimed.item_text.as_deref(), Some(item_text.as_str()));
        assert_eq!(claimed.run_id.as_deref(), Some(run.run_id.as_str()));
        assert_eq!(closed.text.as_deref(), Some(decision));
        assert_eq!(closed.item_text.as_deref(), Some(item_text.as_str()));
        assert_eq!(closed.follow_ups, ["t-cdef4567"]);
        assert_eq!(item.runs.len(), 1, "{item:#?}");
        let recorded = &item.runs[0];
        assert_eq!(
            (recorded.status, &recorded.todo_commit),
            (TodoRunStatus::Done, &finished.todo_commit)
        );
        let attempt = &recorded.attempts[0];
        assert_eq!(
            (
                attempt.verdict,
                attempt.decision.as_deref(),
                attempt.landed_sha.as_ref()
            ),
            (
                Some(WorkerVerdict::Verified),
                Some("approve"),
                finished.picked.as_ref()
            )
        );
        let list = fixture.supervisor.history_list(Some(&repo)).unwrap();
        assert_eq!(list.len(), 1, "{list:#?}");
        assert_eq!(
            (list[0].item.as_str(), list[0].last.kind),
            (ITEM, HistoryEventKind::Closed)
        );
        assert_eq!(list[0].title.as_deref(), Some("The driven item"));
        let clean = fixture.supervisor.history_reconcile(&repo).unwrap();
        assert!(clean.open_claims.is_empty(), "{clean:#?}");
        assert!(clean.deleted_without_close.is_empty(), "{clean:#?}");

        // An item seen at the claim that leaves TODO.md by hand is reported.
        let text = std::fs::read_to_string(&todo).unwrap();
        let line = format!("- [ ] The second item [{ITEM2}]\n");
        assert!(text.contains(&line), "{text}");
        std::fs::write(&todo, text.replace(&line, "")).unwrap();
        let gaps = fixture.supervisor.history_reconcile(&repo).unwrap();
        assert!(gaps.open_claims.is_empty(), "{gaps:#?}");
        assert_eq!(gaps.deleted_without_close.len(), 1, "{gaps:#?}");
        assert_eq!(gaps.deleted_without_close[0].item, ITEM2);
        // The reconcile reports; it writes nothing.
        assert_eq!(
            fixture
                .supervisor
                .history_item(ITEM, Some(&repo))
                .unwrap()
                .events,
            item.events
        );
        let unknown = fixture
            .supervisor
            .history_item("t-zzzzzzzz", Some(&repo))
            .unwrap_err();
        assert_eq!(unknown.code(), "history_item_not_found", "{unknown}");
    }

    #[test]
    fn notes_aborts_and_runs_in_progress_are_in_the_items_history() {
        let fixture = todo_repo("history-note");
        with_finish(&fixture);
        let repo = repo_of(&fixture);
        let first = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(&fixture, &first.run_id, None);
        approve_with(
            &fixture,
            &first.run_id,
            review.event_id,
            Some("Done by the driver.\n"),
            None,
        );
        let (done, _) = wait(&fixture, &first.run_id, Some(review.event_id));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");

        let second = fixture
            .supervisor
            .todo_run(TodoRunParams {
                message: "feat: add b".into(),
                ..params(&fixture, "commit b.txt feat: add b", "ok")
            })
            .unwrap();
        let (review, _) = wait(&fixture, &second.run_id, None);
        abort(&fixture, &second.run_id, review.event_id);

        let third = fixture
            .supervisor
            .todo_run(TodoRunParams {
                message: "feat: add c".into(),
                ..params(&fixture, "commit c.txt feat: add c", "ok")
            })
            .unwrap();
        let (review, _) = wait(&fixture, &third.run_id, None);
        let reconcile = fixture.supervisor.history_reconcile(&repo).unwrap();
        assert_eq!(reconcile.open_claims.len(), 1, "{reconcile:#?}");
        assert_eq!(
            (
                reconcile.open_claims[0].run_id.as_str(),
                reconcile.open_claims[0].run_status
            ),
            (third.run_id.as_str(), Some(TodoRunStatus::Waiting))
        );

        let item = fixture.supervisor.history_item(ITEM, Some(&repo)).unwrap();
        assert_eq!(
            history_kinds(&item),
            [
                HistoryEventKind::Claimed,
                HistoryEventKind::Noted,
                HistoryEventKind::Claimed,
                HistoryEventKind::Aborted,
                HistoryEventKind::Claimed
            ]
        );
        assert_eq!(
            item.events[1].text.as_deref(),
            Some("Done by the driver.\n")
        );
        let aborted = item.events[3].text.as_deref().unwrap_or_default();
        assert!(aborted.contains("superseded"), "{aborted}");
        // The note's run is the claim's: its run is ended by it.
        assert_eq!(item.events[1].run_id, item.events[0].run_id);
        let statuses: Vec<_> = item.runs.iter().map(|run| run.status).collect();
        assert_eq!(
            statuses,
            [
                TodoRunStatus::Done,
                TodoRunStatus::Aborted,
                TodoRunStatus::Waiting
            ]
        );
        abort(&fixture, &third.run_id, review.event_id);
        assert!(fixture
            .supervisor
            .history_reconcile(&repo)
            .unwrap()
            .open_claims
            .is_empty());
    }

    #[test]
    fn an_attempts_review_joins_its_task_reply_diff_verify_and_approval() {
        let fixture = todo_repo("todo-review");
        let run = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, waiting) = wait(&fixture, &run.run_id, None);
        assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
        // Before the decision: the branch's commit, no verify, no approval.
        let before = fixture
            .supervisor
            .todo_review(&run.run_id, None, false)
            .unwrap();
        assert_eq!((before.attempt, before.attempts), (1, 1));
        assert_eq!(before.status, TodoRunStatus::Waiting);
        assert_eq!(before.worker_id, waiting.worker_id);
        // The task as the worker got it, the run's contract appended.
        assert!(
            before
                .task
                .starts_with(&format!("commit a.txt {SUBJECT}\n")),
            "{}",
            before.task
        );
        assert!(
            before.task.contains("no body and no trailers"),
            "{}",
            before.task
        );
        assert!(
            before
                .final_message
                .as_deref()
                .unwrap_or_default()
                .contains("WORKER-DONE"),
            "{before:#?}"
        );
        assert_eq!(before.base, waiting.base);
        assert_eq!(before.commit.as_ref(), review.commits.last());
        assert!(
            before
                .diff_stat
                .as_deref()
                .unwrap_or_default()
                .contains("a.txt"),
            "{before:#?}"
        );
        assert!(before.diff.is_none(), "{before:#?}");
        assert!(before.questions.is_empty() && before.tool_failures.is_empty());
        assert!(before.verification.is_none() && before.decision.is_none());
        assert!(before.approved.is_none() && before.landed_sha.is_none());

        resume(
            &fixture,
            &run.run_id,
            review.event_id,
            TodoAction::Approve,
            None,
        )
        .unwrap();
        let (_, finished) = wait(&fixture, &run.run_id, Some(review.event_id));
        let after = fixture
            .supervisor
            .todo_review(&run.run_id, Some(1), true)
            .unwrap();
        let verification = after.verification.clone().expect("verified");
        assert_eq!(verification.verdict, WorkerVerdict::Verified);
        assert!(
            verification
                .checks
                .iter()
                .any(|check| check.name.as_deref() == Some("ok")
                    && check.outcome == WorkerCheckOutcome::Passed),
            "{verification:#?}"
        );
        assert_eq!(after.decision.as_deref(), Some("approve"));
        assert_eq!(
            after.approved,
            Some(crate::api::schema::TodoApproval {
                commit: review.commits.last().cloned().unwrap(),
                base: waiting.base.clone().unwrap(),
            })
        );
        assert_eq!(after.landed_sha, finished.picked);
        assert!(
            after
                .diff
                .as_deref()
                .unwrap_or_default()
                .contains("+change"),
            "{after:#?}"
        );
        // The JSON reply carries the parts by name.
        let reply = serde_json::to_value(crate::api::schema::ResponseResult::TodoReview {
            review: Box::new(after),
        })
        .unwrap();
        assert_eq!(reply["type"], "todo_review");
        assert_eq!(reply["review"]["approved"]["base"], waiting.base.unwrap());
        assert_eq!(reply["review"]["verification"]["verdict"], "verified");

        let missing = fixture
            .supervisor
            .todo_review(&run.run_id, Some(2), false)
            .unwrap_err();
        assert_eq!(missing.code(), "todo_run_not_found", "{missing}");
    }

    #[test]
    fn two_runs_of_the_same_item_in_a_row_both_start() {
        let fixture = todo_repo("todo-same-item");
        let first = fixture
            .supervisor
            .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (done, finished) = approve_to_done(&fixture, &first.run_id);
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
        let first_branch = format!("todo/{ITEM}-{}-1", first.run_id);
        assert_eq!(finished.kept_branches, std::slice::from_ref(&first_branch));

        // The slot is still on the first run's branch; the second run of
        // the same item starts on its own.
        let second = fixture
            .supervisor
            .todo_run(TodoRunParams {
                message: "feat: add b".into(),
                ..params(&fixture, "commit b.txt feat: add b", "ok")
            })
            .unwrap();
        let second_branch = format!("todo/{ITEM}-{}-1", second.run_id);
        assert_eq!(second.branch.as_deref(), Some(second_branch.as_str()));
        let (done, finished) = approve_to_done(&fixture, &second.run_id);
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        assert_eq!(master_subjects(&fixture), ["feat: add b", SUBJECT, "init"]);
        let branches = git_in(&fixture.repo, &["branch", "--format=%(refname:short)"]);
        assert!(!branches.contains(&first_branch), "{branches}");
        assert_eq!(finished.kept_branches, [second_branch]);
    }

    /// A run with `origin` whose server ends right before the push, after
    /// an approval whose environment holds a secret. Returns the run, the
    /// secret, the bare repository and the approved event.
    fn run_cut_off_before_its_push(fixture: &Fixture) -> (TodoRunInfo, String, PathBuf, i64) {
        let remote = with_finish(fixture);
        let run = fixture
            .supervisor
            .todo_run(params(fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
            .unwrap();
        let (review, _) = wait(fixture, &run.run_id, None);
        let secret = format!("run-env-secret-{}", run.run_id);
        let mut env = caller_env();
        env.insert("RUN_ENV_SECRET".into(), secret.clone());
        runs::crash_before(&run.repo, TodoStep::Push);
        fixture
            .supervisor
            .todo_resume(TodoResumeParams {
                env: Some(env),
                ..resume_params(&run.run_id, review.event_id, TodoAction::Approve)
            })
            .unwrap();
        runs::wait_crashed(&run.repo, HANG_GUARD);
        let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
        assert_eq!(
            (cut.status, cut.step, cut.pushed.clone()),
            (TodoRunStatus::Running, TodoStep::Push, None)
        );
        (cut, secret, remote, review.event_id)
    }

    /// Every file under `dir`, recursively, that holds `needle`.
    fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut dirs = vec![dir.to_owned()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if std::fs::read(&path)
                    .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                {
                    found.push(path);
                }
            }
        }
        found
    }

    #[test]
    fn a_handoff_carries_a_runs_environment_to_its_push() {
        let fixture = todo_repo("todo-handoff-env");
        let (run, secret, remote, approved) = run_cut_off_before_its_push(&fixture);
        // The old server puts the environments in the handoff's message;
        // the new process starts without them and takes them from it.
        let handed = runs::envs_for_handoff();
        assert_eq!(
            handed
                .get(&run.run_id)
                .and_then(|env| env.get("RUN_ENV_SECRET")),
            Some(&secret)
        );
        let message = serde_json::to_string(&handed).unwrap();
        runs::forget_env(&run.run_id);
        runs::restore_handed_off_envs(serde_json::from_str(&message).unwrap());
        fixture.supervisor.resume_runs();
        let (done, finished) = wait(&fixture, &run.run_id, Some(approved));
        assert_eq!(done.kind, TodoEventKind::Done, "{done:#?} {finished:#?}");
        assert_eq!(
            finished.pushed.as_deref(),
            Some(rev(&remote, "master").as_str())
        );
        // Neither the store nor any file beside it holds the environment.
        let leaked = files_holding(&fixture.supervisor.shared.dir, &secret);
        assert!(leaked.is_empty(), "{leaked:?}");
    }

    #[test]
    fn after_a_restart_a_run_has_no_environment_for_its_push() {
        let fixture = todo_repo("todo-restart-env");
        let (run, secret, remote, approved) = run_cut_off_before_its_push(&fixture);
        let before = rev(&remote, "master");
        // A cold restart: nothing hands the environment over.
        runs::forget_env(&run.run_id);
        fixture.supervisor.resume_runs();
        let (failed, waiting) = wait(&fixture, &run.run_id, Some(approved));
        assert_eq!(failed.kind, TodoEventKind::PushFailed, "{failed:#?}");
        assert_eq!(waiting.step, TodoStep::Push);
        assert_eq!(rev(&remote, "master"), before);
        let leaked = files_holding(&fixture.supervisor.shared.dir, &secret);
        assert!(leaked.is_empty(), "{leaked:?}");
        abort(&fixture, &run.run_id, failed.event_id);
    }

    /// `todo run --auto-review` against a stub model: `review-stub` in the
    /// fixture's root logs each call (its argv and input) to
    /// `review-calls.jsonl` and answers with the first line of
    /// `review-answers`, which it removes: a JSON decision (sent as the
    /// structured output, from the model `stub-review-model`), `raw <text>`
    /// (printed as it is), `exit <code>` (a failed call), or `gate <entered>
    /// <gate> <decision>` (opens the FIFO `entered` for writing, then waits
    /// for a line on the FIFO `gate`, then answers with the decision).
    mod auto_review {
        use super::*;

        const REVIEW_STUB: &str = r#"#!/usr/bin/env python3
import json, os, sys
here = os.path.dirname(os.path.abspath(__file__))
answers = os.path.join(here, "review-answers")
prompt = sys.stdin.read()
with open(os.path.join(here, "review-calls.jsonl"), "a") as log:
    log.write(json.dumps({"argv": sys.argv[1:], "stdin": prompt}) + "\n")
lines = open(answers).read().splitlines() if os.path.exists(answers) else []
if not lines:
    sys.stderr.write("no answer left\n")
    sys.exit(3)
with open(answers + ".tmp", "w") as rest:
    rest.write("".join(line + "\n" for line in lines[1:]))
os.replace(answers + ".tmp", answers)
answer = lines[0]
if answer.startswith("exit "):
    sys.stderr.write("stub failure\n")
    sys.exit(int(answer[5:]))
if answer.startswith("raw "):
    print(answer[4:])
    sys.exit(0)
if answer.startswith("gate "):
    _, entered, gate, answer = answer.split(" ", 3)
    with open(entered, "w") as signal:
        signal.write("in")
    with open(gate) as wait:
        wait.read()
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "result": "",
                  "structured_output": json.loads(answer),
                  "modelUsage": {"stub-review-model": {}}}))
"#;

        /// The fixture with the review stub and its answers.
        fn reviewed(name: &str, answers: &[&str]) -> Fixture {
            use std::os::unix::fs::PermissionsExt;
            let fixture = todo_repo(name);
            let stub = fixture.root.join("review-stub");
            std::fs::write(&stub, REVIEW_STUB).unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
            set_answers(&fixture, answers);
            fixture.supervisor.set_review_program_for_test(stub);
            fixture
        }

        fn set_answers(fixture: &Fixture, answers: &[&str]) {
            let text: String = answers.iter().map(|line| format!("{line}\n")).collect();
            std::fs::write(fixture.root.join("review-answers"), text).unwrap();
        }

        fn calls(fixture: &Fixture) -> Vec<serde_json::Value> {
            std::fs::read_to_string(fixture.root.join("review-calls.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }

        fn start(fixture: &Fixture) -> TodoRunInfo {
            let run = fixture
                .supervisor
                .todo_run(TodoRunParams {
                    auto_review: true,
                    ..params(fixture, &format!("commit a.txt {SUBJECT}"), "ok")
                })
                .unwrap();
            assert!(run.auto_review, "{run:?}");
            run
        }

        /// The `review` event the attention step recorded, which the
        /// server reviews itself and `todo.wait` does not return.
        fn reviewed_event(fixture: &Fixture, run_id: &str) -> i64 {
            let created = run_events(fixture, run_id, "run_review_decision");
            created[0]["event"].as_i64().unwrap()
        }

        #[test]
        fn an_approval_is_bound_to_the_commit_and_lands_it() {
            let fixture = reviewed(
                "todo-auto-approve",
                &[r#"{"action": "approve", "note": "does what the item asks"}"#],
            );
            let run = start(&fixture);
            let (done, finished) = wait(&fixture, &run.run_id, None);
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
            assert!(finished.auto_review);

            let decisions = run_events(&fixture, &run.run_id, "run_review_decision");
            assert_eq!(decisions.len(), 1, "{decisions:#?}");
            let decision = &decisions[0];
            assert!(decision["decision_id"].as_str().unwrap().starts_with("d-"));
            assert_eq!(decision["model"], "stub-review-model");
            assert_eq!(decision["input_digest"].as_str().unwrap().len(), 64);
            assert_eq!(
                decision["output"],
                serde_json::json!({"action": "approve", "note": "does what the item asks"})
            );
            let applied = run_events(&fixture, &run.run_id, "run_auto_reviewed");
            assert_eq!(applied[0]["action"], "approve");
            assert_eq!(applied[0]["commit"], decision["commit"]);
            assert_eq!(applied[0]["base"].as_str(), finished.base.as_deref());

            // One stateless call without tools, with the item, the task,
            // the worker's reply and the diff on its input.
            let calls = calls(&fixture);
            assert_eq!(calls.len(), 1, "{calls:#?}");
            let argv: Vec<&str> = calls[0]["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap())
                .collect();
            for flag in ["-p", "--json-schema", "--no-session-persistence"] {
                assert!(argv.contains(&flag), "{flag} not in {argv:?}");
            }
            let tools = argv.iter().position(|arg| *arg == "--tools").unwrap();
            assert_eq!(argv[tools + 1], "");
            let input = calls[0]["stdin"].as_str().unwrap();
            for part in [
                "The driven item",
                "commit a.txt",
                "WORKER-DONE",
                "+++ b/a.txt",
            ] {
                assert!(input.contains(part), "{part:?} not in {input}");
            }
        }

        #[test]
        fn a_retry_starts_the_next_attempt_with_its_review() {
            let fixture = reviewed(
                "todo-auto-retry",
                &[
                    // The stub worker's next command.
                    r#"{"action": "retry", "review": "amend a.txt feat: add a"}"#,
                    r#"{"action": "escalate", "question": "Keep both files?", "options": ["keep", "drop b.txt"]}"#,
                ],
            );
            let run = start(&fixture);
            let (escalated, waiting) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Review, "{escalated:#?}");
            assert_eq!(waiting.attempt, 2);
            assert!(
                waiting
                    .task
                    .ends_with("Review of attempt 1:\namend a.txt feat: add a"),
                "{}",
                waiting.task
            );
            let applied = run_events(&fixture, &run.run_id, "run_auto_reviewed");
            assert_eq!(applied[0]["action"], "retry");
            assert_eq!(applied[0]["task"], "amend a.txt feat: add a");
            assert_eq!(calls(&fixture).len(), 2);
            abort(&fixture, &run.run_id, escalated.event_id);
        }

        #[test]
        fn an_escalation_is_a_new_review_event_for_the_user() {
            let fixture = reviewed(
                "todo-auto-escalate",
                &[
                    r#"{"action": "escalate", "question": "Is a.txt the right file?", "options": ["yes", "no, use b.txt"]}"#,
                ],
            );
            let _ = crate::workers::take_user_notices();
            let run = start(&fixture);
            let (escalated, waiting) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Review, "{escalated:#?}");
            let reviewed = reviewed_event(&fixture, &run.run_id);
            assert_ne!(escalated.event_id, reviewed);
            assert_eq!(waiting.pending_event, Some(escalated.event_id));
            let error = escalated.error.clone().unwrap_or_default();
            for part in ["Is a.txt the right file?", "Options: yes | no, use b.txt"] {
                assert!(error.contains(part), "{part:?} not in {error}");
            }
            assert_eq!(escalated.commits.len(), 1, "{escalated:#?}");
            let notices = crate::workers::take_user_notices();
            assert_eq!(notices.len(), 1, "{notices:?}");
            assert!(notices[0].body.contains("Is a.txt the right file?"));
            // The reviewed event is stale; the escalated one is the
            // coordinator's, and it is not reviewed again.
            let stale =
                resume(&fixture, &run.run_id, reviewed, TodoAction::Approve, None).unwrap_err();
            assert_eq!(stale.code(), "todo_event_stale", "{stale}");
            resume(
                &fixture,
                &run.run_id,
                escalated.event_id,
                TodoAction::Approve,
                None,
            )
            .unwrap();
            let (done, _) = wait(&fixture, &run.run_id, Some(escalated.event_id));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(calls(&fixture).len(), 1);
        }

        #[test]
        fn an_invalid_output_is_asked_again_once_then_escalated() {
            let fixture = reviewed(
                "todo-auto-invalid",
                &[
                    "raw this is not json",
                    r#"{"action": "approve", "review": "approve takes no review"}"#,
                ],
            );
            let run = start(&fixture);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Review, "{escalated:#?}");
            let error = escalated.error.clone().unwrap_or_default();
            assert!(error.contains("failed 2 times"), "{error}");
            let calls_made = run_events(&fixture, &run.run_id, "run_review_call");
            assert_eq!(calls_made.len(), 2, "{calls_made:#?}");
            assert!(calls_made.iter().all(|call| call["error"].is_string()));
            let decision = &run_events(&fixture, &run.run_id, "run_review_decision")[0];
            assert!(decision["output"].is_null(), "{decision:#?}");
            assert_eq!(decision["errors"].as_array().unwrap().len(), 2);
            abort(&fixture, &run.run_id, escalated.event_id);
        }

        #[test]
        fn a_failed_call_is_made_once_more() {
            let fixture = reviewed("todo-auto-failed", &["exit 1", r#"{"action": "approve"}"#]);
            let run = start(&fixture);
            let (done, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            let calls_made = run_events(&fixture, &run.run_id, "run_review_call");
            assert_eq!(calls_made.len(), 2, "{calls_made:#?}");
            assert!(calls_made[0]["error"]
                .as_str()
                .unwrap()
                .contains("stub failure"));
            assert_eq!(calls_made[1]["output"]["action"], "approve");
        }

        #[test]
        fn a_decision_recorded_before_a_crash_is_applied_without_asking_again() {
            let fixture = reviewed("todo-auto-crash", &[r#"{"action": "approve"}"#]);
            let repo = repository_of(&fixture.repo).unwrap();
            runs::crash_after(&repo, TodoStep::Review);
            let run = start(&fixture);
            runs::wait_crashed(&repo, HANG_GUARD);
            let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
            let reviewed = reviewed_event(&fixture, &run.run_id);
            assert_eq!(
                (cut.status, cut.step, cut.pending_event),
                (TodoRunStatus::Waiting, TodoStep::Review, Some(reviewed))
            );
            assert!(run_events(&fixture, &run.run_id, "run_auto_reviewed").is_empty());
            // A model asked again would fail: no answer is left.
            fixture.supervisor.resume_runs();
            let (done, _) = wait(&fixture, &run.run_id, Some(reviewed));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(calls(&fixture).len(), 1);
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_review_decision").len(),
                1
            );
            assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        }

        #[test]
        fn a_coordinators_answer_that_comes_first_wins() {
            let fixture = reviewed("todo-auto-override", &[]);
            let entered = fixture.root.join("entered.fifo");
            let gate = fixture.root.join("gate.fifo");
            fifo(&entered);
            fifo(&gate);
            set_answers(
                &fixture,
                &[&format!(
                    "gate {} {} {}",
                    entered.display(),
                    gate.display(),
                    r#"{"action": "retry", "review": "too late"}"#
                )],
            );
            let run = start(&fixture);
            // The stub is in its call once it opened `entered`.
            assert_eq!(std::fs::read_to_string(&entered).unwrap(), "in");
            let waiting = fixture.supervisor.todo_status(&run.run_id).unwrap();
            let reviewed = waiting.pending_event.unwrap();
            assert_eq!(waiting.step, TodoStep::Review);
            resume(&fixture, &run.run_id, reviewed, TodoAction::Approve, None).unwrap();
            std::fs::write(&gate, "go\n").unwrap();
            let (done, finished) = wait(&fixture, &run.run_id, Some(reviewed));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(finished.attempt, 1);
            let superseded = run_events(&fixture, &run.run_id, "run_review_superseded");
            assert_eq!(superseded.len(), 1, "{superseded:#?}");
            assert_eq!(superseded[0]["event"], reviewed);
            assert!(run_events(&fixture, &run.run_id, "run_auto_reviewed").is_empty());
        }

        #[test]
        fn the_review_judges_the_verify_and_its_approval_is_not_verified_again() {
            let fixture = reviewed("todo-auto-verify-first", &[r#"{"action": "approve"}"#]);
            let run = start(&fixture);
            let (done, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            // The verify ran once, before the review, with the worker
            // stopped first.
            let verified = run_events(&fixture, &run.run_id, "run_review_verified");
            assert_eq!(verified.len(), 1, "{verified:#?}");
            assert_eq!(verified[0]["verdict"], "verified");
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_review_stop_intent").len(),
                1
            );
            let input = calls(&fixture)[0]["stdin"].as_str().unwrap().to_owned();
            for part in [
                "verify_before_review",
                "\"verdict\": \"verified\"",
                "\"name\": \"ok\"",
            ] {
                assert!(input.contains(part), "{part:?} not in {input}");
            }
            let decision = &run_events(&fixture, &run.run_id, "run_review_decision")[0];
            assert_eq!(decision["verdict"], "verified");
            // The approval took that verdict: no second verify.
            assert!(run_events(&fixture, &run.run_id, "run_verify_intent").is_empty());
            let landed = run_events(&fixture, &run.run_id, "run_verified");
            assert_eq!(landed[0]["reused"], true, "{landed:#?}");
            assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        }

        #[test]
        fn an_approval_of_a_commit_whose_verify_failed_is_escalated() {
            let fixture = reviewed("todo-auto-verify-failed", &[r#"{"action": "approve"}"#]);
            let run = fixture
                .supervisor
                .todo_run(TodoRunParams {
                    auto_review: true,
                    ..params(&fixture, &format!("commit a.txt {SUBJECT}"), "never")
                })
                .unwrap();
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Review, "{escalated:#?}");
            let error = escalated.error.clone().unwrap_or_default();
            assert!(error.contains("its verify failed"), "{error}");
            assert!(error.contains("never"), "{error}");
            let input = calls(&fixture)[0]["stdin"].as_str().unwrap().to_owned();
            assert!(input.contains("\"verdict\": \"failed\""), "{input}");
            assert_eq!(master_subjects(&fixture), ["init"]);
            abort(&fixture, &run.run_id, escalated.event_id);
        }

        /// The escalation's entry in the user's `?` list.
        fn listed_escalation(fixture: &Fixture, run_id: &str) -> Option<PendingWorkerQuestion> {
            fixture
                .supervisor
                .pending_questions()
                .into_iter()
                .find(|pending| pending.question.text.contains(run_id))
        }

        #[test]
        fn a_review_escalation_enters_the_users_list_and_their_answer_is_reviewed_again() {
            let fixture = reviewed(
                "todo-auto-escalate-list",
                &[
                    r#"{"action": "escalate", "question": "Keep b.txt?", "options": ["keep", "drop it"]}"#,
                    r#"{"action": "approve", "note": "the user chose"}"#,
                ],
            );
            let run = start(&fixture);
            let (escalated, waiting) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Review, "{escalated:#?}");
            let listed = listed_escalation(&fixture, &run.run_id).expect("not in the ? list");
            assert!(!listed.quiet);
            assert_eq!(
                Some(listed.worker_id.as_str()),
                waiting.worker_id.as_deref()
            );
            assert_eq!(listed.question.kind, WorkerQuestionKind::Choice);
            assert!(listed.question.text.contains(ITEM), "{:?}", listed.question);
            assert_eq!(listed.question.questions[0].question, "Keep b.txt?");
            assert_eq!(listed.question.questions[0].options, ["keep", "drop it"]);
            // The answer dialog shows it with its run and item.
            let detail = fixture
                .supervisor
                .question_detail(&listed.worker_id, &listed.question.request_id)
                .unwrap();
            assert!(
                detail.input_text.contains("Keep b.txt?"),
                "{}",
                detail.input_text
            );
            assert!(detail.name.contains(&run.run_id), "{}", detail.name);
            // The dialog answers with the option's number.
            fixture
                .supervisor
                .answer(&WorkerAnswerParams {
                    worker_id: listed.worker_id.clone(),
                    request_id: Some(listed.question.request_id.clone()),
                    decision: Some(WorkerDecision::Allow),
                    answers: vec!["2".into()],
                    message: None,
                    command_id: None,
                })
                .unwrap();
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
            let (done, _) = wait(&fixture, &run.run_id, Some(escalated.event_id));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            let calls = calls(&fixture);
            assert_eq!(calls.len(), 2, "{calls:#?}");
            let again = calls[1]["stdin"].as_str().unwrap();
            for part in ["user_answer", "drop it", "Keep b.txt?"] {
                assert!(again.contains(part), "{part:?} not in {again}");
            }
            // The verify is not run again for the same commit.
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_review_verified").len(),
                1
            );
            assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        }

        #[test]
        fn a_denied_escalation_leaves_the_review_to_the_coordinator() {
            let fixture = reviewed(
                "todo-auto-escalate-deny",
                &[
                    r#"{"action": "escalate", "question": "Keep b.txt?", "options": ["keep", "drop"]}"#,
                ],
            );
            let run = start(&fixture);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            let listed = listed_escalation(&fixture, &run.run_id).expect("not in the ? list");
            fixture
                .supervisor
                .deny_and_stop(&WorkerDenyAndStopParams {
                    worker_id: listed.worker_id.clone(),
                    request_id: listed.question.request_id.clone(),
                    message: Some("the coordinator decides".into()),
                })
                .unwrap();
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
            let declined = run_events(&fixture, &run.run_id, "run_escalation_declined");
            assert_eq!(declined[0]["event"], escalated.event_id);
            // Still the coordinator's event.
            let waiting = fixture.supervisor.todo_status(&run.run_id).unwrap();
            assert_eq!(waiting.pending_event, Some(escalated.event_id));
            let gone = fixture
                .supervisor
                .question_detail(&listed.worker_id, &listed.question.request_id)
                .unwrap_err();
            assert_eq!(gone.code(), "worker_question_gone", "{gone}");
            abort(&fixture, &run.run_id, escalated.event_id);
        }

        #[test]
        fn a_coordinators_answer_takes_the_escalation_off_the_list() {
            let fixture = reviewed(
                "todo-auto-escalate-resume",
                &[
                    r#"{"action": "escalate", "question": "Keep b.txt?", "options": ["keep", "drop"]}"#,
                ],
            );
            let run = start(&fixture);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            assert!(listed_escalation(&fixture, &run.run_id).is_some());
            abort(&fixture, &run.run_id, escalated.event_id);
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
        }

        #[test]
        fn an_escalation_is_listed_again_after_a_restart() {
            let fixture = reviewed(
                "todo-auto-escalate-restart",
                &[
                    r#"{"action": "escalate", "question": "Keep b.txt?", "options": ["keep", "drop"]}"#,
                ],
            );
            let run = start(&fixture);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            let listed = listed_escalation(&fixture, &run.run_id).unwrap();
            // A server that starts has an empty list until it reads the runs.
            runs::escalations::forget_all_for_test();
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
            fixture.supervisor.resume_runs();
            let again = listed_escalation(&fixture, &run.run_id).unwrap();
            assert_eq!(again.question.request_id, listed.question.request_id);
            abort(&fixture, &run.run_id, escalated.event_id);
        }

        #[test]
        fn a_decision_whose_write_fails_shows_the_run_blocked_until_it_lands() {
            let fixture = reviewed("todo-auto-write-fails", &[r#"{"action": "approve"}"#]);
            let repo = repository_of(&fixture.repo).unwrap();
            runs::decision::fail_next_write(&repo, "run_review_decision");
            let run = start(&fixture);
            // The review event the server could not record a decision for
            // is not hidden: the run shows blocked with the error.
            let (blocked, shown) = next_event(&fixture, &run.run_id, None);
            assert_eq!(blocked.kind, TodoEventKind::Blocked, "{blocked:#?}");
            let error = blocked.error.clone().unwrap_or_default();
            assert!(error.contains("injected failure"), "{error}");
            assert_eq!(shown.status, TodoRunStatus::Blocked);
            let status = fixture.supervisor.todo_status(&run.run_id).unwrap();
            assert_eq!(status.status, TodoRunStatus::Blocked);
            assert!(status.error.unwrap().contains("injected failure"));
            // The store takes a write again: the decision lands, without a
            // second call.
            runs::decision::announce_for_test();
            let (done, _) = wait(&fixture, &run.run_id, Some(blocked.event_id));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(calls(&fixture).len(), 1);
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_review_decision").len(),
                1
            );
        }

        #[test]
        fn a_coordinator_still_answers_a_run_whose_decision_write_failed() {
            let fixture = reviewed("todo-auto-write-fails-abort", &[r#"{"action": "approve"}"#]);
            let repo = repository_of(&fixture.repo).unwrap();
            runs::decision::fail_next_write(&repo, "run_review_decision");
            let run = start(&fixture);
            let (blocked, _) = next_event(&fixture, &run.run_id, None);
            assert_eq!(blocked.kind, TodoEventKind::Blocked, "{blocked:#?}");
            let (_, ended) = abort(&fixture, &run.run_id, blocked.event_id);
            assert!(!ended.error.unwrap_or_default().contains("injected"));
            assert_eq!(
                fixture.supervisor.todo_status(&run.run_id).unwrap().status,
                TodoRunStatus::Aborted
            );
        }

        fn start_answering(fixture: &Fixture, task: &str, review: bool) -> TodoRunInfo {
            let run = fixture
                .supervisor
                .todo_run(TodoRunParams {
                    auto_review: review,
                    auto_answer: true,
                    ..params(fixture, task, "ok")
                })
                .unwrap();
            assert!(run.auto_answer, "{run:?}");
            run
        }

        #[test]
        fn an_allowed_request_lets_the_worker_go_on_to_its_commit() {
            let fixture = reviewed(
                "todo-answer-allow",
                &[r#"{"action": "allow"}"#, r#"{"action": "approve"}"#],
            );
            let run = start_answering(&fixture, &format!("perm-commit a.txt {SUBJECT}"), true);
            let (done, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            let decisions = run_events(&fixture, &run.run_id, "run_answer_decision");
            assert_eq!(decisions.len(), 1, "{decisions:#?}");
            assert_eq!(
                decisions[0]["output"],
                serde_json::json!({"action": "allow"})
            );
            assert_eq!(decisions[0]["model"], "stub-review-model");
            let applied = run_events(&fixture, &run.run_id, "run_auto_answered");
            assert_eq!(applied[0]["questions"][0]["answered"], "answered");
            // The call got the task, the request and the policy, and a
            // schema that names the actions this request takes.
            let call = &calls(&fixture)[0];
            let input = call["stdin"].as_str().unwrap();
            for part in [
                "WebFetch",
                "https://example.com",
                "perm-commit",
                "\"policy\"",
            ] {
                assert!(input.contains(part), "{part:?} not in {input}");
            }
            let argv: Vec<&str> = call["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap())
                .collect();
            let schema = argv.iter().position(|arg| *arg == "--json-schema").unwrap();
            let schema: serde_json::Value = serde_json::from_str(argv[schema + 1]).unwrap();
            assert_eq!(
                schema["properties"]["action"]["enum"],
                serde_json::json!(["allow", "deny", "escalate"])
            );
            assert_eq!(master_subjects(&fixture), [SUBJECT, "init"]);
        }

        #[test]
        fn a_denial_reaches_the_worker_with_its_message() {
            let fixture = reviewed(
                "todo-answer-deny",
                &[r#"{"action": "deny", "message": "not needed for the task"}"#],
            );
            let run = start_answering(&fixture, "perm WebFetch https://example.com", false);
            let (review, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
            assert_eq!(
                review.result_text.as_deref(),
                Some("deny: not needed for the task")
            );
            abort(&fixture, &run.run_id, review.event_id);
        }

        #[test]
        fn a_question_is_answered_with_one_answer_per_question() {
            let fixture = reviewed(
                "todo-answer-choice",
                &[r#"{"action": "answer", "answers": ["alpha.txt", "green"]}"#],
            );
            let run = start_answering(&fixture, "ask", false);
            let (review, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
            let text = review.result_text.clone().unwrap_or_default();
            assert!(
                text.contains("alpha.txt") && text.contains("green"),
                "{text}"
            );
            let argv = calls(&fixture)[0]["argv"].to_string();
            assert!(
                argv.contains(r#"[\"answer\",\"deny\",\"escalate\"]"#),
                "{argv}"
            );
            abort(&fixture, &run.run_id, review.event_id);
        }

        #[test]
        fn a_request_the_policy_leaves_to_the_user_is_escalated_without_a_call() {
            let fixture = reviewed("todo-answer-needs-user", &[]);
            let _ = crate::workers::take_user_notices();
            let run = start_answering(&fixture, "classifier WebFetch https://example.com", false);
            let (escalated, waiting) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Question, "{escalated:#?}");
            let error = escalated.error.clone().unwrap_or_default();
            assert!(error.contains("leaves this to you"), "{error}");
            assert!(calls(&fixture).is_empty());
            let decision = &run_events(&fixture, &run.run_id, "run_answer_decision")[0];
            assert!(decision["model"].is_null(), "{decision:#?}");
            assert_eq!(decision["output"]["action"], "escalate");
            // The worker's own question is in the user's list, not quiet,
            // with the run, the item and the question.
            let worker_id = waiting.worker_id.clone().unwrap();
            let listed = fixture
                .supervisor
                .pending_questions()
                .into_iter()
                .find(|pending| pending.worker_id == worker_id)
                .expect("not in the ? list");
            assert!(!listed.quiet);
            let cause = listed.question.escalated.clone().unwrap_or_default();
            for part in [run.run_id.as_str(), ITEM, "leaves this to you"] {
                assert!(cause.contains(part), "{part:?} not in {cause}");
            }
            let notices = crate::workers::take_user_notices();
            assert_eq!(notices.len(), 1, "{notices:?}");
            // The user allows it from the list: the run goes on by itself.
            fixture
                .supervisor
                .answer(&WorkerAnswerParams {
                    worker_id: worker_id.clone(),
                    request_id: Some(listed.question.request_id.clone()),
                    decision: Some(WorkerDecision::Allow),
                    answers: Vec::new(),
                    message: None,
                    command_id: None,
                })
                .unwrap();
            let (review, _) = wait(&fixture, &run.run_id, Some(escalated.event_id));
            assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
            assert_eq!(review.result_text.as_deref(), Some("allow"));
            abort(&fixture, &run.run_id, review.event_id);
        }

        #[test]
        fn an_answer_call_that_fails_twice_is_escalated() {
            let fixture = reviewed(
                "todo-answer-fails",
                &[
                    "exit 1",
                    r#"{"action": "allow", "message": "allow takes none"}"#,
                ],
            );
            // A driver the answer below wakes runs before the answer
            // returns: the coordinator's resume must not race it (it once
            // made this event stale about 1 in 13 runs).
            runs::settle_inline(&repository_of(&fixture.repo).unwrap());
            let run = start_answering(&fixture, "perm WebFetch https://example.com", false);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Question, "{escalated:#?}");
            let error = escalated.error.clone().unwrap_or_default();
            assert!(error.contains("failed 2 times"), "{error}");
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_answer_call").len(),
                2
            );
            // The coordinator still answers it.
            fixture
                .supervisor
                .todo_resume(TodoResumeParams {
                    decision: Some(WorkerDecision::Deny),
                    message: Some("no".into()),
                    ..resume_params(&run.run_id, escalated.event_id, TodoAction::Answer)
                })
                .unwrap();
            let (review, _) = wait(&fixture, &run.run_id, Some(escalated.event_id));
            assert_eq!(review.result_text.as_deref(), Some("deny: no"));
            abort(&fixture, &run.run_id, review.event_id);
        }

        #[test]
        fn an_answer_recorded_before_a_crash_is_sent_without_asking_again() {
            let fixture = reviewed("todo-answer-crash", &[r#"{"action": "allow"}"#]);
            let repo = repository_of(&fixture.repo).unwrap();
            runs::crash_after(&repo, TodoStep::Attention);
            let run = start_answering(&fixture, "perm WebFetch https://example.com", false);
            runs::wait_crashed(&repo, HANG_GUARD);
            assert!(run_events(&fixture, &run.run_id, "run_auto_answered").is_empty());
            // A model asked again would fail: no answer is left.
            fixture.supervisor.resume_runs();
            let (review, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
            assert_eq!(review.result_text.as_deref(), Some("allow"));
            assert_eq!(calls(&fixture).len(), 1);
            abort(&fixture, &run.run_id, review.event_id);
        }

        /// A draft the stub worker carries out: it commits a.txt with the
        /// run's subject.
        fn valid_draft() -> String {
            serde_json::json!({
                "action": "draft", "task": format!("commit a.txt {SUBJECT}"),
                "message": SUBJECT, "paths": ["*.txt"], "checks": ["ok"],
            })
            .to_string()
        }

        /// The review fixture with a `DECISIONS.md` section that names the
        /// item and `AGENTS.md`'s rules on `master`.
        fn drafting(name: &str, answers: &[&str]) -> Fixture {
            let fixture = reviewed(name, answers);
            std::fs::write(
                fixture.repo.join("DECISIONS.md"),
                format!(
                    "# Decisions\n\n## Driving items\n\nDecided for [{ITEM}]: keep it small.\n\n\
                     ## Unrelated\n\nNot for this item.\n"
                ),
            )
            .unwrap();
            std::fs::write(
                fixture.repo.join("AGENTS.md"),
                "# Rules\n\n## Testing\n\nRun the stub checks.\n\n## Docs\n\nElsewhere.\n",
            )
            .unwrap();
            git_in(&fixture.repo, &["add", "."]);
            git_in(&fixture.repo, &["commit", "-q", "-m", "docs: rules"]);
            fixture
        }

        fn start_draft(fixture: &Fixture) -> TodoRunInfo {
            let run = fixture
                .supervisor
                .todo_draft_run(crate::api::schema::TodoDraftRunParams {
                    cwd: fixture.repo.display().to_string(),
                    item: ITEM.into(),
                    owner_pane_id: Some("p-coordinator".into()),
                    owner_session_id: None,
                    workspace_id: Some("ws-coordinator".into()),
                    env: Some(caller_env()),
                    ignore_usage: false,
                })
                .unwrap();
            assert!(run.drafted && run.auto_review && run.auto_answer, "{run:?}");
            assert_eq!(run.step, TodoStep::Draft);
            assert!(run.task.is_empty() && run.checks.is_empty(), "{run:?}");
            run
        }

        #[test]
        fn a_valid_draft_starts_the_run_as_its_flags_would() {
            let draft = valid_draft();
            let fixture = drafting("todo-draft-valid", &[&draft, r#"{"action": "approve"}"#]);
            let run = start_draft(&fixture);
            let (done, finished) = wait(&fixture, &run.run_id, None);
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(master_subjects(&fixture), [SUBJECT, "docs: rules", "init"]);
            assert_eq!(finished.task, format!("commit a.txt {SUBJECT}"));
            assert_eq!(finished.message, SUBJECT);
            assert_eq!(finished.paths, ["*.txt"]);
            assert_eq!(finished.checks, ["ok"]);
            assert!(finished.drafted);

            let decisions = run_events(&fixture, &run.run_id, "run_draft_decision");
            assert_eq!(decisions.len(), 1, "{decisions:#?}");
            assert!(decisions[0]["decision_id"]
                .as_str()
                .unwrap()
                .starts_with("d-"));
            assert_eq!(decisions[0]["model"], "stub-review-model");
            let applied = run_events(&fixture, &run.run_id, "run_drafted");
            assert_eq!(applied[0]["decision_id"], decisions[0]["decision_id"]);
            assert_eq!(applied[0]["message"], SUBJECT);

            // One stateless call with the item, the decision it names, the
            // rules, the checks and the log; the review is the second.
            let calls = calls(&fixture);
            assert_eq!(calls.len(), 2, "{calls:#?}");
            let input = calls[0]["stdin"].as_str().unwrap();
            for part in [
                "The driven item",
                "Driving items",
                "keep it small",
                "Run the stub checks.",
                "registered_checks",
                "\"never\"",
                "docs: rules",
            ] {
                assert!(input.contains(part), "{part:?} not in {input}");
            }
            for absent in ["Not for this item.", "Elsewhere."] {
                assert!(!input.contains(absent), "{absent:?} in {input}");
            }
            let schema = calls[0]["argv"]
                .as_array()
                .unwrap()
                .iter()
                .skip_while(|arg| *arg != "--json-schema")
                .nth(1)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            assert!(schema.contains("\"draft\""), "{schema}");

            // `todo review` shows the drafted task.
            let review = fixture
                .supervisor
                .todo_review(&run.run_id, None, false)
                .unwrap();
            let shown = review.draft.expect("no draft in the review");
            assert_eq!(shown.decision_id, decisions[0]["decision_id"]);
            assert_eq!(shown.message, SUBJECT);
            assert_eq!(shown.checks, ["ok"]);
            assert!(review.task.starts_with(&format!("commit a.txt {SUBJECT}")));
        }

        #[test]
        fn an_invalid_draft_is_asked_again_once_then_escalated_to_the_users_list() {
            let fixture = drafting(
                "todo-draft-invalid",
                &[
                    r#"{"action": "draft", "task": "t", "message": "Add a", "paths": ["*.txt"], "checks": ["ok"]}"#,
                    r#"{"action": "draft", "task": "t", "message": "feat: add a", "paths": ["../out"], "checks": ["ok"]}"#,
                ],
            );
            let _ = crate::workers::take_user_notices();
            let run = start_draft(&fixture);
            let (escalated, waiting) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Draft, "{escalated:#?}");
            assert_eq!(
                escalated.actions,
                [TodoAction::Retry, TodoAction::Abort],
                "{escalated:#?}"
            );
            assert_eq!(waiting.status, TodoRunStatus::Waiting);
            assert!(waiting.worker_id.is_none());
            let error = escalated.error.clone().unwrap_or_default();
            assert!(error.contains("failed 2 times"), "{error}");
            let made = run_events(&fixture, &run.run_id, "run_draft_call");
            assert_eq!(made.len(), 2, "{made:#?}");
            assert!(made.iter().all(|call| call["error"].is_string()));
            // The second call learns why the first was refused.
            let second = calls(&fixture)[1]["stdin"].as_str().unwrap().to_owned();
            assert!(second.contains("rejected_drafts"), "{second}");
            assert!(second.contains("type: description"), "{second}");
            assert_eq!(crate::workers::take_user_notices().len(), 1);

            // Listed under the run, which has no worker yet; the user's
            // answer drafts again with it.
            let listed = listed_escalation(&fixture, &run.run_id).expect("not in the ? list");
            assert_eq!(listed.worker_id, run.run_id);
            assert_eq!(listed.question.tool_name, "herdr draft");
            let detail = fixture
                .supervisor
                .question_detail(&listed.worker_id, &listed.question.request_id)
                .unwrap();
            assert!(
                detail.input_text.contains("task draft"),
                "{}",
                detail.input_text
            );
            let draft = valid_draft();
            set_answers(&fixture, &[&draft, r#"{"action": "approve"}"#]);
            let replied = fixture
                .supervisor
                .answer(&WorkerAnswerParams {
                    worker_id: listed.worker_id.clone(),
                    request_id: Some(listed.question.request_id.clone()),
                    decision: Some(WorkerDecision::Allow),
                    answers: vec!["use a.txt".into()],
                    message: None,
                    command_id: None,
                })
                .unwrap();
            assert_eq!(replied.worker_id, run.run_id);
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
            let (done, _) = wait(&fixture, &run.run_id, Some(escalated.event_id));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            let answers = run_events(&fixture, &run.run_id, "run_draft_answer");
            assert_eq!(answers.len(), 1, "{answers:#?}");
            assert_eq!(
                (answers[0]["answer"].as_str(), answers[0]["by"].as_str()),
                (Some("use a.txt"), Some("user"))
            );
            let again = calls(&fixture)[2]["stdin"].as_str().unwrap().to_owned();
            assert!(again.contains("use a.txt"), "{again}");
            assert!(!again.contains("rejected_drafts"), "{again}");
            assert_eq!(master_subjects(&fixture), [SUBJECT, "docs: rules", "init"]);
        }

        #[test]
        fn a_drafts_question_is_the_coordinators_too_and_its_retry_drafts_again() {
            let fixture = drafting(
                "todo-draft-escalate",
                &[
                    r#"{"action": "escalate", "question": "Is the item still wanted?", "options": ["yes", "no, drop it"]}"#,
                ],
            );
            let run = start_draft(&fixture);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(escalated.kind, TodoEventKind::Draft, "{escalated:#?}");
            let error = escalated.error.clone().unwrap_or_default();
            for part in ["Is the item still wanted?", "Options: yes | no, drop it"] {
                assert!(error.contains(part), "{part:?} not in {error}");
            }
            assert!(listed_escalation(&fixture, &run.run_id).is_some());
            // A retry needs the answer.
            let refused = resume(
                &fixture,
                &run.run_id,
                escalated.event_id,
                TodoAction::Retry,
                None,
            )
            .unwrap_err();
            assert_eq!(refused.code(), "invalid_request", "{refused}");
            let draft = valid_draft();
            set_answers(&fixture, &[&draft, r#"{"action": "approve"}"#]);
            resume(
                &fixture,
                &run.run_id,
                escalated.event_id,
                TodoAction::Retry,
                Some("yes, as written"),
            )
            .unwrap();
            let (done, _) = wait(&fixture, &run.run_id, Some(escalated.event_id));
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
            let answers = run_events(&fixture, &run.run_id, "run_draft_answer");
            assert_eq!(answers[0]["by"], "coordinator");
            assert_eq!(answers[0]["question"], "Is the item still wanted?");
            let again = calls(&fixture)[1]["stdin"].as_str().unwrap().to_owned();
            for part in ["yes, as written", "Is the item still wanted?"] {
                assert!(again.contains(part), "{part:?} not in {again}");
            }
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_draft_decision").len(),
                2
            );
        }

        #[test]
        fn a_draft_escalation_can_be_aborted() {
            let fixture = drafting(
                "todo-draft-abort",
                &[r#"{"action": "escalate", "question": "Wanted?", "options": ["yes", "no"]}"#],
            );
            let run = start_draft(&fixture);
            let (escalated, _) = wait(&fixture, &run.run_id, None);
            abort(&fixture, &run.run_id, escalated.event_id);
            assert!(listed_escalation(&fixture, &run.run_id).is_none());
        }

        #[test]
        fn a_draft_recorded_before_a_crash_is_applied_without_asking_again() {
            let draft = valid_draft();
            let fixture = drafting("todo-draft-crash", &[&draft]);
            let repo = repository_of(&fixture.repo).unwrap();
            runs::crash_after(&repo, TodoStep::Draft);
            let run = start_draft(&fixture);
            runs::wait_crashed(&repo, HANG_GUARD);
            let cut = fixture.supervisor.todo_status(&run.run_id).unwrap();
            assert_eq!(
                (cut.status, cut.step),
                (TodoRunStatus::Running, TodoStep::Draft)
            );
            assert!(run_events(&fixture, &run.run_id, "run_drafted").is_empty());
            // A draft asked again would get the review's answer, which is
            // no draft.
            set_answers(&fixture, &[r#"{"action": "approve"}"#]);
            fixture.supervisor.resume_runs();
            let (done, _) = wait(&fixture, &run.run_id, None);
            assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
            assert_eq!(calls(&fixture).len(), 2);
            assert_eq!(
                run_events(&fixture, &run.run_id, "run_draft_decision").len(),
                1
            );
            assert_eq!(master_subjects(&fixture), [SUBJECT, "docs: rules", "init"]);
        }

        /// Queue mode (`todo.queue_set`) with the review stub as the model of
        /// every decision call (draft, review) and the worker stub.
        mod queue {
            use super::*;
            use crate::api::schema::{
                TodoQueueInfo, TodoQueueMode, TodoQueueSetParams, TodoQueueStatus,
            };
            use crate::workers::runs::queue::{QueueStep, BREAKER, ITEM_ATTEMPTS};

            const ITEM3: &str = "t-cdef4567";
            const APPROVE: &str = r#"{"action": "approve"}"#;
            const ESCALATE: &str = r#"{"action": "escalate", "question": "Is the item still wanted?", "options": ["yes", "no"]}"#;

            /// A draft the worker stub carries out: it commits `file`.
            fn draft(file: &str, subject: &str) -> String {
                serde_json::json!({
                    "action": "draft", "task": format!("commit {file} {subject}"),
                    "message": subject, "paths": ["*.txt"], "checks": ["ok"],
                })
                .to_string()
            }

            /// The draft fixture with `items` in "Next, in order" on master,
            /// `scripts/todo_edit.py` (a queue run's approval closes its
            /// item) and no `origin` (the push is skipped). Returns it with
            /// the repository as the store names it.
            fn queued(name: &str, items: &[&str], answers: &[&str]) -> (Fixture, String) {
                let fixture = drafting(name, answers);
                let repo = &fixture.repo;
                std::fs::create_dir_all(repo.join("scripts")).unwrap();
                std::fs::copy(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/todo_edit.py"),
                    repo.join("scripts/todo_edit.py"),
                )
                .unwrap();
                let mut todo = String::from("# TODO\n\n## Next, in order\n\n");
                for (number, item) in items.iter().enumerate() {
                    todo.push_str(&format!(
                        "- [ ] Queued item {number} [{item}]\n  Its text.\n\n"
                    ));
                }
                todo.push_str("## Needs a decision\n");
                std::fs::write(repo.join("TODO.md"), todo).unwrap();
                git_in(repo, &["add", "."]);
                git_in(repo, &["commit", "-q", "-m", "docs: queue"]);
                let id = repository_of(repo).unwrap();
                (fixture, id)
            }

            fn set(fixture: &Fixture, mode: TodoQueueMode) -> TodoQueueInfo {
                fixture
                    .supervisor
                    .todo_queue_set(TodoQueueSetParams {
                        cwd: fixture.repo.display().to_string(),
                        mode,
                        reason: None,
                        owner_pane_id: Some("p-coordinator".into()),
                        owner_session_id: None,
                        workspace_id: Some("ws-coordinator".into()),
                        env: Some(caller_env()),
                    })
                    .unwrap()
            }

            fn status(fixture: &Fixture) -> TodoQueueInfo {
                fixture
                    .supervisor
                    .todo_queue_status(&fixture.repo.display().to_string())
                    .unwrap()
            }

            fn runs_of(fixture: &Fixture, repo: &str) -> Vec<TodoRunInfo> {
                fixture.supervisor.todo_runs(Some(repo), None).unwrap().0
            }

            /// The queue once `done` holds for it, read again at every write.
            fn wait_queue(
                fixture: &Fixture,
                what: &str,
                done: impl Fn(&TodoQueueInfo) -> bool,
            ) -> TodoQueueInfo {
                let mut last = None;
                runs::wait_until(what, HANG_GUARD, || {
                    let queue = status(fixture);
                    let reached = done(&queue);
                    last = Some(queue);
                    reached
                });
                last.unwrap()
            }

            fn items_and_states(runs: &[TodoRunInfo]) -> Vec<(&str, TodoRunStatus)> {
                runs.iter()
                    .map(|run| (run.item.as_str(), run.status))
                    .collect()
            }

            #[test]
            fn the_queue_advances_through_three_items() {
                let answers = [
                    draft("a.txt", "feat: add a"),
                    APPROVE.into(),
                    draft("b.txt", "feat: add b"),
                    APPROVE.into(),
                    draft("c.txt", "feat: add c"),
                    APPROVE.into(),
                ];
                let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
                let (fixture, repo) = queued("todo-queue-three", &[ITEM, ITEM2, ITEM3], &answers);
                let on = set(&fixture, TodoQueueMode::On);
                assert_eq!(on.mode, TodoQueueMode::On, "{on:#?}");
                assert_eq!(runs_of(&fixture, &repo)[0].item, ITEM);

                let ended = wait_queue(&fixture, "the queue's end", |queue| {
                    queue.status == TodoQueueStatus::Empty
                });
                assert!(ended.reason.contains("no open item"), "{ended:#?}");
                assert_eq!((ended.mode, ended.failures), (TodoQueueMode::On, 0));
                let runs = runs_of(&fixture, &repo);
                assert_eq!(
                    items_and_states(&runs),
                    [
                        (ITEM, TodoRunStatus::Done),
                        (ITEM2, TodoRunStatus::Done),
                        (ITEM3, TodoRunStatus::Done)
                    ]
                );
                assert!(
                    runs.iter().all(|run| run.queued && run.drafted),
                    "{runs:#?}"
                );
                // Each run landed its commit and closed its item.
                assert_eq!(
                    master_subjects(&fixture),
                    [
                        format!("docs(todo): close {ITEM3}").as_str(),
                        "feat: add c",
                        &format!("docs(todo): close {ITEM2}"),
                        "feat: add b",
                        &format!("docs(todo): close {ITEM}"),
                        "feat: add a",
                        "docs: queue",
                        "docs: rules",
                        "init"
                    ]
                );
                let todo = git_in(&fixture.repo, &["show", "master:TODO.md"]);
                for item in [ITEM, ITEM2, ITEM3] {
                    assert!(!todo.contains(item), "{item} still in {todo}");
                }
                let decisions = git_in(&fixture.repo, &["show", "master:DECISIONS.md"]);
                for title in ["Queued item 0", "Queued item 1", "Queued item 2"] {
                    assert!(decisions.contains(&format!("## {title}")), "{decisions}");
                }
                assert_eq!(
                    decisions.matches("Landed by herdr's TODO queue").count(),
                    3,
                    "{decisions}"
                );
                // Each start claimed the next fencing token; each end was
                // settled once.
                let stored = store_of(&fixture.supervisor).queue_runs(&repo).unwrap();
                assert_eq!(
                    stored
                        .iter()
                        .map(|run| (run.token, run.outcome.as_deref()))
                        .collect::<Vec<_>>(),
                    [(1, Some("done")), (2, Some("done")), (3, Some("done"))]
                );
            }

            #[test]
            fn the_runnable_state_counts_only_a_hand_driven_idle_repository_as_stalled() {
                let runnable = |fixture: &Fixture| {
                    fixture
                        .supervisor
                        .todo_runnable_state(&fixture.repo.display().to_string())
                        .unwrap()
                };
                let (fixture, _) = queued("todo-runnable-state", &[ITEM, ITEM2], &[ESCALATE]);
                let idle = runnable(&fixture);
                assert!(idle.stalled, "{idle:#?}");
                assert_eq!(idle.runnable_items, [ITEM, ITEM2]);
                assert_eq!(idle.queue_mode, None);

                // A pause (`herdr todo queue pause`) is the user's stop.
                fixture
                    .supervisor
                    .todo_queue_set(TodoQueueSetParams {
                        cwd: fixture.repo.display().to_string(),
                        mode: TodoQueueMode::Paused,
                        reason: Some("lunch".into()),
                        owner_pane_id: None,
                        owner_session_id: None,
                        workspace_id: None,
                        env: None,
                    })
                    .unwrap();
                let paused = runnable(&fixture);
                assert!(!paused.stalled, "{paused:#?}");
                assert_eq!(paused.queue_mode, Some(TodoQueueMode::Paused));
                assert!(paused.reason.contains("lunch"), "{paused:#?}");

                // In queue mode the server starts runs: never stalled, and
                // the run in progress is named.
                set(&fixture, TodoQueueMode::On);
                wait_queue(&fixture, "the escalation", |queue| {
                    queue.status == TodoQueueStatus::EscalationPending
                });
                let in_queue = runnable(&fixture);
                assert!(!in_queue.stalled, "{in_queue:#?}");
                assert!(in_queue.active_run.is_some(), "{in_queue:#?}");

                // A repository whose "Next, in order" is empty is not stalled.
                let (empty, _) = queued("todo-runnable-empty", &[], &[]);
                let state = runnable(&empty);
                assert!(
                    !state.stalled && state.runnable_items.is_empty(),
                    "{state:#?}"
                );
                assert!(state.reason.contains("no item"), "{state:#?}");
            }

            #[test]
            fn the_queue_waits_on_an_escalation_and_goes_on_after_the_answer() {
                let (fixture, repo) = queued("todo-queue-escalation", &[ITEM, ITEM2], &[ESCALATE]);
                set(&fixture, TodoQueueMode::On);
                let waiting = wait_queue(&fixture, "the escalation", |queue| {
                    queue.status == TodoQueueStatus::EscalationPending
                });
                assert_eq!(waiting.item.as_deref(), Some(ITEM), "{waiting:#?}");
                assert!(waiting.reason.contains("draft"), "{waiting:#?}");
                let run_id = waiting.run_id.clone().unwrap();
                // Another event starts nothing while the run waits.
                fixture.supervisor.queue_event(&repo);
                assert_eq!(runs_of(&fixture, &repo).len(), 1);

                // The user's answer drafts again; the run lands and the queue
                // goes on with the next item.
                let answers = [
                    draft("a.txt", "feat: add a"),
                    APPROVE.into(),
                    draft("b.txt", "feat: add b"),
                    APPROVE.into(),
                ];
                set_answers(
                    &fixture,
                    &answers.iter().map(String::as_str).collect::<Vec<_>>(),
                );
                let listed = listed_escalation(&fixture, &run_id).expect("not in the ? list");
                fixture
                    .supervisor
                    .answer(&WorkerAnswerParams {
                        worker_id: listed.worker_id.clone(),
                        request_id: Some(listed.question.request_id.clone()),
                        decision: Some(WorkerDecision::Allow),
                        answers: vec!["yes".into()],
                        message: None,
                        command_id: None,
                    })
                    .unwrap();
                wait_queue(&fixture, "the queue's end", |queue| {
                    queue.status == TodoQueueStatus::Empty
                });
                assert_eq!(
                    items_and_states(&runs_of(&fixture, &repo)),
                    [(ITEM, TodoRunStatus::Done), (ITEM2, TodoRunStatus::Done)]
                );
            }

            #[test]
            fn a_server_that_starts_settles_a_done_run_and_starts_the_next_item_once() {
                let answers = [
                    draft("a.txt", "feat: add a"),
                    APPROVE.into(),
                    draft("b.txt", "feat: add b"),
                    APPROVE.into(),
                ];
                let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
                let (fixture, repo) = queued("todo-queue-crash", &[ITEM, ITEM2], &answers);
                // The server ends between the first run's end and its queue's
                // look.
                runs::queue::skip_next_event(&repo);
                set(&fixture, TodoQueueMode::On);
                let first = runs_of(&fixture, &repo)[0].run_id.clone();
                let (done, _) = wait(&fixture, &first, None);
                assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
                assert_eq!(runs_of(&fixture, &repo).len(), 1);
                let stored = store_of(&fixture.supervisor).queue_runs(&repo).unwrap();
                assert_eq!(stored[0].outcome, None, "{stored:#?}");

                // A server that starts settles it, then starts the next item;
                // more events start no second run.
                fixture.supervisor.resume_runs();
                fixture.supervisor.queue_event(&repo);
                fixture.supervisor.queue_event(&repo);
                wait_queue(&fixture, "the queue's end", |queue| {
                    queue.status == TodoQueueStatus::Empty
                });
                assert_eq!(
                    items_and_states(&runs_of(&fixture, &repo)),
                    [(ITEM, TodoRunStatus::Done), (ITEM2, TodoRunStatus::Done)]
                );
                let stored = store_of(&fixture.supervisor).queue_runs(&repo).unwrap();
                assert!(
                    stored
                        .iter()
                        .all(|run| run.outcome.as_deref() == Some("done")),
                    "{stored:#?}"
                );
            }

            #[test]
            fn failures_block_an_item_and_trip_the_circuit_breaker() {
                let (fixture, repo) = queued(
                    "todo-queue-breaker",
                    &[ITEM, ITEM2],
                    &[ESCALATE, ESCALATE, ESCALATE],
                );
                let _ = crate::workers::take_user_notices();
                set(&fixture, TodoQueueMode::On);
                // The first item twice (its attempt cap), then the second.
                for (number, item) in [ITEM, ITEM, ITEM2].into_iter().enumerate() {
                    let waiting = wait_queue(&fixture, "the next escalation", |queue| {
                        queue.status == TodoQueueStatus::EscalationPending
                            && runs_of(&fixture, &repo).len() == number + 1
                    });
                    assert_eq!(waiting.item.as_deref(), Some(item), "{waiting:#?}");
                    let run_id = waiting.run_id.unwrap();
                    let (escalated, _) = wait(&fixture, &run_id, None);
                    assert_eq!(escalated.kind, TodoEventKind::Draft, "{escalated:#?}");
                    abort(&fixture, &run_id, escalated.event_id);
                }
                let paused = wait_queue(&fixture, "the circuit breaker", |queue| {
                    queue.mode == TodoQueueMode::Paused
                });
                assert_eq!(paused.status, TodoQueueStatus::WaitingOnUser, "{paused:#?}");
                assert_eq!(paused.failures, BREAKER);
                for part in ["circuit breaker", "3 consecutive", "aborted"] {
                    assert!(paused.reason.contains(part), "{part:?} not in {paused:#?}");
                }
                assert_eq!(paused.blocked_items.len(), 1, "{paused:#?}");
                assert_eq!(paused.blocked_items[0].item, ITEM);
                assert!(paused.blocked_items[0]
                    .reason
                    .contains(&format!("{ITEM_ATTEMPTS} times")));
                assert_eq!(runs_of(&fixture, &repo).len(), 3);
                let notices = crate::workers::take_user_notices();
                assert!(
                    notices.iter().any(|notice| notice.title.contains("paused")),
                    "{notices:#?}"
                );
                assert!(
                    notices
                        .iter()
                        .any(|notice| notice.title.contains("blocked")
                            && notice.title.contains(ITEM)),
                    "{notices:#?}"
                );

                // On again clears the breaker: the queue skips the blocked
                // item and runs the next one.
                let answers = [draft("b.txt", "feat: add b"), APPROVE.into()];
                set_answers(
                    &fixture,
                    &answers.iter().map(String::as_str).collect::<Vec<_>>(),
                );
                let on = set(&fixture, TodoQueueMode::On);
                assert_eq!(on.failures, 0, "{on:#?}");
                let blocked = wait_queue(&fixture, "the blocked queue", |queue| {
                    queue.status == TodoQueueStatus::Blocked
                });
                assert!(blocked.reason.contains("every item"), "{blocked:#?}");
                assert!(blocked.reason.contains(ITEM), "{blocked:#?}");
                let runs = runs_of(&fixture, &repo);
                assert_eq!(
                    items_and_states(&runs),
                    [
                        (ITEM, TodoRunStatus::Aborted),
                        (ITEM, TodoRunStatus::Aborted),
                        (ITEM2, TodoRunStatus::Aborted),
                        (ITEM2, TodoRunStatus::Done)
                    ]
                );
            }

            #[test]
            fn concurrent_events_start_one_run() {
                let (fixture, repo) = queued("todo-queue-concurrent", &[ITEM, ITEM2], &[ESCALATE]);
                runs::queue::set_env_for_test(&repo, Some(caller_env()));
                // The mode is on without an evaluation of its own: the starts
                // below race for it.
                store_of(&fixture.supervisor)
                    .transaction(|tx| {
                        tx.queue_on(
                            &repo,
                            &store::RunOwner {
                                pane_id: Some("p-coordinator"),
                                session_id: None,
                                workspace: Some("ws-coordinator"),
                                coordinator_id: None,
                            },
                            now_ms(),
                        )
                    })
                    .unwrap();
                let barrier = std::sync::Barrier::new(12);
                let steps: Vec<QueueStep> = std::thread::scope(|scope| {
                    let handles: Vec<_> = (0..12)
                        .map(|number| {
                            let (barrier, supervisor, repo) =
                                (&barrier, &fixture.supervisor, &repo);
                            scope.spawn(move || {
                                barrier.wait();
                                // Some through the event path, the others
                                // straight to the start, past this process's
                                // claim: only the fencing token and the
                                // runs' unique index keep them apart.
                                if number % 3 == 0 {
                                    supervisor.queue_event(repo);
                                    QueueStep::Idle
                                } else {
                                    supervisor.queue_step(repo).unwrap()
                                }
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|handle| handle.join().unwrap())
                        .collect()
                });
                let started = steps
                    .iter()
                    .filter(|step| matches!(step, QueueStep::Started(_)))
                    .count();
                assert!(started <= 1, "{steps:?}");
                let runs = runs_of(&fixture, &repo);
                assert_eq!(runs.len(), 1, "{runs:#?}");
                let store = store_of(&fixture.supervisor);
                assert_eq!(store.queue_runs(&repo).unwrap().len(), 1);
                assert_eq!(store.queue(&repo).unwrap().unwrap().token, 1);

                // Paused, the run's end starts nothing.
                set(&fixture, TodoQueueMode::Paused);
                let (escalated, _) = wait(&fixture, &runs[0].run_id, None);
                abort(&fixture, &runs[0].run_id, escalated.event_id);
                fixture.supervisor.queue_event(&repo);
                assert_eq!(runs_of(&fixture, &repo).len(), 1);
                let paused = status(&fixture);
                assert_eq!(
                    (paused.mode, paused.status),
                    (TodoQueueMode::Paused, TodoQueueStatus::WaitingOnUser)
                );
            }

            #[test]
            fn a_close_in_queue_mode_names_no_next_item() {
                let (fixture, repo) = queued("todo-queue-close", &[ITEM, ITEM2], &[ESCALATE]);
                set(&fixture, TodoQueueMode::Paused);
                let run = fixture
                    .supervisor
                    .todo_run(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
                    .unwrap();
                let (review, _) = wait(&fixture, &run.run_id, None);
                assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
                let close = |next: bool| TodoResumeParams {
                    close: Some("Closed.".into()),
                    stop_reason: next.then(|| "why".to_owned()),
                    ..resume_params(&run.run_id, review.event_id, TodoAction::Approve)
                };
                // Paused (queue mode not on), a close still names what follows.
                let refused = fixture.supervisor.todo_resume(close(false)).unwrap_err();
                assert!(
                    refused.to_string().contains("unless queue mode is on"),
                    "{refused}"
                );
                // On, it names nothing: the queue starts the next item.
                runs::queue::set_env_for_test(&repo, Some(caller_env()));
                store_of(&fixture.supervisor)
                    .transaction(|tx| {
                        tx.queue_on(
                            &repo,
                            &store::RunOwner {
                                pane_id: Some("p-coordinator"),
                                session_id: None,
                                workspace: Some("ws-coordinator"),
                                coordinator_id: None,
                            },
                            now_ms(),
                        )
                    })
                    .unwrap();
                let refused = fixture.supervisor.todo_resume(close(true)).unwrap_err();
                assert!(
                    refused.to_string().contains("queue mode is on"),
                    "{refused}"
                );
                fixture.supervisor.todo_resume(close(false)).unwrap();
                // Its end starts the queue's run of the next item, which waits
                // on its escalated draft.
                let waiting = wait_queue(&fixture, "the queue's start", |queue| {
                    queue.status == TodoQueueStatus::EscalationPending
                });
                assert_eq!(waiting.item.as_deref(), Some(ITEM2), "{waiting:#?}");
                let runs = runs_of(&fixture, &repo);
                assert_eq!(
                    items_and_states(&runs),
                    [(ITEM, TodoRunStatus::Done), (ITEM2, TodoRunStatus::Waiting)]
                );
                assert!(!runs[0].queued && runs[1].queued);
            }

            /// The queue's run of `ITEM` waiting on its review, which the
            /// review's model escalated to the coordinator.
            fn review_escalated(fixture: &Fixture) -> (String, TodoRunEvent) {
                let waiting = wait_queue(fixture, "the escalated review", |queue| {
                    queue.status == TodoQueueStatus::EscalationPending
                });
                assert_eq!(waiting.item.as_deref(), Some(ITEM), "{waiting:#?}");
                let run_id = waiting.run_id.unwrap();
                let (review, _) = wait(fixture, &run_id, None);
                assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
                (run_id, review)
            }

            #[test]
            fn a_coordinators_approval_in_queue_mode_closes_the_item() {
                let answers = [draft("a.txt", "feat: add a"), ESCALATE.into()];
                let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
                let (fixture, repo) = queued("todo-queue-manual-close", &[ITEM, ITEM2], &answers);
                set(&fixture, TodoQueueMode::On);
                let (run_id, review) = review_escalated(&fixture);
                let answers = [draft("b.txt", "feat: add b"), APPROVE.into()];
                set_answers(
                    &fixture,
                    &answers.iter().map(String::as_str).collect::<Vec<_>>(),
                );
                // Neither a note nor a close: it closes like the queue's own
                // review would, and the queue goes on with the next item.
                fixture
                    .supervisor
                    .todo_resume(resume_params(&run_id, review.event_id, TodoAction::Approve))
                    .unwrap();
                wait_queue(&fixture, "the queue's end", |queue| {
                    queue.status == TodoQueueStatus::Empty
                });
                assert_eq!(
                    items_and_states(&runs_of(&fixture, &repo)),
                    [(ITEM, TodoRunStatus::Done), (ITEM2, TodoRunStatus::Done)]
                );
                let todo = git_in(&fixture.repo, &["show", "master:TODO.md"]);
                assert!(!todo.contains(ITEM), "{todo}");
                let decisions = git_in(&fixture.repo, &["show", "master:DECISIONS.md"]);
                assert!(decisions.contains("## Queued item 0"), "{decisions}");
                assert!(
                    decisions.contains(&format!("run {run_id}"))
                        && decisions.contains("approved by the coordinator"),
                    "{decisions}"
                );
                let resumed = run_events(&fixture, &run_id, "run_resumed");
                assert_eq!(resumed[0]["queue_close"], true, "{resumed:#?}");
            }

            #[test]
            fn keep_open_leaves_an_approved_queue_item_open() {
                let answers = [draft("a.txt", "feat: add a"), ESCALATE.into()];
                let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
                let (fixture, repo) = queued("todo-queue-keep-open", &[ITEM, ITEM2], &answers);
                set(&fixture, TodoQueueMode::On);
                let (run_id, review) = review_escalated(&fixture);
                // Paused, so the item left open is not started again here; a
                // queue run still closes its item by default while paused.
                set(&fixture, TodoQueueMode::Paused);
                let approve = |keep_open: bool, close: Option<&str>, action| TodoResumeParams {
                    keep_open,
                    close: close.map(str::to_owned),
                    ..resume_params(&run_id, review.event_id, action)
                };
                for (refused, why) in [
                    (
                        approve(true, Some("Closed."), TodoAction::Approve),
                        "exclude",
                    ),
                    (approve(true, None, TodoAction::Abort), "only approve"),
                ] {
                    let error = fixture.supervisor.todo_resume(refused).unwrap_err();
                    assert!(error.to_string().contains(why), "{error}");
                }
                fixture
                    .supervisor
                    .todo_resume(approve(true, None, TodoAction::Approve))
                    .unwrap();
                let (done, _) = wait(&fixture, &run_id, Some(review.event_id));
                assert_eq!(done.kind, TodoEventKind::Done, "{done:#?}");
                assert_eq!(
                    master_subjects(&fixture)[0],
                    "feat: add a",
                    "nothing closed the item"
                );
                let todo = git_in(&fixture.repo, &["show", "master:TODO.md"]);
                assert!(todo.contains(ITEM), "{todo}");
                assert_eq!(runs_of(&fixture, &repo).len(), 1);
                let resumed = run_events(&fixture, &run_id, "run_resumed");
                assert_eq!(
                    (&resumed[0]["queue_close"], &resumed[0]["keep_open"]),
                    (&serde_json::json!(false), &serde_json::json!(true))
                );
            }

            #[test]
            fn a_usage_reading_wakes_a_queue_the_usage_gate_stopped() {
                let answers = [draft("a.txt", "feat: add a"), APPROVE.into()];
                let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
                let (fixture, repo) = queued("todo-queue-usage", &[ITEM], &answers);
                fixture.supervisor.set_usage_for_test(fresh_usage(95, 10));
                let gated = set(&fixture, TodoQueueMode::On);
                assert_eq!(gated.status, TodoQueueStatus::UsageGate, "{gated:#?}");
                assert!(runs_of(&fixture, &repo).is_empty());
                // A reading while the gate still refuses starts nothing.
                fixture.supervisor.queues_after_usage_reading();
                assert_eq!(status(&fixture).status, TodoQueueStatus::UsageGate);
                assert!(runs_of(&fixture, &repo).is_empty());

                // Below the reopening threshold, the next reading the poller
                // publishes (the server's event) starts the item.
                fixture.supervisor.set_usage_for_test(fresh_usage(10, 10));
                crate::workers::set_test_coordinators(fixture.supervisor.clone());
                crate::workers::usage_reading_published();
                wait_queue(&fixture, "the queue's end", |queue| {
                    queue.status == TodoQueueStatus::Empty
                });
                assert_eq!(
                    items_and_states(&runs_of(&fixture, &repo)),
                    [(ITEM, TodoRunStatus::Done)]
                );
            }

            #[test]
            fn a_cold_restart_pauses_the_queue_until_on_sends_the_environment() {
                let answers = [draft("a.txt", "feat: add a"), APPROVE.into()];
                let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
                let (fixture, repo) = queued("todo-queue-cold", &[ITEM], &answers);
                // On, kept in the store; the server that starts next has no
                // environment in its memory.
                store_of(&fixture.supervisor)
                    .transaction(|tx| {
                        tx.queue_on(
                            &repo,
                            &store::RunOwner {
                                pane_id: Some("p-coordinator"),
                                session_id: None,
                                workspace: Some("ws-coordinator"),
                                coordinator_id: None,
                            },
                            now_ms(),
                        )
                    })
                    .unwrap();
                runs::queue::set_env_for_test(&repo, None);
                let _ = crate::workers::take_user_notices();
                fixture.supervisor.resume_runs();
                let paused = status(&fixture);
                assert_eq!(
                    (paused.mode, paused.status),
                    (TodoQueueMode::Paused, TodoQueueStatus::WaitingOnUser),
                    "{paused:#?}"
                );
                assert_eq!(
                    paused.pause_reason.as_deref(),
                    Some(runs::queue::ENV_NEEDED)
                );
                assert!(runs_of(&fixture, &repo).is_empty());
                let notices = crate::workers::take_user_notices();
                assert!(
                    notices.iter().any(|notice| notice.title.contains("paused")
                        && notice.body == runs::queue::ENV_NEEDED),
                    "{notices:#?}"
                );
                // `queue on` from a shell sends it: the queue runs again.
                set(&fixture, TodoQueueMode::On);
                wait_queue(&fixture, "the queue's end", |queue| {
                    queue.status == TodoQueueStatus::Empty
                });
                assert_eq!(
                    items_and_states(&runs_of(&fixture, &repo)),
                    [(ITEM, TodoRunStatus::Done)]
                );
            }
        }
    }
}

/// `herdr todo next` against a stub item coordinator. The stub reads its
/// item's id from the prompt's first line (`Item coordinator for <id>:
/// <title>`) and acts on the title's first word: `done` removes the item
/// from TODO.md and ends with `COORDINATOR-DONE`, `escalate` ends with
/// `COORDINATOR-ESCALATED`, `ask` asks the user (AskUserQuestion) and then
/// acts as `done`, `hook` runs the PreToolUse hook (herdr's allowlist) on
/// two Bash commands and ends with both answers and `COORDINATOR-BLOCKED`.
mod todo_next {
    use super::*;
    use crate::api::schema::{HistoryEventKind, TodoChainInfo, TodoNextParams};

    const COORDINATOR_STUB: &str = r#"#!/usr/bin/env python3
import json, os, re, sys

def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()

def result(text):
    emit({"type": "result", "subtype": "success", "is_error": False,
          "terminal_reason": "completed", "api_error_status": None, "result": text,
          "session_id": "coordinator-session"})

def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)

emit({"type": "system", "subtype": "init", "session_id": "coordinator-session",
      "herdr_env": sorted(k for k in os.environ if k.startswith("HERDR_"))})
hooks = {}

def hook(command):
    for entry in hooks.get("PreToolUse", []):
        if re.fullmatch(entry.get("matcher") or ".*", "Bash"):
            emit({"type": "control_request", "request_id": "hook-1", "request": {
                "subtype": "hook_callback", "callback_id": entry["hookCallbackIds"][0],
                "tool_use_id": "toolu-1",
                "input": {"hook_event_name": "PreToolUse", "tool_name": "Bash",
                          "tool_input": {"command": command}, "cwd": os.getcwd(),
                          "session_id": "coordinator-session"}}})
            return json.dumps(read()["response"]["response"], sort_keys=True)
    return "unregistered"

def close(item):
    with open("TODO.md") as f:
        lines = [line for line in f if item not in line]
    with open("TODO.md", "w") as f:
        f.writelines(lines)

while True:
    message = read()
    if message.get("type") == "control_request" and \
            message["request"].get("subtype") == "initialize":
        hooks = message["request"].get("hooks") or {}
        emit({"type": "control_response", "response": {
            "subtype": "success", "request_id": message["request_id"], "response": {}}})
        continue
    if message.get("type") != "user":
        continue
    first = message["message"]["content"].split("\n")[0]
    match = re.match(r"Item coordinator for (t-[a-z2-7]{8}): (\w+)", first)
    item, word = match.group(1), match.group(2)
    if word == "ask":
        emit({"type": "control_request", "request_id": "perm-1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": {"questions": [
                {"question": "Which way?", "header": "Way", "multiSelect": False,
                 "options": [{"label": "left", "description": "l"},
                             {"label": "right", "description": "r"}]}]}}})
        read()
        word = "done"
    if word == "done":
        close(item)
        result("closed it\nCOORDINATOR-DONE " + item + " | closed")
    elif word == "escalate":
        result("COORDINATOR-ESCALATED which option for " + item + "?")
    elif word == "hook":
        refused = hook("cargo build")
        allowed = hook("herdr todo status r-abcd2345")
        result(refused + "\n" + allowed + "\nCOORDINATOR-BLOCKED hook test")
    else:
        result("no marker")
"#;

    const A: &str = "t-aaaaaaaa";
    const B: &str = "t-bbbbbbbb";
    const CHAT: &str = "p-chat";

    /// A repository whose "Next, in order" holds `items` (id, title) in
    /// order, with a stub item coordinator as the supervisor's program.
    fn next_repo(name: &str, items: &[(&str, &str)]) -> Fixture {
        let fixture = Fixture::new(name);
        std::fs::write(fixture.root.join("claude-stub"), COORDINATOR_STUB).unwrap();
        let repo = &fixture.repo;
        git_in(repo, &["init", "-q", "-b", "master"]);
        let mut todo = String::from("# TODO\n\n## Next, in order\n\n");
        for (id, title) in items {
            todo.push_str(&format!("- [ ] {title} [{id}]\n  more about it\n"));
        }
        todo.push_str("\n## Needs a decision\n\n- [ ] Later [t-zzzzzzzz]\n");
        std::fs::write(repo.join("TODO.md"), todo).unwrap();
        fixture
    }

    fn next(fixture: &Fixture, chain: bool) -> Result<item_coordinators::NextStarted, WorkerError> {
        fixture.supervisor.todo_next(&TodoNextParams {
            cwd: fixture.repo.display().to_string(),
            chain,
            owner_pane_id: Some(CHAT.into()),
            owner_session_id: Some("s-chat".into()),
            workspace_id: Some("ws-chat".into()),
            env: None,
        })
    }

    fn repo(fixture: &Fixture) -> String {
        repository_of(&fixture.repo).unwrap()
    }

    /// Blocks until `done` holds, checked under the registry lock and woken
    /// by every worker event and every tenure or chain change, which are
    /// announced under that lock; the timeout is only the hang guard.
    fn wait_until(fixture: &Fixture, what: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        let mut registry = lock(&fixture.supervisor.shared.registry);
        while !done() {
            assert!(started.elapsed() < HANG_GUARD, "{what} hung");
            registry = fixture
                .supervisor
                .shared
                .changed
                .wait_timeout(registry, HANG_GUARD)
                .unwrap()
                .0;
        }
    }

    fn wait_ended(fixture: &Fixture, coordinator_id: &str) -> store::StoredTenure {
        wait_until(fixture, coordinator_id, || {
            fixture
                .supervisor
                .tenure_for_test(coordinator_id)
                .is_some_and(|tenure| tenure.ended_at.is_some())
        });
        fixture.supervisor.tenure_for_test(coordinator_id).unwrap()
    }

    fn wait_chain_stopped(fixture: &Fixture) -> TodoChainInfo {
        let repo = repo(fixture);
        wait_until(fixture, "the chain", || {
            fixture
                .supervisor
                .chain_for_test(&repo)
                .is_some_and(|chain| !chain.active)
        });
        fixture.supervisor.chain_for_test(&repo).unwrap()
    }

    /// The item's records: (kind, text).
    fn records(fixture: &Fixture, item: &str) -> Vec<(HistoryEventKind, Option<String>)> {
        fixture
            .supervisor
            .history_item(item, Some(&repo(fixture)))
            .unwrap()
            .events
            .into_iter()
            .map(|event| (event.kind, event.text))
            .collect()
    }

    #[test]
    fn next_starts_one_headless_coordinator_and_refuses_a_second() {
        let fixture = next_repo("next-one", &[(A, "ask first"), (B, "done second")]);
        let started = next(&fixture, false).unwrap();
        assert_eq!(started.item, A);
        assert!(started.chain.is_none());
        let coordinator = &started.coordinator;
        assert!(coordinator.headless, "{coordinator:#?}");
        assert_eq!(coordinator.pane_id, None);
        assert_eq!(coordinator.item.as_deref(), Some(A));
        let worker_id = started.worker.worker_id.clone();
        assert_eq!(coordinator.worker_id.as_deref(), Some(worker_id.as_str()));
        assert_eq!(started.worker.owner_pane_id.as_deref(), Some(CHAT));
        assert_eq!(started.worker.item.as_deref(), Some(A));

        // Its question is the user's at once, not quiet for the chat pane.
        fixture.wait_for_question(&worker_id);
        let pending = fixture.supervisor.pending_questions();
        assert_eq!(pending.len(), 1, "{pending:#?}");
        assert!(!pending[0].quiet, "{pending:#?}");

        // A second one is refused while it is active, naming it.
        let refused = next(&fixture, false).unwrap_err();
        assert_eq!(refused.code(), "coordinator_active", "{refused}");
        assert!(
            refused.to_string().contains(&coordinator.coordinator_id),
            "{refused}"
        );
        // So is a pane's claim of the repository.
        let claimed = fixture
            .supervisor
            .coordinator_start(&repo(&fixture), "p-other", None)
            .unwrap_err();
        assert_eq!(claimed.code(), "coordinator_active", "{claimed}");

        fixture.answer(&worker_id, None, &["1"]).unwrap();
        let ended = wait_ended(&fixture, &coordinator.coordinator_id);
        assert_eq!(ended.end_reason.as_deref(), Some("item_done"));
        let worker = fixture.wait(&worker_id, WorkerWaitUntil::Exit);
        assert_eq!(worker.state, WorkerState::Exited);
        assert_eq!(
            records(&fixture, A),
            [
                (HistoryEventKind::CoordinatorStarted, None),
                (
                    HistoryEventKind::CoordinatorEnded,
                    Some(format!("done: {A} | closed"))
                ),
            ]
        );
        // Without a chain nothing follows, and its end is the chat pane's
        // to acknowledge.
        assert_eq!(fixture.supervisor.list().len(), 1);
        assert_eq!(fixture.supervisor.obligations(Some(CHAT)).len(), 1);
        assert!(fixture.supervisor.chain_for_test(&repo(&fixture)).is_none());
    }

    #[test]
    fn the_chain_advances_on_each_exit_until_next_is_empty() {
        let fixture = next_repo("next-chain", &[(A, "done first"), (B, "done second")]);
        let started = next(&fixture, true).unwrap();
        assert_eq!(started.item, A);
        assert!(started.chain.as_ref().is_some_and(|chain| chain.active));
        let chain = wait_chain_stopped(&fixture);
        assert_eq!(chain.last_item.as_deref(), Some(B));
        assert_eq!(chain.last_outcome.as_deref(), Some("done"));
        let reason = chain.stop_reason.unwrap();
        assert!(reason.contains("has no open item"), "{reason}");
        for item in [A, B] {
            let kinds: Vec<HistoryEventKind> = records(&fixture, item)
                .into_iter()
                .map(|(kind, _)| kind)
                .collect();
            assert_eq!(
                kinds,
                [
                    HistoryEventKind::CoordinatorStarted,
                    HistoryEventKind::CoordinatorEnded
                ],
                "{item}"
            );
        }
        // Each end that started the next was acknowledged.
        assert!(fixture.supervisor.obligations(Some(CHAT)).is_empty());
        let todo = std::fs::read_to_string(fixture.repo.join("TODO.md")).unwrap();
        assert!(!todo.contains(A) && !todo.contains(B), "{todo}");
    }

    #[test]
    fn the_chain_stops_when_an_item_escalates() {
        let fixture = next_repo(
            "next-escalate",
            &[(A, "escalate first"), (B, "done second")],
        );
        let started = next(&fixture, true).unwrap();
        let chain = wait_chain_stopped(&fixture);
        assert_eq!(chain.last_outcome.as_deref(), Some("escalated"));
        let reason = chain.stop_reason.unwrap();
        assert!(
            reason.contains(A) && reason.contains("escalated"),
            "{reason}"
        );
        let ended = wait_ended(&fixture, &started.coordinator.coordinator_id);
        assert_eq!(ended.end_reason.as_deref(), Some("item_escalated"));
        // B never started; A's end waits for the chat pane.
        assert!(fixture
            .supervisor
            .history_item(B, Some(&repo(&fixture)))
            .is_err());
        let obligations = fixture.supervisor.obligations(Some(CHAT));
        assert_eq!(obligations.len(), 1, "{obligations:#?}");
        assert_eq!(obligations[0].worker_id, started.worker.worker_id);
    }

    #[test]
    fn todo_stop_lets_the_running_item_finish_and_starts_no_next() {
        let fixture = next_repo("next-stop", &[(A, "ask first"), (B, "done second")]);
        let started = next(&fixture, true).unwrap();
        let worker_id = started.worker.worker_id.clone();
        fixture.wait_for_question(&worker_id);
        let stopped = fixture
            .supervisor
            .todo_stop(&fixture.repo.display().to_string())
            .unwrap();
        assert!(!stopped.active);
        assert_eq!(
            stopped.stop_reason.as_deref(),
            Some(item_coordinators::STOPPED)
        );
        fixture.answer(&worker_id, None, &["2"]).unwrap();
        let ended = wait_ended(&fixture, &started.coordinator.coordinator_id);
        assert_eq!(ended.end_reason.as_deref(), Some("item_done"));
        let chain = fixture.supervisor.chain_for_test(&repo(&fixture)).unwrap();
        assert_eq!(
            chain.stop_reason.as_deref(),
            Some(item_coordinators::STOPPED)
        );
        assert!(fixture
            .supervisor
            .history_item(B, Some(&repo(&fixture)))
            .is_err());
        // A repository that never had a chain has nothing to stop, and with
        // "Next, in order" empty there is nothing to start.
        let other = next_repo("next-stop-none", &[]);
        assert!(other
            .supervisor
            .todo_stop(&other.repo.display().to_string())
            .is_err());
        let refused = next(&other, false).unwrap_err();
        assert_eq!(refused.code(), WorkerError::Preflight(String::new()).code());
    }

    #[test]
    fn a_coordinator_runs_under_the_allowlist_and_reaches_the_servers_socket() {
        let fixture = next_repo("next-hook", &[(A, "hook first")]);
        let started = next(&fixture, false).unwrap();
        let worker_id = started.worker.worker_id.clone();
        let ended = wait_ended(&fixture, &started.coordinator.coordinator_id);
        assert_eq!(ended.end_reason.as_deref(), Some("item_blocked"));
        let worker = fixture.supervisor.status(&worker_id).unwrap();
        let text = worker.last_result.unwrap().text.unwrap();
        let mut lines = text.lines();
        let refused = lines.next().unwrap();
        assert!(
            refused.contains("\"deny\"") && refused.contains("Herdr coordinator allowlist"),
            "{text}"
        );
        assert!(refused.contains("no override"), "{text}");
        assert_eq!(lines.next(), Some("{}"), "{text}");
        let journal = fixture.journal(&worker_id);
        let settings: Value = serde_json::from_str(&launch_arg(&journal, "--settings")).unwrap();
        let socket = fixture.root.join("workers").join("api.sock");
        assert_eq!(
            settings["sandbox"]["network"]["allowUnixSockets"],
            serde_json::json!([socket.display().to_string()])
        );
        assert!(launch_arg(&journal, "--append-system-prompt").contains("item coordinator"));
        let init = journal
            .iter()
            .find(|record| record["event"]["subtype"] == "init")
            .unwrap();
        assert_eq!(
            init["event"]["herdr_env"],
            serde_json::json!(["HERDR_SOCKET_PATH"])
        );
        assert_eq!(
            fixture.herdr_events(&worker_id, "started")[0]["coordinator"]["coordinator_id"],
            started.coordinator.coordinator_id.as_str()
        );
    }
}

/// `worker.events`: one coordinator's inbox over all of its workers.
mod inbox_events {
    use super::*;
    use crate::api::schema::{WorkerEvent, WorkerEventKind, WorkerEventsParams};
    use crate::workers::inbox::Events;

    const PANE: &str = "p-coordinator";

    /// A fixture with an active tenure bound to [`PANE`], and its id.
    fn coordinated(name: &str) -> (Fixture, String) {
        let fixture = Fixture::new(name);
        let repo = fixture.repo.display().to_string();
        let tenure = fixture
            .supervisor
            .coordinator_start(&repo, PANE, Some("s-coordinator"))
            .unwrap();
        (fixture, tenure.coordinator_id)
    }

    /// Starts a worker owned by [`PANE`]'s tenure, on `supervisor`.
    fn start_owned(fixture: &Fixture, supervisor: &WorkerSupervisor, prompt: &str) -> String {
        let mut params = start_params(&fixture.repo, prompt, Some("stub-model"));
        params.owner_pane_id = Some(PANE.into());
        supervisor.start(&params).unwrap().worker_id
    }

    fn params(owner: &str, after: Option<&str>) -> WorkerEventsParams {
        WorkerEventsParams {
            owner: Some(owner.into()),
            after: after.map(str::to_owned),
            ..WorkerEventsParams::default()
        }
    }

    /// One read that does not wait.
    fn read(supervisor: &WorkerSupervisor, params: &WorkerEventsParams) -> Events {
        supervisor
            .events(params, HANG_GUARD, || panic!("a read without wait blocked"))
            .unwrap()
            .unwrap()
    }

    /// `worker.events --wait` with the liveness re-check as long as the hang
    /// guard, so a missed wake fails the test. `on_block` runs at the first
    /// point the wait would block, in the window between its look and its
    /// block. Returns the reply and how many times the wait got that far.
    fn wait(
        supervisor: &WorkerSupervisor,
        owner: &str,
        after: &str,
        mut on_block: impl FnMut(),
    ) -> (Events, usize) {
        let started = Instant::now();
        let mut blocked = 0;
        let params = WorkerEventsParams {
            wait: true,
            ..params(owner, Some(after))
        };
        let events = supervisor
            .events(&params, HANG_GUARD, || {
                assert!(started.elapsed() < HANG_GUARD, "the events wait hung");
                blocked += 1;
                if blocked == 1 {
                    on_block();
                }
                true
            })
            .unwrap()
            .unwrap();
        (events, blocked)
    }

    /// Waits from `after`, re-arming from each reply's cursor, until the
    /// events gathered satisfy `done`; `on_block` runs at the first block.
    /// Returns them and the last cursor.
    fn gather(
        supervisor: &WorkerSupervisor,
        owner: &str,
        after: &str,
        on_block: impl FnOnce(),
        done: impl Fn(&[WorkerEvent]) -> bool,
    ) -> (Vec<WorkerEvent>, String) {
        let mut cursor = after.to_owned();
        let mut gathered = Vec::new();
        let mut on_block = Some(on_block);
        while !done(&gathered) {
            let (events, _) = wait(supervisor, owner, &cursor, || {
                if let Some(on_block) = on_block.take() {
                    on_block();
                }
            });
            assert!(!events.events.is_empty(), "a wait answered with nothing");
            gathered.extend(events.events);
            cursor = events.next_cursor;
        }
        (gathered, cursor)
    }

    fn kinds(events: &[WorkerEvent]) -> Vec<(String, WorkerEventKind)> {
        events
            .iter()
            .map(|event| (event.worker_id.clone(), event.kind))
            .collect()
    }

    fn has(events: &[WorkerEvent], worker_id: &str, kind: WorkerEventKind) -> bool {
        events
            .iter()
            .any(|event| event.worker_id == worker_id && event.kind == kind)
    }

    fn asked(events: &[WorkerEvent]) -> Vec<String> {
        events
            .iter()
            .flat_map(|event| &event.questions)
            .map(|question| question.request_id.clone())
            .collect()
    }

    #[test]
    fn two_questions_asked_together_arrive_in_one_batch_and_stay_pending() {
        let (fixture, owner) = coordinated("inbox-pair");
        let supervisor = &fixture.supervisor;
        let first = read(supervisor, &params(&owner, None));
        assert!(first.events.is_empty());
        assert!(!first.resync_required);
        assert!(first.snapshot.unwrap().workers.is_empty());

        let id = start_owned(&fixture, supervisor, "pair WebFetch https://example.com");
        fixture.wait_for(&id, |worker| worker.questions.len() == 2);
        let (batch, blocked) = wait(supervisor, &owner, &first.next_cursor, || {});
        assert_eq!(blocked, 0, "both were there already");
        assert_eq!(batch.events[0].kind, WorkerEventKind::Joined);
        assert_eq!(
            asked(&batch.events),
            ["perm-1", "perm-2"],
            "{:#?}",
            batch.events
        );
        assert!(batch
            .events
            .windows(2)
            .all(|pair| pair[0].seq <= pair[1].seq));
        assert!(!batch.more);
        assert!(batch.snapshot.is_none());
        assert!(batch
            .next_cursor
            .ends_with(&format!("-{}", batch.events.last().unwrap().seq)));

        // A bounded batch says more follow, and the next one goes on from it.
        let one = read(
            supervisor,
            &WorkerEventsParams {
                limit: Some(1),
                ..params(&owner, Some(&first.next_cursor))
            },
        );
        assert_eq!(kinds(&one.events), [(id.clone(), WorkerEventKind::Joined)]);
        assert!(one.more);
        let rest = read(supervisor, &params(&owner, Some(&one.next_cursor)));
        assert_eq!(asked(&rest.events), ["perm-1", "perm-2"]);

        // Reading answered nothing: both still wait, and the owner owes them.
        assert_eq!(supervisor.status(&id).unwrap().questions.len(), 2);
        assert_eq!(supervisor.obligations(Some(PANE)).len(), 1);
        let again = read(
            supervisor,
            &WorkerEventsParams {
                snapshot: true,
                ..params(&owner, Some(&batch.next_cursor))
            },
        );
        assert!(again.events.is_empty());
        assert_eq!(again.next_cursor, batch.next_cursor);
        assert_eq!(again.snapshot.unwrap().workers[0].questions.len(), 2);

        fixture
            .answer_request(&id, "perm-1", WorkerDecision::Allow)
            .unwrap();
        fixture
            .answer_request(&id, "perm-2", WorkerDecision::Deny)
            .unwrap();
        let (ended, _) = gather(
            supervisor,
            &owner,
            &batch.next_cursor,
            || {},
            |events| has(events, &id, WorkerEventKind::TurnEnd),
        );
        assert!(asked(&ended).is_empty(), "{ended:#?}");
        assert_eq!(
            ended.last().unwrap().state,
            Some(WorkerState::Finished),
            "{ended:#?}"
        );
    }

    #[test]
    fn events_while_the_coordinator_is_busy_wait_for_its_next_wait() {
        let (fixture, owner) = coordinated("inbox-busy");
        let supervisor = &fixture.supervisor;
        let start = read(supervisor, &params(&owner, None)).next_cursor;
        let a = start_owned(&fixture, supervisor, "finish");
        fixture.wait(&a, WorkerWaitUntil::TurnEnd);
        let (drained, _) = wait(supervisor, &owner, &start, || {});
        assert_eq!(
            kinds(&drained.events),
            [
                (a.clone(), WorkerEventKind::Joined),
                (a.clone(), WorkerEventKind::TurnEnd)
            ]
        );

        // Between the drain and the re-arm, and while the coordinator's
        // turn is busy: a turn ends and another worker joins and asks.
        supervisor.prompt(&a, "finish").unwrap();
        fixture.wait(&a, WorkerWaitUntil::TurnEnd);
        let b = start_owned(&fixture, supervisor, "perm WebFetch https://example.com");
        fixture.wait_for_question(&b);

        let (rearmed, blocked) = wait(supervisor, &owner, &drained.next_cursor, || {});
        assert_eq!(blocked, 0, "the waiting events answer at once");
        assert_eq!(
            kinds(&rearmed.events),
            [
                (a.clone(), WorkerEventKind::TurnEnd),
                (b.clone(), WorkerEventKind::Joined),
                (b.clone(), WorkerEventKind::Question)
            ]
        );
        assert!(rearmed
            .events
            .windows(2)
            .all(|pair| pair[0].seq < pair[1].seq));
    }

    #[test]
    fn a_worker_started_and_one_exiting_during_a_wait_wake_it() {
        let (fixture, owner) = coordinated("inbox-membership");
        let supervisor = &fixture.supervisor;
        let a = start_owned(&fixture, supervisor, "finish");
        fixture.wait(&a, WorkerWaitUntil::TurnEnd);
        let first = read(supervisor, &params(&owner, None));
        assert_eq!(ids(&first.snapshot.unwrap().workers), [a.as_str()]);

        // Started during the wait: it joins.
        let mut b = None;
        let (events, blocked) = wait(supervisor, &owner, &first.next_cursor, || {
            b = Some(start_owned(&fixture, supervisor, "block"));
        });
        let b = b.unwrap();
        assert_eq!(blocked, 1);
        assert_eq!(
            kinds(&events.events)[0],
            (b.clone(), WorkerEventKind::Joined)
        );

        // Exiting during the wait: its exit wakes it.
        let (exited, cursor) = gather(
            supervisor,
            &owner,
            &events.next_cursor,
            || {
                supervisor.stop(&a).unwrap();
            },
            |events| has(events, &a, WorkerEventKind::Exit),
        );
        let exit = exited
            .iter()
            .find(|event| event.kind == WorkerEventKind::Exit)
            .unwrap();
        assert_eq!(exit.state, Some(WorkerState::Exited));

        // Its owner acknowledges its end: it leaves.
        let seq = supervisor.status(&a).unwrap().seq.unwrap();
        supervisor.ack(&a, seq).unwrap();
        let (left, _) = wait(supervisor, &owner, &cursor, || {});
        assert_eq!(kinds(&left.events), [(a.clone(), WorkerEventKind::Left)]);
        supervisor.kill(&b, false).unwrap();
    }

    #[test]
    fn a_restart_or_handoff_keeps_the_sequence_and_an_unservable_cursor_resyncs() {
        let (fixture, owner) = coordinated("inbox-restart");
        let a = start_owned(&fixture, &fixture.supervisor, "finish");
        fixture.wait(&a, WorkerWaitUntil::TurnEnd);
        fixture.supervisor.stop(&a).unwrap();
        fixture.wait(&a, WorkerWaitUntil::Exit);
        let cursor = read(&fixture.supervisor, &params(&owner, None)).next_cursor;

        // The next server opens the same store: the cursor is served.
        let next = WorkerSupervisor::open_with(
            fixture.root.join("workers"),
            fixture.root.join("claude-stub"),
            None,
        );
        let served = read(&next, &params(&owner, Some(&cursor)));
        assert!(!served.resync_required);
        assert!(served.events.is_empty());
        assert_eq!(served.next_cursor, cursor);
        let b = start_owned(&fixture, &next, "finish");
        let (joined, _) = wait(&next, &owner, &cursor, || {});
        assert_eq!(joined.events[0].kind, WorkerEventKind::Joined);
        assert_eq!(joined.events[0].worker_id, b);
        let (incarnation, seq) = cursor.rsplit_once('-').unwrap();
        assert!(joined.events[0].seq > seq.parse::<i64>().unwrap());
        next.stop(&b).ok();
        let started = Instant::now();
        next.wait(&b, WorkerWaitUntil::Exit, HANG_GUARD, || {
            assert!(started.elapsed() < HANG_GUARD, "worker {b} hung");
            true
        })
        .unwrap();

        // Another store's cursor, or one past the latest event, is not
        // served: resync, with the snapshot and a cursor to go on from.
        for unservable in ["ffff0000-1".to_owned(), format!("{incarnation}-999999999")] {
            let resync = read(&next, &params(&owner, Some(&unservable)));
            assert!(resync.resync_required, "{unservable}");
            assert!(resync.events.is_empty());
            let workers = resync.snapshot.unwrap().workers;
            assert_eq!(ids(&workers), [a.clone(), b.clone()], "{unservable}");
            assert!(resync.next_cursor.starts_with(&format!("{incarnation}-")));
            // A resync answers a wait at once.
            let (waited, blocked) = wait(&next, &owner, &unservable, || {});
            assert!(waited.resync_required);
            assert_eq!(blocked, 0);
        }
        // Nor is one older than the inbox.
        store_of(&next)
            .connection()
            .execute(
                "UPDATE meta SET value = '999999998' WHERE key = 'inbox_from'",
                [],
            )
            .unwrap();
        assert!(read(&next, &params(&owner, Some(&cursor))).resync_required);
        // A malformed cursor is the caller's error.
        let malformed = next
            .events(&params(&owner, Some("not a cursor")), HANG_GUARD, || true)
            .err()
            .unwrap();
        assert_eq!(malformed.code(), "invalid_request");
    }

    #[test]
    fn re_owning_during_a_wait_tells_the_old_owner_and_the_new_one_gets_the_question() {
        let (fixture, owner) = coordinated("inbox-reown");
        let supervisor = &fixture.supervisor;
        let a = start_owned(&fixture, supervisor, "perm WebFetch https://example.com");
        fixture.wait_for_question(&a);
        let cursor = read(supervisor, &params(&owner, None)).next_cursor;

        let mut next = None;
        let (moved, blocked) = wait(supervisor, &owner, &cursor, || {
            next = Some(
                supervisor
                    .coordinator_handoff(None, Some(PANE), "p-next", Some("s-next"), None)
                    .unwrap(),
            );
        });
        let next = next.unwrap();
        assert_eq!(blocked, 1);
        assert_eq!(
            kinds(&moved.events),
            [(a.clone(), WorkerEventKind::Reowned)]
        );
        assert_eq!(
            moved.events[0].to_coordinator_id.as_deref(),
            Some(next.coordinator_id.as_str())
        );
        assert!(moved.owner.ended_ms.is_some());

        // The old tenure has ended: its wait answers at once, with nothing.
        let (after, blocked) = wait(supervisor, &owner, &moved.next_cursor, || {});
        assert!(after.events.is_empty());
        assert_eq!(blocked, 0);

        // The new owner: the worker joined it, and its snapshot carries the
        // question asked before it existed, still pending.
        let joined = read(supervisor, &params(&next.coordinator_id, Some(&cursor)));
        assert_eq!(
            kinds(&joined.events),
            [(a.clone(), WorkerEventKind::Joined)]
        );
        assert_eq!(
            joined.events[0].from_coordinator_id.as_deref(),
            Some(owner.as_str())
        );
        let fresh = read(
            supervisor,
            &WorkerEventsParams {
                owner_pane_id: Some("p-next".into()),
                ..WorkerEventsParams::default()
            },
        );
        assert_eq!(fresh.owner.coordinator_id, next.coordinator_id);
        let workers = fresh.snapshot.unwrap().workers;
        assert_eq!(ids(&workers), [a.as_str()]);
        assert_eq!(workers[0].questions[0].request_id, "perm-1");
        supervisor.kill(&a, false).unwrap();
    }
}
