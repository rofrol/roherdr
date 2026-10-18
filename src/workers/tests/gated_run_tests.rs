//! `worker.run`: the gated, blocking headless run an external orchestrator
//! uses, against the stub `claude`.

use super::todo_runs::{fresh_usage, params as todo_params, todo_repo};
use super::*;
use crate::api::schema::{
    ResponseResult, WorkerGatedRun, WorkerGatedRunOutcome, WorkerGatedRunParams,
};
use crate::workers::supervisor::gated_run::GatedRunEndFailure;

/// The Claude account's gate row: one for every repository.
const CLAUDE_GATE: &str = "provider:claude";

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
        tenure_token: None,
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
    fixture.supervisor.set_usage_for_test(fresh_usage(40, 91));
    let refused = gated_run(&fixture, &run_params(&fixture.repo, "usage")).unwrap_err();
    assert_eq!(refused.code(), "usage_gate", "{refused}");
    assert!(refused.to_string().contains("91% used"), "{refused}");
    assert!(fixture.supervisor.list().is_empty());
    assert!(store_of(&fixture.supervisor)
        .usage_gate_closed(CLAUDE_GATE)
        .unwrap());

    // --ignore-usage starts anyway, records the override in the worker's
    // journal and leaves the gate as it was.
    let body = store_of(&fixture.supervisor)
        .usage_gate_body(CLAUDE_GATE)
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
        .usage_gate_closed(CLAUDE_GATE)
        .unwrap());
    assert_eq!(
        store_of(&fixture.supervisor)
            .usage_gate_body(CLAUDE_GATE)
            .unwrap(),
        body
    );
}

