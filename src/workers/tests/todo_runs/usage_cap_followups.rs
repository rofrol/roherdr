//! Event-driven cap regressions: loss, handoff and unspawned admissions.
use super::*;

#[test]
fn a_lost_living_worker_holds_its_slot_until_its_process_ends() {
    let fixture = todo_repo("cap-lost-process");
    cap(&fixture, 2);
    let repo = fixture.repo.display().to_string();
    let live = admitted_worker(&fixture, &fixture.repo, &repo, "live");
    let lost = admitted_worker(&fixture, &fixture.repo, &repo, "lost");
    let store = store_of(&fixture.supervisor);
    // Model the projection left by a server that lost its worker. Keep
    // this fixture's runtime so it can end that exact process below.
    store
        .connection()
        .execute("UPDATE workers SET lost = 1 WHERE id = ?1", [&lost])
        .unwrap();
    let gate = || {
        fixture.supervisor.usage_gate(
            &repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder("third")),
            None,
        )
    };
    assert_eq!(held(&fixture.supervisor, &repo).len(), 2);
    assert_eq!(fixture.supervisor.admissions(&repo).unwrap().len(), 2);
    assert_eq!(gate().unwrap_err().code(), "usage_gate");
    stop(&fixture, &lost);
    // No exit event was heard in the crashed-server case: only the OS
    // evidence can free this row, not the exited projection.
    store
        .connection()
        .execute(
            "UPDATE workers SET exited = 0, lost = 1 WHERE id = ?1",
            [&lost],
        )
        .unwrap();
    assert_eq!(held(&fixture.supervisor, &repo), ["worker.run:live"]);
    assert_eq!(gate().unwrap()["decision"], "admit");
    assert_eq!(held(&fixture.supervisor, &repo).len(), 2);
    stop(&fixture, &live);
}

#[test]
fn admissions_list_includes_pending_starts_and_uses_the_caps_predicate() {
    let fixture = todo_repo("cap-admissions-list");
    cap(&fixture, 2);
    let repo = fixture.repo.display().to_string();
    let live = admitted_worker(&fixture, &fixture.repo, &repo, "live");
    let gate = |name| {
        fixture.supervisor.usage_gate(
            &repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(name)),
            None,
        )
    };
    gate("pending").unwrap();
    let rows = fixture.supervisor.admissions(&repo).unwrap();
    assert_eq!(rows.len(), 2);
    let pending = rows.iter().find(|row| !row.bound).unwrap();
    assert_eq!(pending.holder, "worker.run:pending");
    assert_eq!(pending.provider, "claude");
    assert_eq!(pending.server_pid, Some(std::process::id()));
    assert_eq!(
        pending.server_start,
        crate::platform::process_start_token(std::process::id())
    );
    assert!(pending.admitted_ms > 0);
    assert!(pending.worker_id.is_none());
    assert!(pending.run_id.is_none());
    let bound = rows.iter().find(|row| row.bound).unwrap();
    assert_eq!(bound.worker_id.as_deref(), Some(live.as_str()));
    assert_eq!(gate("third").unwrap_err().code(), "usage_gate");
    stop(&fixture, &live);
    gate("third").unwrap();
    let rows = fixture.supervisor.admissions(&repo).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.bound));
    assert!(rows
        .iter()
        .all(|row| row.worker_id.as_deref() != Some(live.as_str())));
}

