//! The usage gate's cap on the workers of one provider, across all
//! repositories, and its high latch kept per provider account.

use super::*;
use crate::workers::runs::usage_gate::Holder;

/// `fixture`'s gate with at most `max_workers` workers per provider.
fn cap(fixture: &Fixture, max_workers: u32) {
    fixture
        .supervisor
        .set_usage_gate_config_for_test(crate::config::UsageConfig {
            gate_max_workers: max_workers,
            ..crate::config::UsageConfig::default()
        });
}

/// The holders of Claude's slots, sorted.
fn held(supervisor: &WorkerSupervisor) -> Vec<String> {
    let mut held = store_of(supervisor)
        .held_admissions("claude")
        .unwrap()
        .into_iter()
        .map(|admission| admission.holder)
        .collect::<Vec<_>>();
    held.sort();
    held
}

/// A worker the gate admitted as `holder`, started in `dir`; it stays
/// running after its turn until stopped.
fn admitted_worker(fixture: &Fixture, dir: &Path, holder: &str) -> String {
    let holder = gate_holder(holder);
    fixture
        .supervisor
        .usage_gate(
            &dir.display().to_string(),
            WorkerAgent::Claude,
            false,
            Some(&holder),
        )
        .unwrap();
    let worker = fixture
        .supervisor
        .start_admitted(
            &start_params(dir, "finish", Some("stub-model")),
            Some(&holder.key()),
        )
        .unwrap()
        .worker_id;
    fixture.wait(&worker, WorkerWaitUntil::TurnEnd);
    worker
}

fn stop(fixture: &Fixture, worker_id: &str) {
    fixture.supervisor.stop(worker_id).unwrap();
    fixture.wait(worker_id, WorkerWaitUntil::Exit);
}