#[test]
fn the_run_leaves_the_gate_state_todo_run_leaves_for_the_same_reading() {
    let ours = git_fixture("gated-run-gate-state");
    let todo = todo_repo("gated-run-gate-state-todo");
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
        assert!(ours_store.usage_gate_closed(CLAUDE_GATE).unwrap());
        assert!(todo_store.usage_gate_closed(CLAUDE_GATE).unwrap());
        assert_eq!(
            ours_store.usage_gate_body(CLAUDE_GATE).unwrap(),
            todo_store.usage_gate_body(CLAUDE_GATE).unwrap()
        );
    }
    // A reading below the reopen threshold admits and reopens.
    ours.supervisor.set_usage_for_test(fresh_usage(79, 79));
    let run = gated_run(&ours, &run_params(&ours.repo, "finish")).unwrap();
    assert_eq!(run.usage_gate["decision"], "admit");
    assert_eq!(run.usage_gate["reopened"], true);
    assert!(!store_of(&ours.supervisor)
        .usage_gate_closed(CLAUDE_GATE)
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
fn a_dropped_worker_run_connection_records_termination_and_frees_the_slot() {
    use interprocess::local_socket::traits::Listener;
    use std::sync::atomic::AtomicBool;
    // The broker fixture has a short root suitable for Unix socket paths.
    let fixture = Fixture::with_broker();
    git_in(&fixture.repo, &["init", "-q", "-b", "master"]);
    std::fs::write(fixture.repo.join("a.txt"), "a\n").unwrap();
    git_in(&fixture.repo, &["add", "."]);
    git_in(&fixture.repo, &["commit", "-q", "-m", "init"]);
    fixture
        .supervisor
        .set_usage_gate_config_for_test(crate::config::UsageConfig {
            gate_max_workers: 2,
            ..Default::default()
        });
    let repo = fixture.repo.display().to_string();
    let holder = crate::workers::runs::usage_gate::Holder::WorkerRun("blocker".into());
    fixture
        .supervisor
        .usage_gate(&repo, WorkerAgent::Claude, None, false, Some(&holder), None)
        .unwrap();
    // A real API connection, closed only after the worker was recorded.
    let path = fixture.root.join("api.sock");
    let listener = crate::ipc::bind_local_listener(&path).unwrap();
    let client = crate::ipc::connect_local_stream(&path).unwrap();
    let mut stream = listener.accept().unwrap();
    let params = run_params(&fixture.repo, "block");
    let supervisor = fixture.supervisor.clone();
    let connection = std::thread::spawn(move || {
        crate::workers::set_test_supervisor(supervisor);
        crate::api::handle_worker_request(
            "dropped-run".into(),
            crate::api::schema::Method::WorkerGatedRun(params),
            &mut stream,
            &Arc::new(AtomicBool::new(true)),
        )
    });
    let mut registry = lock(&fixture.supervisor.shared.registry);
    while registry.workers.is_empty() {
        let waited = fixture
            .supervisor
            .shared
            .changed
            .wait_timeout(registry, HANG_GUARD)
            .unwrap();
        registry = waited.0;
        assert!(!waited.1.timed_out(), "worker was not recorded");
    }
    drop(registry);
    let third = crate::workers::runs::usage_gate::Holder::WorkerRun("third".into());
    assert_eq!(
        fixture
            .supervisor
            .usage_gate(&repo, WorkerAgent::Claude, None, false, Some(&third), None,)
            .unwrap_err()
            .code(),
        "usage_gate"
    );
    drop(client);
    assert!(connection.join().unwrap().is_none());
    let workers = fixture.supervisor.list();
    assert_eq!(workers.len(), 1);
    let worker = fixture.wait(&workers[0].worker_id, WorkerWaitUntil::Exit);
    assert_eq!(worker.state, WorkerState::Exited);
    assert!(worker.exit_signal.is_some() || worker.exit_code.is_some());
    assert_eq!(fixture.herdr_events(&worker.worker_id, "signal").len(), 1);
    assert_eq!(fixture.herdr_events(&worker.worker_id, "exited").len(), 1);
    // The termination is durable, not only a freed slot: the worker's row
    // records the exit and its end event is in the journal.
    let (exited, lost, exit_code): (bool, bool, Option<i64>) = store_of(&fixture.supervisor)
        .connection()
        .query_row(
            "SELECT exited, lost, exit_code FROM workers WHERE id = ?1",
            [&worker.worker_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(exited && !lost, "exited={exited} lost={lost}");
    assert_eq!(exit_code, worker.exit_code.map(i64::from));
    assert_eq!(fixture.supervisor.admissions(&repo).unwrap().len(), 1);
    fixture
        .supervisor
        .usage_gate(&repo, WorkerAgent::Claude, None, false, Some(&third), None)
        .unwrap();
    assert_eq!(fixture.supervisor.admissions(&repo).unwrap().len(), 2);
    // The ordinary JSON/outcome path remains covered above and still
    // works after the disconnected run has relinquished its slot.
    store_of(&fixture.supervisor)
        .release_admission(&third.key(), "test released", now_ms())
        .unwrap();
    let run = gated_run(&fixture, &run_params(&fixture.repo, "usage")).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended);
    let reply = serde_json::to_value(ResponseResult::WorkerGatedRun { run }).unwrap();
    assert_eq!(reply["run"]["text"], "final text");
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

#[test]
fn an_active_tenure_admits_only_a_run_with_its_current_token() {
    let fixture = git_fixture("gated-run-tenure-token");
    let repo = repository_of(&fixture.repo).unwrap();
    let with_token = |token: Option<&str>| WorkerGatedRunParams {
        tenure_token: token.map(str::to_owned),
        ..run_params(&fixture.repo, "finish")
    };
    // No tenure: a run is admitted with or without a token, as before.
    for token in [None, Some("anything")] {
        let run = gated_run(&fixture, &with_token(token)).unwrap();
        assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    }
    let started = fixture.supervisor.list().len();

    // A pane holds the repository and was given a token.
    fixture
        .supervisor
        .coordinator_start(&repo, "p1", None)
        .unwrap();
    let (tenure, token) = fixture.supervisor.coordinator_token_for_pane("p1").unwrap();
    for (given, what) in [
        (None, "none was given"),
        (Some("wrong"), "not that tenure's"),
    ] {
        let refused = gated_run(&fixture, &with_token(given)).unwrap_err();
        assert_eq!(refused.code(), "tenure_token_refused", "{refused}");
        let message = refused.to_string();
        assert!(message.contains(what), "{message}");
        assert!(
            message.contains(&tenure.coordinator_id) && message.contains("p1"),
            "{message}"
        );
    }
    assert_eq!(
        fixture.supervisor.list().len(),
        started,
        "a refusal starts nothing"
    );
    let run = gated_run(&fixture, &with_token(Some(&token))).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);

    // Released and acquired again: the earlier token is stale.
    fixture
        .supervisor
        .coordinator_end(
            &tenure.coordinator_id,
            crate::workers::coordinators::ENDED,
            None,
        )
        .unwrap();
    fixture
        .supervisor
        .coordinator_start(&repo, "p2", None)
        .unwrap();
    let (_, current) = fixture.supervisor.coordinator_token_for_pane("p2").unwrap();
    let refused = gated_run(&fixture, &with_token(Some(&token))).unwrap_err();
    assert_eq!(refused.code(), "tenure_token_refused", "{refused}");
    // The new holder's token is admitted.
    let run = gated_run(&fixture, &with_token(Some(&current))).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    // Acquiring again replaces the token.
    let (_, replaced) = fixture.supervisor.coordinator_token_for_pane("p2").unwrap();
    assert_ne!(replaced, current);
    let refused = gated_run(&fixture, &with_token(Some(&current))).unwrap_err();
    assert_eq!(refused.code(), "tenure_token_refused", "{refused}");
}

/// The interleaving a check-then-act token check cannot survive: the holder
/// rotates its token inside the step the check guards (a latch, not timing).
/// The admission's transaction re-checks it there, so the run starts nothing,
/// and a run admitted with the current token records the tenure it was
/// admitted under.
#[test]
fn a_tenure_change_inside_the_guarded_step_starts_nothing() {
    let fixture = git_fixture("gated-run-tenure-rotate");
    let repo = repository_of(&fixture.repo).unwrap();
    let tenure = fixture
        .supervisor
        .coordinator_start(&repo, "p1", None)
        .unwrap();
    let (_, first) = fixture.supervisor.coordinator_token_for_pane("p1").unwrap();
    // The latch: the holder acquires again (a new token replaces the first)
    // after the precheck passed and before the usage admission.
    let rotating = fixture.supervisor.clone();
    crate::workers::supervisor::gated_run::set_tenure_hook_for_test(Box::new(
        move |_repo: &str| {
            rotating.coordinator_token_for_pane("p1").unwrap();
        },
    ));
    let with_token = |token: &str| WorkerGatedRunParams {
        tenure_token: Some(token.to_owned()),
        ..run_params(&fixture.repo, "finish")
    };
    let refused = gated_run(&fixture, &with_token(&first)).unwrap_err();
    assert_eq!(refused.code(), "tenure_token_refused", "{refused}");
    assert!(
        fixture.supervisor.list().is_empty(),
        "a refusal starts nothing"
    );
    let admitted = |supervisor: &WorkerSupervisor, tenure: &str| -> i64 {
        store_of(supervisor)
            .connection()
            .query_row(
                "SELECT count(*) FROM usage_admissions WHERE tenure_id = ?1",
                [tenure],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        admitted(&fixture.supervisor, &tenure.coordinator_id),
        0,
        "a refused start is admitted nowhere"
    );
    // The token the rotation left is the current one: its run starts, and the
    // admission that authorizes it names the tenure it was admitted under,
    // also when that tenure changes afterwards.
    let (_, current) = fixture.supervisor.coordinator_token_for_pane("p1").unwrap();
    let run = gated_run(&fixture, &with_token(&current)).unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    assert_eq!(
        admitted(&fixture.supervisor, &tenure.coordinator_id),
        1,
        "the admission records the tenure that admitted the start"
    );
}

/// The interleaving left open by admission-time checking alone: the token is
/// rotated, or the tenure ended, after the admission committed and before the
/// worker's process exists (a latch at the start step, not timing). The start
/// step re-checks the admission's tenure and token there, so the run starts
/// nothing and leaves no admission held.
#[test]
fn a_tenure_change_between_the_admission_and_the_spawn_starts_nothing() {
    let fixture = git_fixture("gated-run-start-rotate");
    let repo = repository_of(&fixture.repo).unwrap();
    let tenure = fixture
        .supervisor
        .coordinator_start(&repo, "p1", None)
        .unwrap();
    let (_, first) = fixture.supervisor.coordinator_token_for_pane("p1").unwrap();
    let held = |supervisor: &WorkerSupervisor| -> i64 {
        store_of(supervisor)
            .connection()
            .query_row(
                "SELECT count(*) FROM usage_admissions
                 WHERE tenure_id = ?1 AND released_ms IS NULL",
                [&tenure.coordinator_id],
                |row| row.get(0),
            )
            .unwrap()
    };
    let open_steps = |supervisor: &WorkerSupervisor| -> i64 {
        store_of(supervisor)
            .connection()
            .query_row(
                "SELECT count(*) FROM tenure_steps WHERE retired_ms IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let with_token = |token: &str| WorkerGatedRunParams {
        tenure_token: Some(token.to_owned()),
        ..run_params(&fixture.repo, "finish")
    };
    // The latch: the holder acquires again (a new token replaces the first)
    // after the admission committed and before the start step.
    let rotating = fixture.supervisor.clone();
    crate::workers::supervisor::gated_run::set_start_hook_for_test(Box::new(move |_repo: &str| {
        rotating.coordinator_token_for_pane("p1").unwrap();
    }));
    let refused = gated_run(&fixture, &with_token(&first)).unwrap_err();
    assert_eq!(refused.code(), "tenure_token_refused", "{refused}");
    assert!(
        fixture.supervisor.list().is_empty(),
        "a refusal starts nothing"
    );
    assert_eq!(held(&fixture.supervisor), 0, "nothing stays admitted");
    assert_eq!(open_steps(&fixture.supervisor), 0, "no start step is left");

    // The same with an end: the tenure is gone by the time the start step runs.
    let ending = fixture.supervisor.clone();
    let id = tenure.coordinator_id.clone();
    crate::workers::supervisor::gated_run::set_start_hook_for_test(Box::new(move |_repo: &str| {
        ending
            .coordinator_end(&id, crate::workers::coordinators::ENDED, None)
            .unwrap();
    }));
    let (_, after_rotation) = fixture.supervisor.coordinator_token_for_pane("p1").unwrap();
    let refused = gated_run(&fixture, &with_token(&after_rotation)).unwrap_err();
    assert_eq!(refused.code(), "tenure_token_refused", "{refused}");
    assert!(
        fixture.supervisor.list().is_empty(),
        "a refusal starts nothing"
    );
    assert_eq!(held(&fixture.supervisor), 0, "nothing stays admitted");
    assert_eq!(open_steps(&fixture.supervisor), 0, "no start step is left");
}

/// The token a `worker.run` client sends with its request enters no worker
/// process: not its command line (which the process list shows) and not its
/// environment (herdr strips every `HERDR_*` variable). The descriptor a
/// supervisor hands to the CLI is closed there before the request is sent
/// (`src/cli/worker.rs` tests), so it reaches no process either.
#[test]
fn a_gated_run_keeps_the_tenure_token_out_of_the_workers_argv_and_environment() {
    let fixture = git_fixture("gated-run-tenure-token-worker");
    let repo = repository_of(&fixture.repo).unwrap();
    fixture
        .supervisor
        .coordinator_start(&repo, "p1", None)
        .unwrap();
    let (_, token) = fixture.supervisor.coordinator_token_for_pane("p1").unwrap();
    let run = gated_run(
        &fixture,
        &WorkerGatedRunParams {
            tenure_token: Some(token.clone()),
            ..run_params(&fixture.repo, "finish")
        },
    )
    .unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    let journal = fixture.journal(&run.worker_id);
    let started = journal
        .iter()
        .find(|record| record["event"]["type"] == "started")
        .expect("the worker's started event");
    let argv = started["event"]["args"].to_string();
    assert!(!argv.contains(&token), "{argv}");
    let init = journal
        .iter()
        .find(|record| record["event"]["subtype"] == "init")
        .expect("the worker's init event");
    assert_eq!(init["event"]["herdr_env"], serde_json::json!([]));
}

/// `worker.run` reads the gate of its model's provider too, as `todo.run`
/// does: a `deepseek/` model is admitted while the ChatGPT plan is closed,
/// and a provider herdr has no live usage read for starts nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_gated_run_with_a_pi_model_reads_that_models_provider() {
    let fixture = git_fixture("gated-run-pi-model");
    super::pi_workers::use_pi_stub(&fixture, "pi");
    fixture
        .supervisor
        .set_openai_usage_for_test(fresh_usage(95, 95));
    fixture
        .supervisor
        .set_deepseek_usage_for_test(Ok(runs::deepseek_usage_for_test()));
    let run = gated_run(
        &fixture,
        &WorkerGatedRunParams {
            agent: Some(WorkerAgent::Pi),
            model: Some("deepseek/deepseek-flash".into()),
            ..run_params(&fixture.repo, "finish")
        },
    )
    .unwrap();
    assert_eq!(run.outcome, WorkerGatedRunOutcome::Ended, "{}", run.message);
    assert_eq!(run.usage_gate["provider"], "deepseek");
    assert_eq!(run.usage_gate["decision"], "admit");
    let started = &fixture.herdr_events(&run.worker_id, "started")[0];
    assert_eq!(started["agent"], "pi");
    assert_eq!(started["model"], "deepseek/deepseek-flash");

    let refused = gated_run(
        &fixture,
        &WorkerGatedRunParams {
            agent: Some(WorkerAgent::Pi),
            model: Some("openrouter/some-model".into()),
            ..run_params(&fixture.repo, "finish")
        },
    )
    .unwrap_err();
    assert_eq!(refused.code(), "invalid_request", "{refused}");
    assert!(
        refused.to_string().contains("no live usage read"),
        "{refused}"
    );
}