#[test]
fn a_committed_handoff_retires_pending_starts_but_keeps_a_live_workers_slot() {
    let fixture = Fixture::with_broker();
    cap(&fixture, 2);
    let repo = fixture.repo.display().to_string();
    let live = admitted_worker(&fixture, &fixture.repo, &repo, "live");
    let gate = |supervisor: &WorkerSupervisor, name| {
        supervisor.usage_gate(
            &repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(name)),
            None,
        )
    };
    gate(&fixture.supervisor, "pending").unwrap();
    assert_eq!(
        gate(&fixture.supervisor, "third").unwrap_err().code(),
        "usage_gate"
    );
    fixture.supervisor.prepare_for_handoff(false).unwrap();
    // As in the broker handoff harness: import while the source still
    // owns the journal, then commit and detach it.
    let imported = WorkerSupervisor::open_with(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
        Some(broker::tests::test_launcher()),
    );
    imported.set_usage_gate_config_for_test(crate::config::UsageConfig {
        gate_max_workers: 2,
        ..Default::default()
    });
    assert_eq!(held(&imported, &repo).len(), 2);
    assert_eq!(fixture.supervisor.detach_for_handoff(), [live.as_str()]);
    imported.reconcile_handoff_admissions(Some((
        std::process::id(),
        crate::platform::process_start_token(std::process::id()),
    )));
    assert_eq!(held(&imported, &repo), ["worker.run:live"]);
    gate(&imported, "third").unwrap();
    assert_eq!(gate(&imported, "fourth").unwrap_err().code(), "usage_gate");
    // A dead foreign server's unbound admission can appear after import
    // opens the store but before it assumes ownership. Import reconciles
    // at that event, without waiting for another restart.
    store_of(&imported).connection().execute(
        "UPDATE usage_admissions SET server_pid = 0, server_start = 1 WHERE holder = 'worker.run:third'",
        [],
    ).unwrap();
    imported.reconcile_admissions();
    assert_eq!(held(&imported, &repo), ["worker.run:live"]);
    gate(&imported, "fourth").unwrap();
    // Wait for the positive journal-adoption event before stopping it.
    let started = Instant::now();
    let mut registry = lock(&imported.shared.registry);
    while registry.workers[&worker_number(&live).unwrap()].foreign {
        assert!(started.elapsed() < HANG_GUARD, "journal was not adopted");
        registry = imported
            .shared
            .changed
            .wait_timeout(registry, HANG_GUARD)
            .unwrap()
            .0;
    }
    drop(registry);
    imported.stop(&live).unwrap();
    imported
        .wait(&live, WorkerWaitUntil::Exit, HANG_GUARD, || true)
        .unwrap()
        .unwrap();
}

/// Records `server` as the process that admitted `holder`, as a server whose
/// row outlived it would carry it.
fn admitted_by(supervisor: &WorkerSupervisor, holder: &str, server: (u32, Option<u64>)) {
    store_of(supervisor)
        .connection()
        .execute(
            "UPDATE usage_admissions SET server_pid = ?1, server_start = ?2 WHERE holder = ?3",
            rusqlite::params![server.0, server.1.map(|start| start as i64), holder],
        )
        .unwrap();
}

/// This process as an admission's admitting server.
fn this_server() -> (u32, Option<u64>) {
    let pid = std::process::id();
    (pid, crate::platform::process_start_token(pid))
}

/// A server process that has ended: the identity a row of a server that
/// died between naming its worker and recording it carries.
fn ended_server() -> (u32, Option<u64>) {
    let mut ended = std::process::Command::new("true").spawn().unwrap();
    let pid = ended.id();
    let start = crate::platform::process_start_token(pid);
    ended.wait().unwrap();
    (pid, start)
}

/// A running process standing in for the source of a live handoff: a row
/// records its identity, and it stays alive until the test ends it.
struct LiveSource {
    child: std::process::Child,
    identity: (u32, Option<u64>),
}

