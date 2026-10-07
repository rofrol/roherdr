//! Supervisor tests against a stub `claude` that speaks stream-json. No real
//! Claude runs here. The stub's behavior is chosen by each user message:
//! `finish`, `fail`, `crash`, `block` (until an interrupt), `perm <tool>
//! <words...>` (one `can_use_tool` request whose path or command is the
//! words), `ask` (an `AskUserQuestion` request), `ignore-term` (SIGTERM is
//! ignored from then on) and `orphan <fifo>` (a tool process in its own
//! session that holds `<fifo>` open until it dies).
#![cfg(unix)]

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::*;
use crate::api::schema::{WorkerAnswerParams, WorkerDecision, WorkerQuestionKind};

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
      "herdr_env": sorted(k for k in os.environ if k.startswith("HERDR_"))})

ignore_term = False

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

def ask_host(tool, tool_input):
    emit({"type": "control_request", "request_id": "perm-1", "request": {
        "subtype": "can_use_tool", "tool_name": tool, "input": tool_input}})
    answer = read()
    response = answer["response"]
    assert response["request_id"] == "perm-1", answer
    return response["response"]

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
        rest = " ".join(words[2:])
        response = ask_host(words[1], {"file_path": rest, "command": rest})
        text = response["behavior"]
        if text == "deny":
            text += ": " + response["message"]
        result(text=text)
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
fn the_policy_answers_the_requests_it_decides() {
    let fixture = Fixture::new("permissions");
    let outside = fixture.root.join("outside.txt").display().to_string();
    for (prompt, expected) in [
        ("perm Write inside.txt".to_owned(), "allow"),
        (format!("perm Write {outside}"), "deny"),
        ("perm Bash git status".to_owned(), "allow"),
        ("perm Bash cargo test -p herdr".to_owned(), "allow"),
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
    let id = fixture.start("perm Bash git push origin master");

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
    let answers = fixture.herdr_events(&id, "answer");
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["decision"], "allow");
    assert_eq!(answers[0]["by"], "user");

    let none = fixture
        .answer(&id, Some(WorkerDecision::Allow), &[])
        .unwrap_err();
    assert_eq!(none.code(), "worker_no_question");
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
        fixture.herdr_events(&id, "answer")[0]["answers"]["Which file?"],
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
    let id = fixture.start("perm Bash rm -rf target");
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

    fixture.supervisor.kill(&id).unwrap();
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
