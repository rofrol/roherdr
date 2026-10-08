//! Supervisor tests against a stub `claude` that speaks stream-json. No real
//! Claude runs here. The stub's behavior is chosen by each user message:
//! `finish`, `fail`, `crash`, `block` (until an interrupt), `perm <tool>
//! <words...>` (one `can_use_tool` request whose path or command is the
//! words), `classifier <tool> <words...>` (the same, escalated by the auto
//! mode classifier), `refuse` (a model refusal, then `result/success`),
//! `refuse-wait` (a refusal, then the next input line, then the result),
//! `exit1` (`result/success`, then exit code 1), `denials` (a result with
//! `permission_denials`), `ask` (an `AskUserQuestion` request), `pair <tool>
//! <words...>` (two requests at once, `perm-1` and `perm-2`), `cancel <tool>
//! <words...>` (`perm-1`, cancelled, then `perm-2`), `ignore-term` (SIGTERM is
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
      "herdr_env": sorted(k for k in os.environ if k.startswith("HERDR_")),
      "cwd": os.getcwd(),
      "cache_env": {k: os.environ.get(k) for k in
                    ("CARGO_TARGET_DIR", "ZIG_GLOBAL_CACHE_DIR", "ZIG_LOCAL_CACHE_DIR")}})

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

def ask_host(tool, tool_input, reason_type=None):
    request = {"subtype": "can_use_tool", "tool_name": tool, "input": tool_input}
    if reason_type:
        request["decision_reason_type"] = reason_type
    emit({"type": "control_request", "request_id": "perm-1", "request": request})
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
    let init = journal
        .iter()
        .find(|record| record["event"]["subtype"] == "init")
        .unwrap();
    assert_eq!(init["event"]["herdr_env"], serde_json::json!([]));
    assert!(journal.iter().any(|record| record["dir"] == "in"));
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
    let (number, live, _) = fixture.supervisor.live(&id).unwrap();
    let message = user_message("meanwhile");
    live.send(&message).unwrap();
    fixture.supervisor.update(number, Direction::In, &message);
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
        .start(&start_params(&fixture.repo, "finish", None))
        .unwrap();
    assert_eq!(next.worker_id, "w8");
    supervisor.kill("w8").unwrap();
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
    let takeover = fixture.supervisor.begin_takeover(&id).unwrap();
    assert_eq!(takeover.session_id, "stub-session");
    assert!(matches!(
        fixture.supervisor.begin_takeover(&id),
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
        fixture.supervisor.begin_takeover(&id),
        Err(WorkerError::Busy(_))
    ));
    assert_eq!(fixture.supervisor.status(&id).unwrap().takeover_ms, None);
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
