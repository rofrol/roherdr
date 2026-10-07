//! Supervisor tests against a stub `claude` that speaks stream-json. No real
//! Claude runs here. The stub's behavior is chosen by each user message:
//! `finish`, `fail`, `crash`, `block` (until an interrupt), `perm <tool>
//! <path>` (one `can_use_tool` request) and `orphan <fifo>` (a tool process
//! in its own session that holds `<fifo>` open until it dies).
#![cfg(unix)]

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::*;

const STUB: &str = r#"#!/usr/bin/env python3
import json, os, subprocess, sys

def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()

def result(subtype="success", is_error=False, reason="completed", text="ok"):
    emit({"type": "result", "subtype": subtype, "is_error": is_error,
          "terminal_reason": reason, "api_error_status": None, "result": text,
          "session_id": "stub-session"})

emit({"type": "system", "subtype": "init", "session_id": "stub-session",
      "herdr_env": sorted(k for k in os.environ if k.startswith("HERDR_"))})

def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)

while True:
    message = read()
    if message.get("type") != "user":
        continue
    words = message["message"]["content"].split()
    emit({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}})
    command = words[0]
    if command == "finish":
        result()
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
    elif command == "perm":
        emit({"type": "control_request", "request_id": "perm-1", "request": {
            "subtype": "can_use_tool", "tool_name": words[1],
            "input": {"file_path": words[2], "command": words[2]}}})
        answer = read()
        response = answer["response"]
        assert response["request_id"] == "perm-1", answer
        result(text=response["response"]["behavior"])
    elif command == "orphan":
        subprocess.Popen([sys.executable, "-c",
            "import sys, time; f = open(sys.argv[1], 'w'); time.sleep(1000)", words[1]],
            start_new_session=True, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        # The tool runs; this event lets the supervisor see its session.
        emit({"type": "assistant", "message": {"content": [{"type": "tool_use"}]}})
        while True:
            read()
"#;

/// Fails a broken test instead of hanging the suite; never decides an
/// outcome a passing test depends on.
const HANG_GUARD: Duration = Duration::from_secs(60);

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    supervisor: WorkerSupervisor,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-workers-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let stub = root.join("claude-stub");
        std::fs::write(&stub, STUB).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let supervisor = WorkerSupervisor::open(root.join("workers"), stub);
        Self {
            root,
            repo,
            supervisor,
        }
    }

    fn start(&self, prompt: &str) -> String {
        self.supervisor
            .start(&self.repo.display().to_string(), prompt, Some("stub-model"))
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

impl Drop for Fixture {
    fn drop(&mut self) {
        for worker in self.supervisor.list() {
            if !matches!(worker.state, WorkerState::Exited | WorkerState::Lost) {
                let _ = self.supervisor.kill(&worker.worker_id);
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
    assert!(args.contains(&r#"{"disableAllHooks":true}"#));
    let init = journal
        .iter()
        .find(|record| record["event"]["subtype"] == "init")
        .unwrap();
    assert_eq!(init["event"]["herdr_env"], serde_json::json!([]));
    assert!(journal.iter().any(|record| record["dir"] == "in"));
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
    fixture.supervisor.interrupt(&id).unwrap();

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
fn file_tools_inside_are_allowed_and_everything_else_denied() {
    let fixture = Fixture::new("permissions");
    let outside = fixture.root.join("outside.txt").display().to_string();
    for (prompt, expected) in [
        ("perm Write inside.txt".to_owned(), "allow"),
        (format!("perm Write {outside}"), "deny"),
        ("perm Bash touch".to_owned(), "deny"),
    ] {
        let id = fixture.start(&prompt);
        let worker = fixture.wait(&id, WorkerWaitUntil::TurnEnd);
        assert_eq!(
            worker.last_result.unwrap().text.as_deref(),
            Some(expected),
            "{prompt}"
        );
        let decisions = fixture.herdr_events(&id, "permission");
        assert_eq!(decisions.len(), 1, "{prompt}");
        assert_eq!(decisions[0]["decision"], expected, "{prompt}");
        if expected == "deny" {
            assert!(decisions[0]["message"]
                .as_str()
                .unwrap()
                .contains("herdr worker policy"));
        }
    }
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
fn stop_terminates_an_idle_worker() {
    let fixture = Fixture::new("stop");
    let id = fixture.start("finish");
    fixture.wait(&id, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.stop(&id).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Exited);
    assert_eq!(fixture.herdr_events(&id, "signal")[0]["signal"], "SIGTERM");
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

    fixture.supervisor.kill(&id).unwrap();
    let worker = fixture.wait(&id, WorkerWaitUntil::Exit);
    assert_eq!(worker.exit_signal, Some(libc::SIGKILL));
    started = Instant::now();
    let mut buffer = Vec::new();
    tool.read_to_end(&mut buffer).unwrap();
    assert!(started.elapsed() < HANG_GUARD);
    let killed = fixture.herdr_events(&id, "killed_tool_processes");
    assert!(!killed[0]["pids"].as_array().unwrap().is_empty());
}

fn write_journal(dir: &Path, worker_id: &str, lines: &[Value]) {
    std::fs::create_dir_all(dir).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(dir.join(format!("{worker_id}.jsonl")), text).unwrap();
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
        .start(&fixture.repo.display().to_string(), "finish", None)
        .unwrap();
    assert_eq!(next.worker_id, "w8");
    supervisor.kill("w8").unwrap();
}
