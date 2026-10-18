//! The usage gate's cap on the workers of one provider in one repository
//! keyed by git identity, and its high latch kept per provider account.

use super::*;
use crate::workers::runs::usage_gate::Holder;

#[path = "usage_cap_followups.rs"]
mod followups;

/// `fixture`'s gate with at most `max_workers` workers per provider.
fn cap(fixture: &Fixture, max_workers: u32) {
    fixture
        .supervisor
        .set_usage_gate_config_for_test(crate::config::UsageConfig {
            gate_max_workers: max_workers,
            ..crate::config::UsageConfig::default()
        });
}

/// The holders of Claude's slots in `repo`, sorted.
fn held(supervisor: &WorkerSupervisor, repo: &str) -> Vec<String> {
    let identity = crate::workers::runs::repo_identity(Path::new(repo)).unwrap();
    let mut held = store_of(supervisor)
        .held_admissions("claude", &identity)
        .unwrap()
        .into_iter()
        .map(|admission| admission.holder)
        .collect::<Vec<_>>();
    held.sort();
    held
}

/// A worker the gate admitted as `holder` in `repo`, started in `dir`; it
/// stays running after its turn until stopped.
fn admitted_worker(fixture: &Fixture, dir: &Path, repo: &str, holder: &str) -> String {
    let holder = gate_holder(holder);
    fixture
        .supervisor
        .usage_gate(repo, WorkerAgent::Claude, None, false, Some(&holder))
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
fn concurrent_starts_in_one_repository_admit_exactly_the_cap() {
    for max_workers in [1, 2, 3] {
        let fixture = Fixture::new(&format!("usage-cap-concurrent-{max_workers}"));
        cap(&fixture, max_workers);
        let repos = ["/repo-a".to_owned(), "/repo-a".to_owned()];
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
                            None,
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
            message.contains(&format!(
                "at most {max_workers} per repository and provider ([usage] gate_max_workers)"
            )),
            "{message}"
        );
        assert!(message.contains("/repo-a"), "{message}");
        // The refusal names every admission that holds a slot.
        assert_eq!(
            message.matches("worker.run (not started yet,").count(),
            max_workers as usize,
            "{message}"
        );
        assert_eq!(
            held(&fixture.supervisor, "/repo-a").len(),
            max_workers as usize
        );
        // The admissions record the cap; Pi's provider has slots of its own.
        let admission = results
            .iter()
            .find_map(|result| result.as_ref().ok())
            .unwrap();
        assert_eq!(admission["cap"]["max_workers"], max_workers);
        fixture
            .supervisor
            .usage_gate(
                "/repo-a",
                WorkerAgent::Pi,
                None,
                false,
                Some(&gate_holder("pi")),
            )
            .unwrap();
    }
}

/// The cap's key is the repository's git identity, not a path: a linked
/// worktree and a subdirectory of one clone share the main checkout's
/// bucket, so one coordinator's worktrees cannot take extra slots.
#[test]
fn worktrees_and_subdirectories_of_one_clone_share_a_bucket() {
    let fixture = todo_repo("usage-cap-git-identity");
    cap(&fixture, 2);
    let identity = crate::workers::slot::common_dir(&fixture.repo).unwrap();
    let worktrees = [
        fixture.root.join("linked-one"),
        fixture.root.join("linked-two"),
    ];
    for worktree in &worktrees {
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
    }
    let subdirectory = fixture.repo.join("sub");
    std::fs::create_dir_all(&subdirectory).unwrap();
    // Pass the actual cwd, as both admission entry points do. Resolving it
    // in the test would mask a gate that still keys on the recorded path.
    let gate = |dir: &Path, holder: &str| {
        fixture.supervisor.usage_gate(
            &dir.display().to_string(),
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(holder)),
        )
    };
    gate(&worktrees[0], "one").unwrap();
    gate(&worktrees[1], "two").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let alias = fixture.root.join("repo-alias");
        symlink(&fixture.repo, &alias).unwrap();
        assert_eq!(gate(&alias, "alias").unwrap_err().code(), "usage_gate");
    }
    let refused = gate(&subdirectory, "third").unwrap_err();
    assert_eq!(refused.code(), "usage_gate", "{refused}");
    let message = refused.to_string();
    assert!(
        message.contains(&identity.display().to_string()),
        "{message}"
    );
    assert!(
        message.contains(&subdirectory.display().to_string()),
        "{message}"
    );
    assert_eq!(
        held(&fixture.supervisor, &fixture.repo.display().to_string()),
        ["worker.run:one", "worker.run:two"]
    );
}

