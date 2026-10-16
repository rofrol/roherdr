//! `worker.run`: the gated, blocking headless run an external orchestrator
//! uses, against the stub `claude`.

use super::todo_runs::{fresh_usage, params as todo_params, todo_repo};
use super::*;
use crate::api::schema::{
    ResponseResult, WorkerGatedRun, WorkerGatedRunOutcome, WorkerGatedRunParams,
};
use crate::workers::supervisor::gated_run::GatedRunEndFailure;

fn run_params(dir: &Path, prompt: &str) -> WorkerGatedRunParams {
    WorkerGatedRunParams {
        cwd: dir.display().to_string(),
        prompt: prompt.to_owned(),
        agent: None,
        model: Some("stub-model".into()),
        name: None,
        workspace_id: None,
        item: None,
        ignore_usage: false,
    }
}

/// `worker.run` to its end; the liveness check is as long as the hang
/// guard, so a missed wake fails the test instead of deciding it.
fn gated_run(
    fixture: &Fixture,
    params: &WorkerGatedRunParams,
) -> Result<WorkerGatedRun, WorkerError> {
    let started = Instant::now();
    fixture
        .supervisor
        .gated_run(params, HANG_GUARD, || {
            assert!(started.elapsed() < HANG_GUARD, "worker.run hung");
            true
        })
        .map(|run| run.expect("the run was not given up"))
}

/// A fixture whose `repo` is a git repository with one commit.
fn git_fixture(name: &str) -> Fixture {
    let fixture = Fixture::new(name);
    git_in(&fixture.repo, &["init", "-q", "-b", "master"]);
    std::fs::write(fixture.repo.join("a.txt"), "a\n").unwrap();
    git_in(&fixture.repo, &["add", "."]);
    git_in(&fixture.repo, &["commit", "-q", "-m", "init"]);
    fixture
}

#[test]
fn an_admitted_run_answers_with_the_turns_text_and_usage_per_model() {
    let fixture = git_fixture("gated-run-admitted");
    fixture.supervisor.set_usage_for_test(fresh_usage(10, 10));
    let run = gated_run(&fixture, &run_params(&fixture.repo, "usage")).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    assert_eq!(run.text.as_deref(), Some("final text"));
    assert_eq!(run.usage.len(), 1, "{:?}", run.usage);
    let usage = &run.usage[0];
    assert_eq!(usage.model, "stub-model");
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens
        ),
        (10, 5, 3, 2)
    );
    assert_eq!(usage.cost_usd, Some(0.25));
    assert_eq!(run.total_cost_usd, Some(0.25));
    assert_eq!(run.usage_gate["decision"], "admit");
    assert_eq!(run.usage_gate["ignore_usage"], false);
    assert!(run.question.is_none());

    // The worker is stopped and gone once the run answers.
    let worker = fixture.supervisor.status(&run.worker_id).unwrap();
    assert_eq!(worker.state, WorkerState::Exited);
    let started = &fixture.herdr_events(&run.worker_id, "started")[0];
    assert_eq!(started["folder_slot"], Value::Null);
    assert_eq!(
        started["cwd"],
        fixture.repo.canonicalize().unwrap().display().to_string()
    );
    let gate = &fixture.herdr_events(&run.worker_id, "usage_gate")[0];
    assert_eq!(gate["method"], "worker.run");
    assert_eq!(gate["decision"], "admit");
    assert_eq!(fixture.herdr_events(&run.worker_id, "signal").len(), 1);

    // The reply as the CLI prints it.
    let reply = serde_json::to_value(ResponseResult::WorkerGatedRun { run }).unwrap();
    assert_eq!(reply["type"], "worker_gated_run");
    assert_eq!(reply["run"]["outcome"], "ended");
    assert_eq!(reply["run"]["text"], "final text");
    assert_eq!(reply["run"]["usage"][0]["output_tokens"], 5);
}

#[test]
fn a_closed_gate_refuses_the_run_and_starts_nothing_unless_usage_is_ignored() {
    let fixture = git_fixture("gated-run-refused");
    let repo = repository_of(&fixture.repo).unwrap();
    fixture.supervisor.set_usage_for_test(fresh_usage(40, 91));
    let refused = gated_run(&fixture, &run_params(&fixture.repo, "usage")).unwrap_err();
    assert_eq!(refused.code(), "usage_gate", "{refused}");
    assert!(refused.to_string().contains("91% used"), "{refused}");
    assert!(fixture.supervisor.list().is_empty());
    assert!(store_of(&fixture.supervisor)
        .usage_gate_closed(&repo)
        .unwrap());

    // --ignore-usage starts anyway, records the override in the worker's
    // journal and leaves the gate as it was.
    let body = store_of(&fixture.supervisor)
        .usage_gate_body(&repo)
        .unwrap();
    let run = gated_run(
        &fixture,
        &WorkerGatedRunParams {
            ignore_usage: true,
            ..run_params(&fixture.repo, "finish")
        },
    )
    .unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    assert_eq!(run.usage_gate["decision"], "ignored");
    let gate = &fixture.herdr_events(&run.worker_id, "usage_gate")[0];
    assert_eq!(gate["decision"], "ignored");
    assert_eq!(gate["ignore_usage"], true);
    assert!(store_of(&fixture.supervisor)
        .usage_gate_closed(&repo)
        .unwrap());
    assert_eq!(
        store_of(&fixture.supervisor)
            .usage_gate_body(&repo)
            .unwrap(),
        body
    );
}