impl LiveSource {
    fn identity(&self) -> (u32, Option<u64>) {
        self.identity
    }
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn live_source() -> LiveSource {
    let child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let start = crate::platform::process_start_token(pid);
    LiveSource {
        child,
        identity: (pid, start),
    }
}

/// The release reason recorded for `holder`, once its slot was retired.
fn release_reason(supervisor: &WorkerSupervisor, holder: &str) -> Option<String> {
    store_of(supervisor)
        .connection()
        .query_row(
            "SELECT release FROM usage_admissions WHERE holder = ?1",
            [holder],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap()
}

#[test]
fn reconciliation_frees_a_bound_admission_whose_worker_was_never_written() {
    let fixture = Fixture::new("cap-bound-unwritten");
    cap(&fixture, 1);
    let repo = fixture.repo.display().to_string();
    let gate = |supervisor: &WorkerSupervisor, name| {
        supervisor.usage_gate(
            &repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(name)),
            None,
        )
    };
    gate(&fixture.supervisor, "half-spawned").unwrap();
    // A server named this worker and died before writing its row. The cap
    // reads a named worker with no row as one being spawned and keeps the
    // slot, so it is reconciliation that must free it.
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:half-spawned", "w-never-written")
        .unwrap();
    admitted_by(
        &fixture.supervisor,
        "worker.run:half-spawned",
        ended_server(),
    );
    assert_eq!(
        held(&fixture.supervisor, &repo),
        ["worker.run:half-spawned"]
    );
    assert_eq!(
        gate(&fixture.supervisor, "next").unwrap_err().code(),
        "usage_gate"
    );

    // The next start is the producing event: the admitting server is gone,
    // so nothing can still be spawning this worker.
    let restarted = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    assert!(held(&restarted, &repo).is_empty());
    let reason = release_reason(&restarted, "worker.run:half-spawned").unwrap();
    assert!(
        reason.contains("before the worker was recorded"),
        "{reason}"
    );
    // And the cap admits again.
    restarted.set_usage_gate_config_for_test(crate::config::UsageConfig {
        gate_max_workers: 1,
        ..Default::default()
    });
    gate(&restarted, "next").unwrap();
    assert_eq!(held(&restarted, &repo), ["worker.run:next"]);
}

#[test]
fn a_start_its_server_may_still_spawn_keeps_its_slot_without_a_worker_row() {
    let fixture = Fixture::new("cap-bound-in-flight");
    cap(&fixture, 1);
    let repo = fixture.repo.display().to_string();
    fixture
        .supervisor
        .usage_gate(
            &repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder("spawning")),
            None,
        )
        .unwrap();
    // Between naming the worker and writing its row there is no journal
    // either: the admitting server's own identity is the evidence that this
    // may be in flight, and retiring it would let a second worker take its
    // slot while it spawns.
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:spawning", "w-not-yet")
        .unwrap();
    fixture.supervisor.reconcile_admissions();
    assert_eq!(held(&fixture.supervisor, &repo), ["worker.run:spawning"]);
    // Once that server is gone, the same row is freed.
    admitted_by(&fixture.supervisor, "worker.run:spawning", ended_server());
    fixture.supervisor.reconcile_admissions();
    assert!(held(&fixture.supervisor, &repo).is_empty());
}

#[test]
fn a_handoff_retires_the_sources_unbound_starts_and_keeps_its_spawn_in_flight() {
    let fixture = Fixture::new("cap-handoff-spawn");
    cap(&fixture, 3);
    let repo = fixture.repo.display().to_string();
    let gate = |name| {
        fixture
            .supervisor
            .usage_gate(
                &repo,
                WorkerAgent::Claude,
                None,
                false,
                Some(&gate_holder(name)),
                None,
            )
            .unwrap();
    };
    gate("unbound");
    gate("spawning");
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:spawning", "w-in-flight")
        .unwrap();
    // The source of a committed handoff: its unbound starts can no longer
    // become this session's workers, while a start it already named may be
    // spawning right now, so its slot waits for that server's exit.
    fixture
        .supervisor
        .reconcile_handoff_admissions(Some(this_server()));
    assert_eq!(held(&fixture.supervisor, &repo), ["worker.run:spawning"]);
    let reason = release_reason(&fixture.supervisor, "worker.run:unbound").unwrap();
    assert!(reason.contains("handoff"), "{reason}");
}