/// The cap is the coordinator's repository: another repository's workers
/// (the user's bug, 2026-10-18) do not block this one's approved item.
#[test]
fn another_repository_has_slots_of_its_own() {
    let fixture = Fixture::new("usage-cap-per-repository");
    cap(&fixture, 1);
    let repo_a = fixture.root.join("repo-a");
    let repo_b = fixture.root.join("repo-b");
    for repo in [&repo_a, &repo_b] {
        std::fs::create_dir_all(repo).unwrap();
        git_in(repo, &["init", "-q"]);
    }
    let repo_a = repo_a.display().to_string();
    let repo_b = repo_b.display().to_string();
    let gate = |repo: &str, holder: &str| {
        fixture.supervisor.usage_gate(
            repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(holder)),
        )
    };
    gate(&repo_a, "a1").unwrap();
    // Repo A is full, and the refusal names only its own starts.
    let refused = gate(&repo_a, "a2").unwrap_err();
    assert_eq!(refused.code(), "usage_gate", "{refused}");
    assert!(refused.to_string().contains(&repo_a), "{refused}");
    assert!(!refused.to_string().contains(&repo_b), "{refused}");
    gate(&repo_b, "b1").unwrap();
    assert_eq!(held(&fixture.supervisor, &repo_a), ["worker.run:a1"]);
    assert_eq!(held(&fixture.supervisor, &repo_b), ["worker.run:b1"]);
    gate(&repo_b, "b2").unwrap_err();
}

#[test]
fn non_git_directories_use_real_paths_then_absolute_paths() {
    let fixture = Fixture::new("usage-cap-non-git");
    cap(&fixture, 1);
    let directory = fixture.root.join("plain");
    std::fs::create_dir_all(&directory).unwrap();
    assert!(crate::workers::slot::common_dir(&directory).is_err());
    let gate = |dir: &Path, holder: &str| {
        fixture.supervisor.usage_gate(
            &dir.display().to_string(),
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(holder)),
        )
    };
    gate(&directory, "real").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let alias = fixture.root.join("alias");
        symlink(&directory, &alias).unwrap();
        assert_eq!(gate(&alias, "alias").unwrap_err().code(), "usage_gate");
    }
    let missing = fixture.root.join("missing");
    gate(&missing, "missing").unwrap();
    // Check the stored keys directly, without the test helper resolving them.
    let store = store_of(&fixture.supervisor);
    assert_eq!(
        store
            .held_admissions(
                "claude",
                &format!("path:{}", directory.canonicalize().unwrap().display())
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .held_admissions(
                "claude",
                &format!("path:{}", std::path::absolute(&missing).unwrap().display())
            )
            .unwrap()
            .len(),
        1
    );

    // A bare repository at exactly the fallback path must get a distinct key.
    let fallback_key = format!("path:{}", directory.canonicalize().unwrap().display());
    git_in(&directory, &["init", "--bare", "-q"]);
    let git_key = crate::workers::runs::repo_identity(&directory).unwrap();
    assert_eq!(
        git_key,
        directory.canonicalize().unwrap().display().to_string()
    );
    assert_ne!(fallback_key, git_key);
}

