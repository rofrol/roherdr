//! Taking a worker over into a terminal tab: claiming it, recording the
//! tab, ending the headless process, and finding the tab of a takeover an
//! earlier server left unfinished.

use super::super::*;

impl WorkerSupervisor {
    /// Why a running worker refuses a prompt or an answer: a takeover is
    /// ending it.
    pub(in crate::workers) fn taken_over(worker_id: &str) -> WorkerError {
        WorkerError::Busy(format!(
            "worker {worker_id} is being taken over; its session resumes in a tab"
        ))
    }

    pub(crate) fn begin_takeover(
        &self,
        worker_id: &str,
        force: bool,
    ) -> Result<Takeover, WorkerError> {
        let takeover = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let status = &entry.status;
            if let Some(gap) = &status.continuity_gap {
                return Err(Self::continuity_gap(worker_id, gap));
            }
            if status.lost && !status.process_gone() {
                return Err(WorkerError::NotRunning(format!(
                    "worker {worker_id} was lost: its process is not this server's to end"
                )));
            }
            if !status.questions.is_empty() {
                return Err(WorkerError::Busy(format!(
                    "worker {worker_id} waits on your answer; answer it before taking over"
                )));
            }
            if status.takeover_ms.is_some() && !status.takeover_unfinished {
                return Err(WorkerError::Busy(match &status.takeover_tab {
                    Some(tab_id) => format!(
                        "worker {worker_id} was already taken over: its session resumes in tab {tab_id}"
                    ),
                    None => format!("worker {worker_id} is already being taken over"),
                }));
            }
            if status.takeover_unfinished && !force {
                return Err(WorkerError::Busy(unfinished_takeover_refusal(worker_id)));
            }
            if pi_extension_path(&self.shared.dir, worker_id).exists() {
                return Err(WorkerError::Unsupported(format!(
                    "worker {worker_id} is a pi worker, which runs without a session file: \
                     there is no session to resume in a tab"
                )));
            }
            let session_id = status.session_id.clone().ok_or_else(|| {
                WorkerError::NotRunning(format!("worker {worker_id} has no session yet"))
            })?;
            let at_ms = now_ms();
            let takeover = Takeover {
                worker_id: worker_id.to_owned(),
                workspace_id: status.workspace_id.clone(),
                name: status.name.clone(),
                cwd: status.cwd.clone(),
                session_id,
                takeover_id: new_takeover_id(worker_id, at_ms),
            };
            let event = json!({
                "type": "takeover",
                "at_ms": at_ms,
                "takeover_id": takeover.takeover_id,
                "forced": status.takeover_unfinished,
            });
            // Claimed under the lock, so a second takeover is refused.
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&event),
            );
            takeover
        };
        self.shared.changed.notify_all();
        notify_clients();
        Ok(takeover)
    }

    /// Releases a takeover's claim with a journaled `takeover_failed`, so the
    /// user can try again: its thread did not start, ending the worker
    /// failed, or its tab could not be opened.
    pub(crate) fn fail_takeover(&self, worker_id: &str, error: &str) -> Result<(), WorkerError> {
        self.record_takeover_step(
            worker_id,
            json!({"type": "takeover_failed", "error": error, "at_ms": now_ms()}),
        )
    }

    /// Journals the tab that resumes a taken-over worker's session; until
    /// then a restart reports the takeover as unfinished.
    pub(crate) fn takeover_tab_opened(
        &self,
        worker_id: &str,
        tab_id: &str,
    ) -> Result<(), WorkerError> {
        self.record_takeover_step(
            worker_id,
            json!({"type": "takeover_tab_opened", "tab_id": tab_id, "at_ms": now_ms()}),
        )
    }

    /// The takeovers an earlier server left unfinished, for recovery to look
    /// for their tabs.
    pub(crate) fn unfinished_takeovers(&self) -> Vec<UnfinishedTakeover> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .filter_map(|entry| unfinished_takeover_of(&entry.status))
            .collect()
    }

    /// The worker's takeover an earlier server left unfinished, if any.
    pub(crate) fn unfinished_takeover(
        &self,
        worker_id: &str,
    ) -> Result<Option<UnfinishedTakeover>, WorkerError> {
        self.with_entry(worker_id, |entry| unfinished_takeover_of(&entry.status))
    }

    /// Adopts the tab recovery found for an unfinished takeover: journals
    /// `takeover_tab_opened` with it, as the server that opened it would
    /// have. Only while that same claim is still unfinished, so a claim
    /// taken meanwhile is not given another claim's tab.
    pub(crate) fn adopt_takeover_tab(
        &self,
        takeover: &UnfinishedTakeover,
        tab_id: &str,
    ) -> Result<(), WorkerError> {
        let worker_id = takeover.worker_id.as_str();
        {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if unfinished_takeover_of(&entry.status).as_ref() != Some(takeover) {
                return Err(WorkerError::Invalid(format!(
                    "worker {worker_id} has no such unfinished takeover"
                )));
            }
            let event = json!({
                "type": "takeover_tab_opened",
                "tab_id": tab_id,
                "adopted": true,
                "at_ms": now_ms(),
            });
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&event),
            );
        }
        self.shared.changed.notify_all();
        notify_clients();
        Ok(())
    }

    /// Records a step of a claimed takeover under the lock.
    pub(in crate::workers) fn record_takeover_step(
        &self,
        worker_id: &str,
        event: Value,
    ) -> Result<(), WorkerError> {
        {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if entry.status.takeover_ms.is_none() {
                return Err(WorkerError::Invalid(format!(
                    "worker {worker_id} has no takeover claimed"
                )));
            }
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&event),
            );
        }
        self.shared.changed.notify_all();
        notify_clients();
        Ok(())
    }

    /// Ends a worker being taken over, so its session has one writer: the
    /// running turn is interrupted and its `result` (`aborted_*`) awaited,
    /// then the worker is stopped and its exit awaited. Blocks on those
    /// events; no timer decides anything, so a worker that ignores SIGTERM
    /// keeps this waiting until the user force-stops it (`worker.kill`).
    pub(crate) fn end_for_takeover(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        // Only to look at nothing: the waits below end on the worker's
        // events, never on this interval.
        const RECHECK: Duration = Duration::from_secs(60);
        let (gone, status) = self.with_entry(worker_id, |entry| {
            (
                entry.status.is_gone(),
                entry.status.info(&entry.journal_path),
            )
        })?;
        if gone {
            return Ok(status);
        }
        let in_turn = !matches!(
            status.state,
            WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
        );
        if in_turn {
            let interrupt = WorkerInterruptParams {
                caller_pane_id: None,
                worker_id: worker_id.to_owned(),
                turn: status.turn_seq,
                command_id: None,
            };
            match self.interrupt(&interrupt) {
                // The turn ended on its own meanwhile.
                Ok(_) | Err(WorkerError::TurnEnded(_)) => {}
                Err(error) => return Err(error),
            }
            self.wait(worker_id, WorkerWaitUntil::TurnEnd, RECHECK, || true)?;
        }
        match self.stop(worker_id) {
            // It exited between the wait and the stop.
            Ok(_) | Err(WorkerError::NotRunning(_)) => {}
            Err(error) => return Err(error),
        }
        self.wait(worker_id, WorkerWaitUntil::Exit, RECHECK, || true)?
            .ok_or_else(|| WorkerError::NotRunning(format!("worker {worker_id} wait ended")))
    }
}

