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
//! ignored from then on) and `orphan <fifo>` (a tool process in its own
//! session that holds `<fifo>` open until it dies).
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
    elif command == "done":
        result(text="work done\nWORKER-DONE " + words[1] + " | summary")
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

impl Drop for Fixture {
    fn drop(&mut self) {
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

#[test]
fn an_exited_worker_is_taken_over_once() {
    let fixture = Fixture::new("takeover-exited");
    let id = fixture.start("crash");
    fixture.wait(&id, WorkerWaitUntil::Exit);
    fixture.supervisor.begin_takeover(&id).unwrap();
    let worker = fixture.supervisor.end_for_takeover(&id).unwrap();
    assert!(worker.takeover_ms.is_some());
    let running = fixture.supervisor.begin_takeover(&id).unwrap_err();
    assert_eq!(running.code(), "worker_busy");
    assert!(running.to_string().contains("being taken over"));

    fixture
        .supervisor
        .takeover_tab_opened(&id, "tab_9")
        .unwrap();
    let done = fixture.supervisor.begin_takeover(&id).unwrap_err();
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
    fixture.supervisor.begin_takeover(&id).unwrap();
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
    fixture.supervisor.begin_takeover(&id).unwrap();
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
    fixture.supervisor.begin_takeover(&id).unwrap();
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
        supervisor.begin_takeover("w2").unwrap_err().code(),
        "worker_busy"
    );

    // Claimed, but the tab was never recorded: reported, and open to retry.
    let unfinished = supervisor.status("w3").unwrap();
    assert_eq!(unfinished.takeover_ms, Some(5));
    assert!(unfinished.takeover_unfinished);
    let summary = supervisor
        .summaries()
        .into_iter()
        .find(|summary| summary.worker_id == "w3")
        .unwrap();
    assert!(!summary.takeover);
    supervisor.begin_takeover("w3").unwrap();
    assert!(!supervisor.status("w3").unwrap().takeover_unfinished);

    supervisor.begin_takeover("w1").unwrap();
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
    supervisor.begin_takeover("w2").unwrap();
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
        let one = scope.spawn(|| fixture.supervisor.begin_takeover(&id));
        let two = scope.spawn(|| fixture.supervisor.begin_takeover(&id));
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
    fixture.wait(&owned, WorkerWaitUntil::TurnEnd);
    fixture.wait(&crashed, WorkerWaitUntil::Exit);
    assert!(listed(&owned), "a running worker is listed");
    // The crash is not hidden while its owner has not handled it.
    assert!(listed(&crashed));
    assert_eq!(
        fixture.supervisor.item_counts(),
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
        fixture.supervisor.item_counts(),
        ItemCounts {
            in_progress: 0,
            attention: 1
        }
    );
    let gone = obligation_of(&fixture, "p1", &owned).unwrap();
    fixture.supervisor.ack(&owned, gone.seq).unwrap();
    assert!(!listed(&owned));
    assert_eq!(fixture.supervisor.item_counts(), ItemCounts::default());

    // Nobody acknowledges a worker without an owner: it leaves at its end.
    fixture.wait(&loose, WorkerWaitUntil::TurnEnd);
    assert!(listed(&loose));
    fixture.supervisor.stop(&loose).unwrap();
    fixture.wait(&loose, WorkerWaitUntil::Exit);
    assert!(!listed(&loose));
}