#[test]
fn bare_and_separate_git_directories_store_the_common_directory_identity() {
    let fixture = Fixture::new("usage-cap-git-layouts");
    let bare = fixture.root.join("bare.git");
    let checkout = fixture.root.join("checkout");
    let separate = fixture.root.join("separate.git");
    git_in(
        &fixture.root,
        &["init", "--bare", "-q", bare.to_str().unwrap()],
    );
    git_in(
        &fixture.root,
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", separate.display()),
            checkout.to_str().unwrap(),
        ],
    );
    for (dir, expected, holder) in [(&bare, &bare, "bare"), (&checkout, &separate, "separate")] {
        fixture
            .supervisor
            .usage_gate(
                dir.to_str().unwrap(),
                WorkerAgent::Claude,
                None,
                false,
                Some(&gate_holder(holder)),
            )
            .unwrap();
        // Inspect the persisted identity independently of repo_identity.
        let admissions = store_of(&fixture.supervisor)
            .held_admissions("claude", expected.canonicalize().unwrap().to_str().unwrap())
            .unwrap();
        assert_eq!(admissions.len(), 1);
        assert_eq!(admissions[0].holder, format!("worker.run:{holder}"));
    }
}

#[test]
fn invalid_worktree_git_file_falls_back_to_real_path() {
    let fixture = todo_repo("usage-cap-invalid-git");
    let worktree = fixture.root.join("invalid-worktree");
    git_in(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let git_file = worktree.join(".git");
    std::fs::write(
        &git_file,
        format!(
            "gitdir: {}\n",
            fixture.root.join("nonexistent-git-dir").display()
        ),
    )
    .unwrap();
    assert!(crate::workers::slot::common_dir(&worktree).is_err());
    fixture
        .supervisor
        .usage_gate(
            worktree.to_str().unwrap(),
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder("invalid")),
        )
        .unwrap();
    let admissions = store_of(&fixture.supervisor)
        .held_admissions(
            "claude",
            &format!("path:{}", worktree.canonicalize().unwrap().display()),
        )
        .unwrap();
    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0].holder, "worker.run:invalid");
}

