//! What the clients read: a worker's status, the list, summaries and item
//! counts, its runs and the verification of its commit, and the waits for
//! its turn end, exit or the next event its owner has to handle.

use super::super::*;

impl WorkerSupervisor {
    /// The parts of a status the clients show. Questions are only added or
    /// removed, never replaced in place, and only ever go from quiet to
    /// escalated, so the two counts tell a change.
    #[allow(clippy::type_complexity)] // Only compared with itself, never taken apart.
    pub(in crate::workers) fn shown(
        status: &Status,
    ) -> (WorkerState, bool, bool, usize, usize, bool, bool, bool) {
        (
            status.state,
            status.is_gone(),
            status.in_sidebar(),
            status.questions.len(),
            status
                .questions
                .iter()
                .filter(|pending| status.is_quiet(pending))
                .count(),
            status.session_id.is_some(),
            status.takeover_ms.is_some(),
            status.degraded.is_some(),
        )
    }

    pub(in crate::workers) fn summaries(&self) -> Vec<WorkerSummary> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .map(|entry| WorkerSummary {
                worker_id: entry.status.worker_id.clone(),
                workspace_id: entry.status.workspace_id.clone(),
                name: entry.status.name.clone(),
                cwd: entry.status.cwd.clone(),
                state: entry.status.state,
                session_id: entry.status.session_id.clone(),
                takeover: entry.status.takeover_ms.is_some() && !entry.status.takeover_unfinished,
                item: entry.status.item.clone(),
                item_title: entry.status.item_title.clone(),
                slot: entry.status.slot.clone(),
                branch: entry.status.branch.clone(),
                turns: entry.status.turns,
                started_ms: entry.status.started_ms,
                turn_started_ms: entry
                    .status
                    .turn_started_ms
                    .filter(|_| !entry.status.turn_ended()),
                last_event_ms: entry.status.last_event_ms,
                listed: entry.status.in_sidebar(),
                gone: entry.status.is_gone(),
                end_note: entry.status.end_note.clone(),
                quiet: entry.status.settled(),
                owner_pane: entry.status.owner_pane.clone(),
            })
            .collect()
    }

    /// [`ItemCounts`] of the items of repository `repo`.
    pub(in crate::workers) fn item_counts(&self, repo: &str) -> ItemCounts {
        let registry = lock(&self.shared.registry);
        let mut in_progress = std::collections::BTreeSet::new();
        let mut attention = std::collections::BTreeSet::new();
        for entry in registry.workers.values() {
            let status = &entry.status;
            let Some(item) = &status.item else {
                continue;
            };
            if status.repo.as_deref() != Some(repo) {
                continue;
            }
            let key = item.as_str();
            if !status.is_gone() {
                in_progress.insert(key);
            } else if status.end_unacked() {
                attention.insert(key);
            }
        }
        ItemCounts {
            in_progress: in_progress.len().try_into().unwrap_or(u32::MAX),
            attention: attention.len().try_into().unwrap_or(u32::MAX),
        }
    }

    pub(crate) fn status(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        self.with_entry(worker_id, |entry| entry.status.info(&entry.journal_path))
    }

    /// Every worker's run grouped by its item and repository, the group
    /// whose first run started first first, and the runs without an item.
    pub(crate) fn runs(
        &self,
        params: &WorkerRunsParams,
    ) -> Result<(Vec<WorkerItemRuns>, Vec<WorkerRun>), WorkerError> {
        if let Some(item) = &params.item {
            check_item_id(item)?;
        }
        // Any directory in the repository names it.
        let repo = params
            .repo
            .as_ref()
            .map(|dir| repository_of(Path::new(dir)).unwrap_or_else(|| dir.clone()));
        let registry = lock(&self.shared.registry);
        let mut items: Vec<WorkerItemRuns> = Vec::new();
        let mut unassigned = Vec::new();
        // The registry is ordered by worker number, which is the start order.
        for entry in registry.workers.values() {
            let status = &entry.status;
            if repo.is_some() && status.repo != repo {
                continue;
            }
            if params.item.is_some() && status.item != params.item {
                continue;
            }
            let run = status.run(&entry.journal_path);
            let Some(item) = &status.item else {
                unassigned.push(run);
                continue;
            };
            match items
                .iter_mut()
                .find(|group| group.item == *item && group.repo == status.repo)
            {
                Some(group) => {
                    group.runs.push(run);
                    // The latest run's title: the item may have been renamed.
                    if status.item_title.is_some() {
                        group.title = status.item_title.clone();
                    }
                }
                None => items.push(WorkerItemRuns {
                    item: item.clone(),
                    repo: status.repo.clone(),
                    runs: vec![run],
                    title: status.item_title.clone(),
                }),
            }
        }
        drop(registry);
        // Only items none of whose runs stored a title (workers started
        // before titles were stored) are looked up in TODO.md, after the
        // registry is released: a slow disk must not hold the workers'
        // events.
        let mut titles: HashMap<String, HashMap<String, String>> = HashMap::new();
        for group in &mut items {
            if group.title.is_some() {
                continue;
            }
            let Some(repo) = &group.repo else {
                continue;
            };
            let repo_titles = titles
                .entry(repo.clone())
                .or_insert_with(|| todo_titles::read_titles(Path::new(repo)));
            group.title = repo_titles.get(&group.item).cloned();
        }
        Ok((items, unassigned))
    }

    /// Checks the worker's commit in its directory ([`verify::verify`]) and
    /// records the verdict as its `verification` event, which its run then
    /// shows. Blocks while a command runs, as long as it takes. A second
    /// verification of the same directory while one runs is refused: they
    /// would restore the worktree under each other.
    pub(crate) fn verify(
        &self,
        params: &WorkerVerifyParams,
    ) -> Result<WorkerVerification, WorkerError> {
        self.verify_with(
            params,
            params
                .command
                .as_deref()
                .map(|command| (None, verify::CheckCommand::Shell(command)))
                .into_iter()
                .collect(),
        )
    }

    /// [`Self::verify`] with the command checks given apart, each with its
    /// registered name: `params.command` is not read.
    pub(in crate::workers) fn verify_with(
        &self,
        params: &WorkerVerifyParams,
        commands: Vec<(Option<&str>, verify::CheckCommand<'_>)>,
    ) -> Result<WorkerVerification, WorkerError> {
        let (number, status) = {
            let registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, &params.worker_id)?;
            (number, registry.workers[&number].status.clone())
        };
        if status.cwd.is_empty() {
            return Err(WorkerError::Invalid(format!(
                "worker {} has no recorded directory",
                params.worker_id
            )));
        }
        let dir = PathBuf::from(&status.cwd);
        let mut processes = Vec::new();
        if !status.process_gone() {
            processes.push(format!(
                "the worker's process{}; stop it first (herdr worker stop {})",
                status.pid.map(|pid| format!(" {pid}")).unwrap_or_default(),
                params.worker_id
            ));
        }
        let real = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        for session in status.tool_sessions.keys() {
            for pid in crate::platform::session_members(*session) {
                if slot::runs_in_slot(pid, &real) {
                    processes.push(format!("tool process {pid} (session {session})"));
                }
            }
        }
        let _claim = VerifyClaim::take(&real)?;
        let verification = verify::verify(
            &verify::Request {
                dir: &dir,
                base: &params.base,
                expected_message: &params.expected_message,
                allowed_paths: &params.allowed_paths,
                commands,
                generated: &params.generated,
                env: params.env.as_ref(),
                processes,
            },
            now_ms(),
        );
        self.record(
            number,
            Direction::Herdr,
            &json!({"type": "verification", "verification": verification}),
        );
        Ok(verification)
    }

    pub(crate) fn list(&self) -> Vec<WorkerInfo> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect()
    }

    /// Blocks until the worker reaches `until` (`turn_end` or `exit`),
    /// woken by its events ([`Self::wait_on`]). Returns `None` when the
    /// caller gave up.
    pub(crate) fn wait(
        &self,
        worker_id: &str,
        until: WorkerWaitUntil,
        liveness_check: Duration,
        keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<WorkerInfo>, WorkerError> {
        if until == WorkerWaitUntil::Attention {
            return Err(WorkerError::Invalid(
                "until: attention answers with worker_attention".into(),
            ));
        }
        self.wait_on(worker_id, liveness_check, keep_waiting, |entry| {
            let reached = match until {
                WorkerWaitUntil::Exit => entry.status.is_gone(),
                _ => entry.status.turn_ended(),
            };
            reached.then(|| entry.status.info(&entry.journal_path))
        })
    }

    /// Blocks until the worker needs its coordinator: a pending question
    /// asked after `after`, a turn ended after it, or its end (gone counts
    /// however old: nothing follows it). Level-triggered: what is already
    /// there returns at once. The reply's `seq` is the worker's latest
    /// event's; passed back as `after`, the same state does not wake the
    /// caller again. Returns `None` when the caller gave up.
    ///
    /// A shorthand over the inbox (`worker.events` filtered to one worker):
    /// with `after`, what woke it are the worker's inbox rows after it
    /// ([`inbox::attention`]), checked against what is still pending. A
    /// cursor the inbox cannot serve, a worker whose store write failed and
    /// a wait without `after` are answered from the snapshot, the worker's
    /// status, as `worker.events` answers them.
    pub(crate) fn wait_attention(
        &self,
        worker_id: &str,
        after: Option<i64>,
        liveness_check: Duration,
        keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<Attention>, WorkerError> {
        self.wait_on(worker_id, liveness_check, keep_waiting, |entry| {
            let status = &entry.status;
            let from_inbox = match (after, &self.shared.store) {
                (Some(after), Ok(store)) => store
                    .read(|conn| inbox::attention(conn, status, after))
                    .unwrap_or_else(|error| {
                        warn!(%error, worker_id, "the worker inbox could not be read");
                        None
                    }),
                _ => None,
            };
            let reason = match from_inbox {
                Some(reason) => reason,
                None => status.attention(after),
            };
            reason.map(|reason| Attention {
                reason,
                questions: status.questions.iter().map(Pending::shown).collect(),
                seq: status.last_seq,
                worker: status.info(&entry.journal_path),
            })
        })
    }

    /// Where `worker.wait --attention` without `after` starts for a caller
    /// in `caller_pane`: after the `seq` the worker's owner acknowledged,
    /// when that pane owns it (as in [`Self::obligations`]: through its
    /// tenure, else as the starting pane) and has acknowledged anything.
    /// The owner's loop is then wait, handle, ack, the same wait again,
    /// with no cursor of its own; an ack means handled, so a crash before
    /// it wakes the next wait again. Anyone else starts from the snapshot.
    pub(crate) fn owner_cursor(&self, worker_id: &str, caller_pane: &str) -> Option<i64> {
        let tenure = self
            .coordinator_of_pane(caller_pane)
            .map(|tenure| tenure.coordinator_id);
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id).ok()?;
        let status = &registry.workers[&number].status;
        let owns = match &status.owner_coordinator {
            Some(coordinator) => tenure.as_ref() == Some(coordinator),
            None => status.owner_pane.as_deref() == Some(caller_pane),
        };
        (owns && status.acked_seq > 0).then_some(status.acked_seq)
    }

    /// Blocks until `reached` returns something for the worker, woken by
    /// its events: every commit notifies `changed`. Subscribe before check:
    /// the check and the `seq` it saw are read in one hold of the registry
    /// lock; the lock is let go only to run `keep_waiting`, and an event
    /// committed meanwhile has a higher `seq`, so it is checked before the
    /// wait blocks rather than missed. `keep_waiting` runs at least every
    /// `liveness_check`, only so that a caller whose client went away or
    /// whose server stops can give up; that interval never decides the
    /// outcome. Returns `None` when the caller gave up.
    pub(in crate::workers) fn wait_on<T>(
        &self,
        worker_id: &str,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
        mut reached: impl FnMut(&Entry) -> Option<T>,
    ) -> Result<Option<T>, WorkerError> {
        let missing = || WorkerError::NotFound(worker_id.to_owned());
        let mut registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        loop {
            let entry = registry.workers.get(&number).ok_or_else(missing)?;
            if let Some(value) = reached(entry) {
                return Ok(Some(value));
            }
            let seen = entry.status.last_seq;
            let handed_off = self
                .shared
                .handed_off
                .load(std::sync::atomic::Ordering::SeqCst);
            drop(registry);
            if !keep_waiting() {
                return Ok(None);
            }
            registry = lock(&self.shared.registry);
            let entry = registry.workers.get(&number).ok_or_else(missing)?;
            // A handoff between `keep_waiting` and here (it is set under
            // this lock) asks `keep_waiting` again instead of being lost.
            if entry.status.last_seq != seen
                || self
                    .shared
                    .handed_off
                    .load(std::sync::atomic::Ordering::SeqCst)
                    != handed_off
            {
                continue;
            }
            registry = self
                .shared
                .changed
                .wait_timeout(registry, liveness_check)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    pub(in crate::workers) fn journal_path(&self, worker_id: &str) -> PathBuf {
        self.shared.dir.join(format!("{worker_id}.jsonl"))
    }
}

/// The directories a `worker.verify` runs in now.
static VERIFYING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// A directory claimed by one verification until it is dropped.
struct VerifyClaim(PathBuf);

impl VerifyClaim {
    fn take(dir: &Path) -> Result<Self, WorkerError> {
        let mut verifying = lock(&VERIFYING);
        if verifying.iter().any(|claimed| claimed == dir) {
            return Err(WorkerError::Busy(format!(
                "a verification already runs in {}",
                dir.display()
            )));
        }
        verifying.push(dir.to_owned());
        Ok(Self(dir.to_owned()))
    }
}

impl Drop for VerifyClaim {
    fn drop(&mut self) {
        lock(&VERIFYING).retain(|claimed| *claimed != self.0);
    }
}