#[test]
fn the_run_leaves_the_gate_state_todo_run_leaves_for_the_same_reading() {
    let ours = git_fixture("gated-run-gate-state");
    let todo = todo_repo("gated-run-gate-state-todo");
    let our_repo = repository_of(&ours.repo).unwrap();
    let todo_repo_key = repository_of(&todo.repo).unwrap();
    let todo_params = todo_params(&todo, "finish", "ok");
    // A refusal closes the gate; a reading between the thresholds keeps it
    // closed; the same readings leave the same state through both.
    for (five_hour, weekly) in [(40, 91), (85, 10)] {
        ours.supervisor
            .set_usage_for_test(fresh_usage(five_hour, weekly));
        todo.supervisor
            .set_usage_for_test(fresh_usage(five_hour, weekly));
        let refused = gated_run(&ours, &run_params(&ours.repo, "finish")).unwrap_err();
        assert_eq!(refused.code(), "usage_gate", "{refused}");
        let todo_refused = todo.supervisor.todo_run(todo_params.clone()).unwrap_err();
        assert_eq!(todo_refused.code(), "usage_gate", "{todo_refused}");
        assert_eq!(refused.to_string(), todo_refused.to_string());
        let ours_store = store_of(&ours.supervisor);
        let todo_store = store_of(&todo.supervisor);
        assert!(ours_store.usage_gate_closed(&our_repo).unwrap());
        assert!(todo_store.usage_gate_closed(&todo_repo_key).unwrap());
        assert_eq!(
            ours_store.usage_gate_body(&our_repo).unwrap(),
            todo_store.usage_gate_body(&todo_repo_key).unwrap()
        );
    }
    // A reading below the reopen threshold admits and reopens.
    ours.supervisor.set_usage_for_test(fresh_usage(79, 79));
    let run = gated_run(&ours, &run_params(&ours.repo, "finish")).unwrap();
    assert_eq!(run.usage_gate["decision"], "admit");
    assert_eq!(run.usage_gate["reopened"], true);
    assert!(!store_of(&ours.supervisor)
        .usage_gate_closed(&our_repo)
        .unwrap());
}

#[test]
fn a_directory_worker_start_refuses_outside_git_or_in_a_folder_slot_is_refused() {
    let fixture = Fixture::new("gated-run-directories");
    // Not in a git repository: no gate to admit it.
    let plain = fixture.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let refused = gated_run(&fixture, &run_params(&plain, "finish")).unwrap_err();
    assert_eq!(refused.code(), "invalid_request", "{refused}");
    assert!(
        refused.to_string().contains("not in a git repository"),
        "{refused}"
    );
    // Refused as worker start refuses it: a relative directory, a missing
    // one and an empty prompt.
    for params in [
        WorkerGatedRunParams {
            cwd: "relative".into(),
            ..run_params(&fixture.repo, "finish")
        },
        run_params(&fixture.root.join("missing"), "finish"),
        run_params(&fixture.repo, "  "),
    ] {
        let refused = gated_run(&fixture, &params).unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");
    }
    assert!(fixture.supervisor.list().is_empty());

    // Inside herdr's folder slot, herdr's own checkout.
    let repo = &fixture.repo;
    git_in(repo, &["init", "-q", "-b", "master"]);
    std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git_in(repo, &["add", "."]);
    git_in(repo, &["commit", "-q", "-m", "init"]);
    let in_slot = fixture
        .supervisor
        .start(&WorkerStartParams {
            folder_slot: Some("worker".into()),
            branch: Some("w/one".into()),
            ..start_params(repo, "finish", None)
        })
        .unwrap();
    fixture.wait(&in_slot.worker_id, WorkerWaitUntil::TurnEnd);
    fixture.supervisor.stop(&in_slot.worker_id).unwrap();
    fixture.wait(&in_slot.worker_id, WorkerWaitUntil::Exit);
    let slot = PathBuf::from(&in_slot.cwd);
    for dir in [slot.clone(), slot.join("target")] {
        std::fs::create_dir_all(&dir).unwrap();
        let refused = gated_run(&fixture, &run_params(&dir, "finish")).unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");
        assert!(refused.to_string().contains("folder slot"), "{refused}");
    }
    // Only the slot's own worker ever started.
    assert_eq!(fixture.supervisor.list().len(), 1);
}