#[test]
fn todo_and_worker_run_race_for_the_same_git_identity() {
    let fixture = todo_repo("usage-cap-mixed-race");
    cap(&fixture, 1);
    // Persist a real waiting run so its admission holds a slot even before
    // it spawns. No usage event is emitted: the test drives the gate itself.
    fixture.supervisor.set_usage_for_test(fresh_usage(95, 95));
    let pending = fixture
        .supervisor
        .todo_run_wait(params(&fixture, &format!("commit a.txt {SUBJECT}"), "ok"))
        .unwrap();
    assert_eq!(pending.status, TodoRunStatus::Waiting);
    fixture.supervisor.set_usage_for_test(fresh_usage(10, 10));
    let worktree = fixture.root.join("race-worktree");
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
    let barrier = std::sync::Barrier::new(2);
    let holders = [
        Holder::Run {
            run_id: pending.run_id,
            attempt: 1,
        },
        gate_holder("racing-worker"),
    ];
    let results = std::thread::scope(|scope| {
        let handles = holders
            .iter()
            .zip([&fixture.repo, &worktree])
            .map(|(holder, dir)| {
                let (barrier, supervisor) = (&barrier, &fixture.supervisor);
                scope.spawn(move || {
                    barrier.wait();
                    supervisor.usage_gate(
                        &dir.display().to_string(),
                        WorkerAgent::Claude,
                        None,
                        false,
                        Some(holder),
                    )
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:#?}"
    );
    assert_eq!(
        results
            .iter()
            .find_map(|result| result.as_ref().err())
            .unwrap()
            .code(),
        "usage_gate"
    );
    assert_eq!(
        held(&fixture.supervisor, &fixture.repo.display().to_string()).len(),
        1
    );
}

#[test]
fn the_same_holder_admitted_again_takes_one_slot_and_ignore_usage_overrides_the_cap() {
    let fixture = Fixture::new("usage-cap-idempotent");
    cap(&fixture, 1);
    let gate = |holder: &str, ignore| {
        fixture.supervisor.usage_gate(
            "/repo-a",
            WorkerAgent::Claude,
            None,
            ignore,
            Some(&gate_holder(holder)),
        )
    };
    gate("retried", false).unwrap();
    let again = gate("retried", false).unwrap();
    // The slots held by others: none.
    assert_eq!(again["cap"]["held"], json!([]));
    assert_eq!(held(&fixture.supervisor, "/repo-a"), ["worker.run:retried"]);
    let refused = gate("other", false).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("worker.run (not started yet, path:/repo-a)"),
        "{refused}"
    );
    // --ignore-usage admits past the cap, says so, and counts.
    let ignored = gate("other", true).unwrap();
    assert_eq!(ignored["cap"]["ignored"], true, "{ignored}");
    assert_eq!(held(&fixture.supervisor, "/repo-a").len(), 2);
}

#[test]
fn an_exit_frees_a_slot_and_wakes_the_start_waiting_on_the_cap() {
    let fixture = todo_repo("usage-cap-exit-wakes");
    cap(&fixture, 1);
    // The cap is the repository's: the blocker holds a slot of the same one.
    let repo = repository_of(&fixture.repo).unwrap();
    let blocker = admitted_worker(&fixture, &fixture.repo, &repo, "blocker");
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
    assert_eq!(held(&fixture.supervisor, &repo), [holder.key()]);
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
    let repo = fixture.repo.display().to_string();
    let first = admitted_worker(&fixture, &fixture.repo, &repo, "first");
    let second = admitted_worker(&fixture, &fixture.repo, &repo, "second");
    stop(&fixture, &first);
    // The exit recorded again (a second report of the same end).
    let exited = json!({"type": "exited", "code": 0});
    fixture
        .supervisor
        .record(worker_number(&first).unwrap(), Direction::Herdr, &exited);
    assert_eq!(held(&fixture.supervisor, &repo), ["worker.run:second"]);
    let gate = |holder: &str| {
        fixture.supervisor.usage_gate(
            &repo,
            WorkerAgent::Claude,
            None,
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
fn the_cap_counts_the_models_provider_not_its_agent() {
    let fixture = Fixture::new("usage-cap-provider-per-model");
    cap(&fixture, 1);
    let repo = "/repo-a";
    fixture
        .supervisor
        .set_openai_usage_for_test(fresh_usage(10, 10));
    fixture
        .supervisor
        .set_deepseek_usage_for_test(Ok(runs::deepseek_usage_for_test()));
    // One pi worker on the ChatGPT plan holds OpenAI's only slot.
    fixture
        .supervisor
        .usage_gate(
            repo,
            WorkerAgent::Pi,
            None,
            false,
            Some(&gate_holder("openai")),
        )
        .unwrap();
    // The same agent on a DeepSeek model has DeepSeek's slot.
    fixture
        .supervisor
        .usage_gate(
            repo,
            WorkerAgent::Pi,
            Some("deepseek/deepseek-flash"),
            false,
            Some(&gate_holder("deepseek")),
        )
        .unwrap();
    // A second ChatGPT-plan worker is still past its cap.
    let refused = fixture
        .supervisor
        .usage_gate(
            repo,
            WorkerAgent::Pi,
            None,
            false,
            Some(&gate_holder("third")),
        )
        .unwrap_err();
    assert_eq!(refused.code(), "usage_gate", "{refused}");
    assert!(
        refused.to_string().contains("OpenAI (ChatGPT plan)"),
        "{refused}"
    );
    // A closed ChatGPT (and Claude) latch leaves DeepSeek's gate open: the
    // rows are one per provider, not one per agent.
    fixture.supervisor.set_usage_for_test(fresh_usage(95, 95));
    assert_eq!(
        fixture
            .supervisor
            .usage_gate(
                repo,
                WorkerAgent::Claude,
                None,
                false,
                Some(&gate_holder("claude"))
            )
            .unwrap_err()
            .code(),
        "usage_gate"
    );
    assert!(store_of(&fixture.supervisor)
        .usage_gate_closed("provider:claude")
        .unwrap());
    assert!(!store_of(&fixture.supervisor)
        .usage_gate_closed("provider:deepseek")
        .unwrap());
    assert!(!store_of(&fixture.supervisor)
        .usage_gate_closed("provider:openai")
        .unwrap());
}

#[test]
fn a_restart_keeps_the_slots_of_live_workers_and_ends_those_no_server_makes() {
    let fixture = Fixture::new("usage-cap-restart");
    cap(&fixture, 10);
    let repo = fixture.repo.display().to_string();
    let gate = |holder: &Holder| {
        fixture
            .supervisor
            .usage_gate(&repo, WorkerAgent::Claude, None, false, Some(holder))
            .unwrap();
    };
    // Running, and still running after the restart (its server still holds
    // it, as the old server of a live handoff does).
    let live = admitted_worker(&fixture, &fixture.repo, &repo, "live");
    // Exited before the restart.
    let exited = admitted_worker(&fixture, &fixture.repo, &repo, "exited");
    stop(&fixture, &exited);
    // A server that ended before recording a worker it had named, and one
    // that ended before the start it admitted.
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let gone_pid = gone.id();
    gone.wait().unwrap();
    gate(&gate_holder("unknown"));
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:unknown", "w999")
        .unwrap();
    gate(&gate_holder("crashed"));
    store_of(&fixture.supervisor)
        .connection()
        .execute(
            "UPDATE usage_admissions SET server_pid = ?1, server_start = 1
             WHERE holder IN ('worker.run:unknown', 'worker.run:crashed')",
            [gone_pid],
        )
        .unwrap();
    // Named before its spawn, from a server that recorded no identity at all
    // (written before the admissions carried one).
    gate(&gate_holder("legacy"));
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:legacy", "w998")
        .unwrap();
    store_of(&fixture.supervisor)
        .connection()
        .execute(
            "UPDATE usage_admissions SET server_pid = NULL, server_start = NULL
             WHERE holder = 'worker.run:legacy'",
            [],
        )
        .unwrap();
    // A worker.run admitted by a server that still runs (this process).
    gate(&gate_holder("starting"));
    // A start this process admitted and named, whose worker row it has not
    // written yet: it may be spawning right now.
    gate(&gate_holder("spawning"));
    store_of(&fixture.supervisor)
        .bind_admission("worker.run:spawning", "w997")
        .unwrap();
    // A run's attempt admitted before the run was recorded: no run holds it.
    gate(&Holder::Run {
        run_id: "r-never-recorded".into(),
        attempt: 1,
    });
    assert_eq!(
        held(&fixture.supervisor, &repo),
        [
            "worker.run:crashed",
            "worker.run:legacy",
            "worker.run:live",
            "worker.run:spawning",
            "worker.run:starting",
            "worker.run:unknown"
        ]
    );

    let restarted = WorkerSupervisor::open(
        fixture.root.join("workers"),
        fixture.root.join("claude-stub"),
    );
    assert_eq!(
        held(&restarted, &repo),
        [
            "worker.run:live",
            "worker.run:spawning",
            "worker.run:starting"
        ]
    );
    for holder in ["worker.run:unknown", "worker.run:legacy"] {
        let released = store_of(&restarted)
            .connection()
            .query_row(
                "SELECT release FROM usage_admissions WHERE holder = ?1",
                [holder],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert!(
            released.contains("before the worker was recorded"),
            "{released}"
        );
    }
    drop(restarted);
    stop(&fixture, &live);
}

#[test]
fn the_high_latch_closed_from_one_repository_refuses_another() {
    let fixture = Fixture::new("usage-cap-latch-per-account");
    let gate = |repo: &str| {
        fixture.supervisor.usage_gate(
            repo,
            WorkerAgent::Claude,
            None,
            false,
            Some(&gate_holder(repo)),
        )
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