fn unfinished_takeover_of(status: &Status) -> Option<UnfinishedTakeover> {
    status.takeover_unfinished.then(|| UnfinishedTakeover {
        worker_id: status.worker_id.clone(),
        takeover_id: status.takeover_id.clone(),
        session_id: status.session_id.clone(),
    })
}

/// Why a takeover an earlier server left unfinished is not tried again
/// without `force`.
pub(crate) fn unfinished_takeover_refusal(worker_id: &str) -> String {
    format!(
        "worker {worker_id}: an earlier server claimed its takeover and ended before \
         recording a tab, and no tab carrying that takeover's id and no process \
         resuming its session was found; that server may still have opened a tab. \
         Retrying with --force opens another tab on the same session: if the \
         first one exists, two writers fork its transcript"
    )
}

/// The process facts recovery reads; [`LiveProcesses`] reads the system.
pub(crate) trait ProcessProbe {
    fn env_value(&self, pid: u32, key: &str) -> Option<String>;
    /// The live processes resuming the agent session `session_id`.
    fn resuming(&self, session_id: &str) -> Vec<u32>;
    fn parent(&self, pid: u32) -> Option<u32>;
}

/// The system's processes, through [`crate::platform`].
pub(crate) struct LiveProcesses;

impl ProcessProbe for LiveProcesses {
    fn env_value(&self, pid: u32, key: &str) -> Option<String> {
        crate::platform::process_env_value(pid, key)
    }

    fn resuming(&self, session_id: &str) -> Vec<u32> {
        crate::platform::processes_with_argv(&|argv| {
            crate::platform::argv_resumes_session(argv, session_id)
        })
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        crate::platform::process_parent(pid)
    }
}

/// Finds an unfinished takeover's tab among `tabs`: the one whose title or
/// pane shell's environment carries its id, else the one whose pane shell
/// is an ancestor of a process resuming its session. A resuming process
/// under no tab is reported as such.
pub(crate) fn find_takeover_tab(
    takeover: &UnfinishedTakeover,
    tabs: &[TakeoverTabCandidate],
    probe: &dyn ProcessProbe,
) -> TakeoverFound {
    if let Some(takeover_id) = takeover.takeover_id.as_deref() {
        let carries_id = |tab: &&TakeoverTabCandidate| {
            tab.title.contains(takeover_id)
                || tab.shell_pids.iter().any(|pid| {
                    probe.env_value(*pid, TAKEOVER_ID_ENV).as_deref() == Some(takeover_id)
                })
        };
        if let Some(tab) = tabs.iter().find(carries_id) {
            return TakeoverFound::Tab(tab.tab_id.clone());
        }
    }
    let Some(session_id) = takeover.session_id.as_deref() else {
        return TakeoverFound::Nothing;
    };
    // Bounds a walk through a parent table that changed under it.
    const MAX_DEPTH: usize = 64;
    let mut outside = None;
    for pid in probe.resuming(session_id) {
        let mut ancestor = Some(pid);
        for _ in 0..MAX_DEPTH {
            let Some(current) = ancestor.filter(|pid| *pid > 1) else {
                break;
            };
            if let Some(tab) = tabs.iter().find(|tab| tab.shell_pids.contains(&current)) {
                return TakeoverFound::Tab(tab.tab_id.clone());
            }
            ancestor = probe.parent(current).filter(|parent| *parent != current);
        }
        outside.get_or_insert(pid);
    }
    outside.map_or(TakeoverFound::Nothing, TakeoverFound::Process)
}

/// The takeover id of a new claim, unique per claim: this server's pid and
/// a counter tell its claims apart, `at_ms` those of servers before it.
fn new_takeover_id(worker_id: &str, at_ms: u64) -> String {
    static CLAIMS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let claim = CLAIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "takeover-{worker_id}-{at_ms:x}-{:x}-{claim}",
        std::process::id()
    )
}