#[test]
fn a_question_the_policy_leaves_to_the_user_ends_the_run_unanswered() {
    let fixture = git_fixture("gated-run-question");
    let run = gated_run(
        &fixture,
        &run_params(&fixture.repo, "perm WebFetch https://example.com"),
    )
    .unwrap();
    assert_eq!(
        run.outcome,
        WorkerGatedRunOutcome::PolicyRefused,
        "{}",
        run.message
    );
    assert!(run.message.contains("WebFetch"), "{}", run.message);
    let question = run.question.expect("the question that ended the run");
    assert_eq!(question.tool_name, "WebFetch");
    let worker = fixture.supervisor.status(&run.worker_id).unwrap();
    assert_eq!(worker.state, WorkerState::Exited);
    assert!(worker.questions.is_empty(), "{:?}", worker.questions);
}

#[test]
fn a_failed_turn_is_reported_failed() {
    let fixture = git_fixture("gated-run-failed");
    let run = gated_run(&fixture, &run_params(&fixture.repo, "fail")).unwrap();
    assert_eq!(
        run.outcome,
        WorkerGatedRunOutcome::Failed,
        "{}",
        run.message
    );
    let crashed = gated_run(&fixture, &run_params(&fixture.repo, "crash")).unwrap();
    assert_eq!(
        crashed.outcome,
        WorkerGatedRunOutcome::Failed,
        "{}",
        crashed.message
    );
    assert!(crashed.message.contains("code 3"), "{}", crashed.message);
}

#[test]
fn a_caller_that_gives_up_leaves_no_worker_running() {
    let fixture = git_fixture("gated-run-given-up");
    // The caller is gone at the first point the run would block.
    let given_up = fixture
        .supervisor
        .gated_run(&run_params(&fixture.repo, "block"), HANG_GUARD, || false)
        .unwrap();
    assert!(given_up.is_none());
    let workers = fixture.supervisor.list();
    assert_eq!(workers.len(), 1);
    let worker = fixture.wait(&workers[0].worker_id, WorkerWaitUntil::Exit);
    assert!(worker.exit_signal.is_some() || worker.exit_code.is_some());
    assert_eq!(fixture.herdr_events(&worker.worker_id, "signal").len(), 1);
}

#[test]
fn a_folder_slot_no_worker_ran_in_is_refused_by_its_path() {
    let fixture = git_fixture("gated-run-unused-slot");
    let repo = &fixture.repo;
    // Where `todo.run`'s slot goes now, and the earlier place beside the
    // repositories, both worktrees of this repository no worker ran in.
    let slot = fixture.root.join("herdr-worktrees/repo").join(runs::SLOT);
    let legacy = fixture.root.join("herdr-worktrees").join(runs::SLOT);
    for dir in [&slot, &legacy] {
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        let path = dir.display().to_string();
        git_in(repo, &["worktree", "add", "-q", "--detach", &path]);
    }
    assert!(fixture.supervisor.list().is_empty());
    for dir in [slot.clone(), slot.join("src"), legacy.clone()] {
        std::fs::create_dir_all(&dir).unwrap();
        let refused = gated_run(&fixture, &run_params(&dir, "finish")).unwrap_err();
        assert_eq!(refused.code(), "invalid_request", "{refused}");
        assert!(refused.to_string().contains("folder slot"), "{refused}");
    }
    assert!(fixture.supervisor.list().is_empty());

    // A worktree of another name beside them is the caller's.
    let own = fixture.root.join("herdr-worktrees/repo/task");
    git_in(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &own.display().to_string(),
        ],
    );
    let run = gated_run(&fixture, &run_params(&own, "finish")).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
}

#[test]
fn a_stop_that_fails_while_the_worker_runs_kills_it_instead_of_waiting() {
    let fixture = git_fixture("gated-run-stop-fails");
    fixture
        .supervisor
        .set_gated_run_end_failure_for_test(GatedRunEndFailure::Stop);
    let run = gated_run(&fixture, &run_params(&fixture.repo, "finish")).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    let worker = fixture.supervisor.status(&run.worker_id).unwrap();
    assert_eq!(worker.state, WorkerState::Exited);
    let signals = fixture.herdr_events(&run.worker_id, "signal");
    assert_eq!(signals.len(), 1, "{signals:?}");
    assert_eq!(signals[0]["signal"], "SIGKILL");

    // When the kill fails too, the caller gets the error naming the worker.
    fixture
        .supervisor
        .set_gated_run_end_failure_for_test(GatedRunEndFailure::StopAndKill);
    let failed = gated_run(&fixture, &run_params(&fixture.repo, "finish")).unwrap_err();
    let left = fixture
        .supervisor
        .list()
        .into_iter()
        .find(|worker| worker.worker_id != run.worker_id)
        .expect("the second run's worker");
    assert!(failed.to_string().contains(&left.worker_id), "{failed}");
    assert!(failed.to_string().contains("nor kill it"), "{failed}");
    fixture
        .supervisor
        .set_gated_run_end_failure_for_test(GatedRunEndFailure::None);
    fixture.supervisor.kill(&left.worker_id, false).unwrap();
    fixture.wait(&left.worker_id, WorkerWaitUntil::Exit);
}