#[test]
fn concurrent_starts_across_two_repositories_admit_exactly_the_cap() {
    for max_workers in [1, 2, 3] {
        let fixture = Fixture::new(&format!("usage-cap-concurrent-{max_workers}"));
        cap(&fixture, max_workers);
        let repos = ["/repo-a".to_owned(), "/repo-b".to_owned()];
        let starts = max_workers as usize + 1;
        // Every start reads usage below the threshold and goes through the
        // gate at once: the barrier releases them together.
        let barrier = std::sync::Barrier::new(starts);
        let results = std::thread::scope(|scope| {
            let handles = (0..starts)
                .map(|start| {
                    let (barrier, fixture, repos) = (&barrier, &fixture, &repos);
                    scope.spawn(move || {
                        barrier.wait();
                        fixture.supervisor.usage_gate(
                            &repos[start % 2],
                            WorkerAgent::Claude,
                            false,
                            Some(&gate_holder(&format!("start-{start}"))),
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let admitted = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(admitted, max_workers as usize, "{results:#?}");
        let refused = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .unwrap();
        assert_eq!(refused.code(), "usage_gate", "{refused}");
        let message = refused.to_string();
        assert!(
            message.contains(&format!("at most {max_workers} ([usage] gate_max_workers)")),
            "{message}"
        );
        // The refusal names every admission that holds a slot.
        assert_eq!(
            message.matches("worker.run (not started yet,").count(),
            max_workers as usize,
            "{message}"
        );
        assert_eq!(held(&fixture.supervisor).len(), max_workers as usize);
        // The admissions record the cap; Pi's provider has slots of its own.
        let admission = results
            .iter()
            .find_map(|result| result.as_ref().ok())
            .unwrap();
        assert_eq!(admission["cap"]["max_workers"], max_workers);
        fixture
            .supervisor
            .usage_gate("/repo-a", WorkerAgent::Pi, false, Some(&gate_holder("pi")))
            .unwrap();
    }
}

#[test]
fn the_same_holder_admitted_again_takes_one_slot_and_ignore_usage_overrides_the_cap() {
    let fixture = Fixture::new("usage-cap-idempotent");
    cap(&fixture, 1);
    let gate = |holder: &str, ignore| {
        fixture.supervisor.usage_gate(
            "/repo-a",
            WorkerAgent::Claude,
            ignore,
            Some(&gate_holder(holder)),
        )
    };
    gate("retried", false).unwrap();
    let again = gate("retried", false).unwrap();
    // The slots held by others: none.
    assert_eq!(again["cap"]["held"], json!([]));
    assert_eq!(held(&fixture.supervisor), ["worker.run:retried"]);
    let refused = gate("other", false).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("worker.run (not started yet, /repo-a)"),
        "{refused}"
    );
    // --ignore-usage admits past the cap, says so, and counts.
    let ignored = gate("other", true).unwrap();
    assert_eq!(ignored["cap"]["ignored"], true, "{ignored}");
    assert_eq!(held(&fixture.supervisor).len(), 2);
}

#[test]
fn an_exit_frees_a_slot_and_wakes_the_start_waiting_on_the_cap() {
    let fixture = todo_repo("usage-cap-exit-wakes");
    cap(&fixture, 1);
    let other = fixture.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let blocker = admitted_worker(&fixture, &other, "blocker");
    let pending = fixture
        .supervisor
        .todo_run_wait(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
        .unwrap();
    assert_eq!(pending.status, TodoRunStatus::Waiting);
    let why = pending.error.clone().unwrap_or_default();
    assert!(why.contains("gate_max_workers"), "{why}");
    assert!(why.contains(&format!("worker {blocker}")), "{why}");
    assert_eq!(fixture.supervisor.list().len(), 1);
    // The blocker's exit is the only event that can start the run: no
    // measurement comes, and the gate is open.
    stop(&fixture, &blocker);
    let (review, _) = wait(&fixture, &pending.run_id, None);
    assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
    let run = fixture.supervisor.load_run(&pending.run_id).unwrap();
    assert!(!run.finish.pending_usage);
    let holder = Holder::Run {
        run_id: pending.run_id.clone(),
        attempt: 1,
    };
    assert_eq!(held(&fixture.supervisor), [holder.key()]);
    // A retry's attempt is not refused for the slot its own run's worker
    // holds: that worker is the one it replaces.
    let retried = resume(
        &fixture,
        &pending.run_id,
        review.event_id,
        TodoAction::Retry,
        Some(&format!("commit b.txt {SUBJECT}")),
    )
    .unwrap();
    assert_eq!(retried.step, TodoStep::Restart);
    let (review, _) = wait(&fixture, &pending.run_id, Some(review.event_id));
    assert_eq!(review.kind, TodoEventKind::Review, "{review:#?}");
    abort(&fixture, &pending.run_id, review.event_id);
}

#[test]
fn a_duplicate_exit_frees_the_slot_once() {
    let fixture = Fixture::new("usage-cap-duplicate-exit");
    cap(&fixture, 2);
    let first = admitted_worker(&fixture, &fixture.repo, "first");
    let second = admitted_worker(&fixture, &fixture.repo, "second");
    stop(&fixture, &first);
    // The exit recorded again (a second report of the same end).
    let exited = json!({"type": "exited", "code": 0});
    fixture
        .supervisor
        .record(worker_number(&first).unwrap(), Direction::Herdr, &exited);
    assert_eq!(held(&fixture.supervisor), ["worker.run:second"]);
    let gate = |holder: &str| {
        fixture.supervisor.usage_gate(
            &fixture.repo.display().to_string(),
            WorkerAgent::Claude,
            false,
            Some(&gate_holder(holder)),
        )
    };
    gate("third").unwrap();
    let refused = gate("fourth").unwrap_err();
    assert!(
        refused.to_string().contains(&format!("worker {second}")),
        "{refused}"
    );
    stop(&fixture, &second);
}

#[test]
fn a_restart_keeps_the_slots_of_live_workers_and_ends_those_no_server_makes() {
    let fixture = Fixture::new("usage-cap-restart");
    cap(&fixture, 10);
    let repo = fixture.repo.display().to_string();
    let gate = |holder: &Holder| {
        fixture
            .supervisor
            .usage_gate(&repo, WorkerAgent::Claude, false, Some(holder))
            .unwrap();
    };
    // Running, and still running after the restart (its server still holds
    // it, as the old server of a live handoff does).
    let live = admitted_worker(&fixture, &fixture.repo, "live");
    // Exited before the restart.
    let exited = admitted_worker(&fixture, &fixture.repo, "exited");
    stop(&fixture, &exited);
    // Named before its spawn, by a server that ended before recording it.
    gate(&gate_holder("unknown"));
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:unknown", "w999")
        .unwrap();
    // A worker.run admitted by a server that ended before the start.
    gate(&gate_holder("crashed"));
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let gone_pid = gone.id();
    gone.wait().unwrap();
    store_of(&fixture.supervisor)
        .connection()
        .execute(
            "UPDATE usage_admissions SET server_pid = ?1, server_start = 1
             WHERE holder = 'worker.run:crashed'",
            [gone_pid],
        )
        .unwrap();
    // A worker.run admitted by a server that still runs (this process).
    gate(&gate_holder("starting"));
    // A run's attempt admitted before the run was recorded: no run holds it.
    gate(&Holder::Run {
        run_id: "r-never-recorded".into(),
        attempt: 1,
    });
    assert_eq!(
        held(&fixture.supervisor),
        [
            "worker.run:crashed",
            "worker.run:live",
            "worker.run:starting",
            "worker.run:unknown"
        ]
    );

    let restarted = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    assert_eq!(held(&restarted), ["worker.run:live", "worker.run:starting"]);
    let released = store_of(&restarted)
        .connection()
        .query_row(
            "SELECT release FROM usage_admissions WHERE holder = 'worker.run:unknown'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert!(
        released.contains("before the worker was recorded"),
        "{released}"
    );
    drop(restarted);
    stop(&fixture, &live);
}

#[test]
fn the_high_latch_closed_from_one_repository_refuses_another() {
    let fixture = Fixture::new("usage-cap-latch-per-account");
    let gate = |repo: &str| {
        fixture
            .supervisor
            .usage_gate(repo, WorkerAgent::Claude, false, Some(&gate_holder(repo)))
    };
    fixture.supervisor.set_usage_for_test(fresh_usage(40, 95));
    assert_eq!(gate("/repo-a").unwrap_err().code(), "usage_gate");
    // 85% would open a gate of its own; the account's high latch reopens
    // only below 80%, whichever repository asks.
    fixture.supervisor.set_usage_for_test(fresh_usage(85, 10));
    let refused = gate("/repo-b").unwrap_err().to_string();
    assert!(refused.contains("below 80%"), "{refused}");
    fixture.supervisor.set_usage_for_test(fresh_usage(79, 10));
    assert_eq!(gate("/repo-b").unwrap()["reopened"], true);
    assert_eq!(
        store_of(&fixture.supervisor).usage_gate_keys().unwrap(),
        ["provider:claude"]
    );
}