#[cfg(unix)]
#[test]
fn the_sources_late_admissions_are_retired_when_its_exit_event_arrives() {
    let fixture = Fixture::new("cap-handoff-late");
    cap(&fixture, 3);
    let repo = fixture.repo.display().to_string();
    let gate = |name| {
        fixture
            .supervisor
            .usage_gate(
                &repo,
                WorkerAgent::Claude,
                None,
                false,
                Some(&gate_holder(name)),
                None,
            )
            .unwrap();
    };
    // A process this test can end, standing in for the server that hands
    // off: only the identity it records matters.
    let mut source_child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let source_pid = source_child.id();
    let source_start = crate::platform::process_start_token(source_pid).unwrap();
    let source = (source_pid, Some(source_start));
    gate("before-import");
    admitted_by(&fixture.supervisor, "worker.run:before-import", source);
    // The new server's own start, admitted before the import.
    gate("new-server");
    crate::workers::set_test_supervisor(fixture.supervisor.clone());
    crate::workers::reconcile_admissions_after_handoff(Some(source));
    assert_eq!(held(&fixture.supervisor, &repo), ["worker.run:new-server"]);
    // The source finishes one more call after the import: its slot is
    // admitted by the old server's identity, which the import's own
    // reconciliation could not have seen.
    gate("after-import");
    admitted_by(&fixture.supervisor, "worker.run:after-import", source);
    assert_eq!(
        held(&fixture.supervisor, &repo),
        ["worker.run:after-import", "worker.run:new-server"]
    );
    // Its exit event is what retires it, by the identity it admitted it
    // under; the new server's own start keeps its slot.
    drop(source_child.stdin.take());
    source_child.wait().unwrap();
    let started = Instant::now();
    while held(&fixture.supervisor, &repo).contains(&"worker.run:after-import".to_owned()) {
        assert!(
            started.elapsed() < HANG_GUARD,
            "the source's late admission outlived its exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(held(&fixture.supervisor, &repo), ["worker.run:new-server"]);
    let reason = release_reason(&fixture.supervisor, "worker.run:after-import").unwrap();
    assert!(reason.contains("handoff"), "{reason}");
}

#[test]
fn two_consecutive_handoffs_retire_each_source_and_keep_a_live_workers_slot() {
    let fixture = Fixture::new("cap-two-handoffs");
    cap(&fixture, 3);
    let repo = fixture.repo.display().to_string();
    let gate = |supervisor: &WorkerSupervisor, name| {
        supervisor
            .usage_gate(
                &repo,
                WorkerAgent::Claude,
                None,
                false,
                Some(&gate_holder(name)),
                None,
            )
            .unwrap();
    };
    let live = admitted_worker(&fixture, &fixture.repo, &repo, "live");
    // The first source is still running when it hands off (a live child
    // stands in for its process): it admits a start it never bounds, then
    // hands off.
    let first_source = live_source();
    let first = first_source.identity();
    gate(&fixture.supervisor, "first-pending");
    admitted_by(&fixture.supervisor, "worker.run:first-pending", first);
    let second = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    second.reconcile_handoff_admissions(Some(first));
    assert_eq!(held(&second, &repo), ["worker.run:live"]);
    drop(first_source);
    // The second server admits one of its own and hands off as well.
    let second_source = live_source();
    let next = second_source.identity();
    gate(&second, "second-pending");
    admitted_by(&second, "worker.run:second-pending", next);
    let third = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    third.reconcile_handoff_admissions(Some(next));
    assert_eq!(held(&third, &repo), ["worker.run:live"]);
    drop(second_source);
    // Each row kept the reason of the handoff that retired it: the first
    // one was not retired again by the second.
    for holder in ["worker.run:first-pending", "worker.run:second-pending"] {
        let reason = release_reason(&third, holder).unwrap();
        assert!(reason.contains("handoff"), "{holder}: {reason}");
    }
    // The live worker's slot outlived both handoffs.
    assert_eq!(third.admissions(&repo).unwrap().len(), 1);
    stop(&fixture, &live);
}

#[test]
fn the_admissions_list_resolves_a_worktrees_repository_like_the_gate() {
    let fixture = todo_repo("usage-cap-admissions-worktree");
    cap(&fixture, 2);
    let repo = fixture.repo.display().to_string();
    let worktree = fixture.root.join("admissions-worktree");
    git_in(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &worktree.display().to_string(),
            "HEAD",
        ],
    );
    // A start admitted from the worktree holds a slot of the repository.
    fixture
        .supervisor
        .usage_gate(
            &worktree.display().to_string(),
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder("from-worktree")),
            None,
        )
        .unwrap();
    let listed = fixture.supervisor.admissions(&repo).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].holder, "worker.run:from-worktree");
    // Asked from the worktree and from a subdirectory of the checkout, the
    // list is the same: one repository, never mixed or missed.
    std::fs::create_dir_all(fixture.repo.join("sub/dir")).unwrap();
    for cwd in [worktree.clone(), fixture.repo.join("sub/dir")] {
        let rows = fixture
            .supervisor
            .admissions(&cwd.display().to_string())
            .unwrap();
        assert_eq!(rows.len(), 1, "{cwd:?}");
        assert_eq!(rows[0].repo, listed[0].repo, "{cwd:?}");
    }
}
