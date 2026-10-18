//! Opening the supervisor after a restart or a live handoff, handing its
//! workers to the next server, the drain before an install, and re-attaching
//! to the brokers and spools a gone server left.

use super::super::*;

impl WorkerSupervisor {
    /// Opens the worker directory and its store, and rebuilds the workers
    /// from the store's projections. A journal the store does not hold yet
    /// (written before the store) is imported into it once. A worker that
    /// has not ended and whose lock no server holds was left by a previous
    /// server: it is marked `lost`. One whose lock another server holds
    /// (the old server of a live handoff) is loaded as it is, and marked
    /// `lost` only if its exit is not recorded once that server lets go of
    /// it ([`Self::adopt_when_released`]).
    #[cfg(any(test, not(unix)))]
    pub(crate) fn open(dir: PathBuf, program: PathBuf) -> Self {
        Self::open_with(
            dir,
            program,
            #[cfg(unix)]
            None,
        )
    }

    /// [`Self::open`], starting new workers through `broker` when given.
    /// A worker whose broker still serves it is re-attached either way.
    pub(in crate::workers) fn open_with(
        dir: PathBuf,
        program: PathBuf,
        #[cfg(unix)] broker: Option<broker::Launcher>,
    ) -> Self {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            warn!(%error, dir = %dir.display(), "worker journal directory unavailable");
        }
        // A store whose projections cannot be read is not written either:
        // importing the journals again would duplicate its events.
        let opened = store::Store::open(&dir.join(store::STORE_FILE))
            .and_then(|store| Ok((store.load_all()?, store)))
            .map_err(|error| {
                warn!(%error, dir = %dir.display(), "worker store unavailable");
                format!("the worker store is unavailable: {error}")
            });
        let (mut stored, store) = match opened {
            Ok((stored, store)) => (stored, Ok(store)),
            Err(error) => (BTreeMap::new(), Err(error)),
        };
        // Every journal's number is reserved, readable or not, so a new
        // worker never appends to an old one's journal.
        let mut numbers: Vec<u64> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
                    .then(|| path.file_stem()?.to_str().and_then(worker_number))
                    .flatten()
            })
            .collect();
        numbers.extend(stored.keys().copied());
        numbers.sort_unstable();
        numbers.dedup();

        let mut registry = Registry {
            next_number: 1,
            ..Registry::default()
        };
        let mut owned_elsewhere = Vec::new();
        // Held until the workers left by a gone server are marked lost.
        let mut unowned = Vec::new();
        for number in numbers {
            registry.next_number = registry.next_number.max(number + 1);
            let worker_id = format!("w{number}");
            let path = dir.join(format!("{worker_id}.jsonl"));
            let ownership = journal_ownership(&path).unwrap_or_else(|error| {
                warn!(%error, path = %path.display(), "worker journal lock unavailable");
                Ownership::Unowned { _held: None }
            });
            let loaded = match (stored.remove(&number), &ownership, &store) {
                (Some(mut status), ..) => {
                    status.mark_unfinished_takeover();
                    if let Ok(store) = &store {
                        note_journal_gap(store, &mut status, &path);
                    }
                    Ok(status)
                }
                // A server from before the store runs it and writes only
                // its journal: imported once it lets go.
                (None, Ownership::Owned(_), _) | (None, _, Err(_)) => {
                    replay_journal(&worker_id, &path).map_err(|error| error.to_string())
                }
                (None, Ownership::Unowned { .. }, Ok(store)) => {
                    import_journal(store, &worker_id, &path)
                }
            };
            let status = match loaded {
                Ok(status) => status,
                Err(error) => {
                    warn!(%error, path = %path.display(), "worker journal unreadable");
                    continue;
                }
            };
            let mut entry = Entry::new(status, path);
            match ownership {
                Ownership::Owned(lock) => {
                    entry.foreign = true;
                    owned_elsewhere.push((number, lock));
                }
                Ownership::Unowned { _held } => unowned.push((number, _held)),
            }
            registry.workers.insert(number, entry);
        }
        let supervisor = Self {
            shared: Arc::new(Shared {
                dir,
                program,
                pi_program: Mutex::new(PathBuf::from(pi::PROGRAM)),
                store,
                registry: Mutex::new(registry),
                changed: Condvar::new(),
                slot_lock: Mutex::new(()),
                holder_watches: Mutex::new(BTreeSet::new()),
                handed_off: std::sync::atomic::AtomicBool::new(false),
                #[cfg(unix)]
                broker,
                #[cfg(test)]
                pre_tool_checks: Mutex::new(Vec::new()),
                #[cfg(test)]
                claude_usage: Mutex::new(Ok(runs::low_usage_for_test())),
                #[cfg(test)]
                openai_usage: Mutex::new(Ok(runs::low_usage_for_test())),
                #[cfg(test)]
                deepseek_usage: Mutex::new(Ok(runs::deepseek_usage_for_test())),
                #[cfg(test)]
                usage_gate_config: Mutex::new(crate::config::UsageConfig::default()),
                #[cfg(test)]
                review_program: Mutex::new(None),
                #[cfg(all(test, unix))]
                gated_run_end_failure: Mutex::new(Default::default()),
            }),
        };
        for (number, held) in unowned {
            // Held until the worker is re-attached or marked lost.
            let _held = match held {
                Some(owner_lock) => match supervisor.reattach(number, owner_lock) {
                    None => continue,
                    Some(owner_lock) => Some(owner_lock),
                },
                None => None,
            };
            supervisor.settle_unowned(number);
        }
        for (number, owner_lock) in owned_elsewhere {
            supervisor.adopt_when_released(number, owner_lock);
        }
        // The handoffs a previous server left pending stay frozen.
        supervisor.restore_handoffs();
        // The usage gate's slots of starts a gone server left unrecorded.
        supervisor.reconcile_admissions();
        supervisor
    }

    /// For a worker no server runs any more: records it `lost` unless it
    /// has ended, then removes its temp dir once its process is gone. A
    /// lost worker's process may still run (a server that died without its
    /// workers); its temp dir is then checked again at the next start.
    pub(in crate::workers) fn settle_unowned(&self, number: u64) {
        let process_gone = {
            let mut registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get_mut(&number) else {
                return;
            };
            entry.foreign = false;
            if !entry.status.is_gone() {
                let lost = json!({"type": "lost", "reason": "server restarted"});
                self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&lost),
                );
            }
            let status = &registry.workers[&number].status;
            let process_gone = status.process_gone();
            let answering: Vec<String> = status
                .questions
                .iter()
                .filter(|pending| pending.answering)
                .map(|pending| pending.question.request_id.clone())
                .collect();
            // An answer whose write a previous server did not confirm: its
            // worker can no longer take it, unless its process outlived
            // that server, which is reported, not guessed at.
            for request_id in &answering {
                if process_gone {
                    let expired = json!({
                        "type": "answer_expired",
                        "request_id": request_id,
                        "how": "expired: a server restart found its answer not confirmed sent",
                    });
                    self.commit_locked(
                        &mut registry,
                        number,
                        Direction::Herdr,
                        store::Recorded::Event(&expired),
                    );
                } else {
                    let worker_id = format!("w{number}");
                    warn!(
                        worker_id,
                        request_id,
                        "an answer in flight at a server restart; its worker still runs"
                    );
                    if let Some(entry) = registry.workers.get_mut(&number) {
                        entry.status.degraded = Some(format!(
                            "the answer to question {request_id} was not confirmed sent before \
                             a server restart, and the worker's process still runs outside this \
                             server: it may or may not have received it"
                        ));
                    }
                }
            }
            process_gone
        };
        if process_gone {
            remove_temp_dir(&temp_dir_path(&self.shared.dir, &format!("w{number}")));
        }
    }

    /// Waits, in a thread, until the server that holds a worker's journal
    /// lets go of it (the worker's exit is recorded, or that server ended),
    /// then loads the worker again (from the store, or by importing the
    /// journal a server from before the store wrote) and marks it `lost`
    /// if its exit is not there.
    pub(in crate::workers) fn adopt_when_released(&self, number: u64, owner_lock: File) {
        let supervisor = self.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-worker-owner", move || {
            let worker_id = format!("w{number}");
            let path = supervisor.journal_path(&worker_id);
            if let Err(error) = owner_lock.lock() {
                warn!(%error, path = %path.display(), "cannot wait for the worker journal's lock");
                return;
            }
            let loaded = match &supervisor.shared.store {
                Ok(store) => match store.load(&worker_id) {
                    Ok(Some(mut status)) => {
                        note_journal_gap(store, &mut status, &path);
                        Ok(status)
                    }
                    Ok(None) => import_journal(store, &worker_id, &path),
                    Err(error) => Err(error.to_string()),
                },
                Err(_) => replay_journal(&worker_id, &path).map_err(|error| error.to_string()),
            };
            let mut status = match loaded {
                Ok(status) => status,
                Err(error) => {
                    warn!(%error, path = %path.display(), "worker journal unreadable");
                    return;
                }
            };
            status.mark_unfinished_takeover();
            {
                let mut registry = lock(&supervisor.shared.registry);
                if let Some(entry) = registry.workers.get_mut(&number) {
                    if entry.live.is_none() {
                        entry.status = status;
                    }
                }
            }
            let Some(owner_lock) = supervisor.reattach(number, owner_lock) else {
                supervisor.shared.changed.notify_all();
                notify_clients();
                return;
            };
            supervisor.settle_unowned(number);
            drop(owner_lock);
            supervisor.shared.changed.notify_all();
            notify_clients();
        });
        if let Err(error) = spawned {
            warn!(%error, "worker journal owner wait unavailable");
        }
    }

    /// Readies this server's workers for a live handoff, which cannot carry
    /// their pipes. A worker with a broker survives it, whatever it does:
    /// once the handoff succeeded this server detaches from its broker
    /// ([`Self::detach_for_handoff`]), and the new server re-attaches from
    /// the last broker seq the store holds, replaying what it missed, and
    /// sends again the answers not confirmed sent, which the broker writes
    /// at most once. The others (started without a broker: by a build
    /// before it, or on Windows) end with this server's pipes: without
    /// `force` it refuses while any of their processes is alive, in a turn
    /// or idle between turns, naming each and how to end it. With `force`
    /// it sends each SIGTERM and goes on.
    ///
    /// It never waits for an exit. It runs on the server's main loop, and
    /// waiting there would freeze every pane and client; a deadline would
    /// let a timer decide whether the handoff happens. The caller stops the
    /// idle workers first and waits for each one's exit event
    /// (`herdr worker stop`, then `herdr worker wait --exit`, as
    /// `scripts/herdr_live.sh` does), then asks again. Returns the workers
    /// it signalled.
    pub(crate) fn prepare_for_handoff(&self, force: bool) -> Result<Vec<String>, String> {
        let running: Vec<(String, WorkerState, bool, bool)> = {
            let registry = lock(&self.shared.registry);
            registry
                .workers
                .values()
                .filter(|entry| entry.live.is_some() && !entry.status.is_gone())
                .filter(|entry| !entry.survives_handoff())
                .map(|entry| {
                    (
                        entry.status.worker_id.clone(),
                        entry.status.state,
                        entry.status.turn_ended(),
                        entry.status.stop_requested_ms.is_some(),
                    )
                })
                .collect()
        };
        if running.is_empty() {
            return Ok(Vec::new());
        }
        if !force {
            let named: Vec<String> = running
                .iter()
                .map(|(worker_id, state, turn_ended, stopping)| {
                    let doing = match (stopping, turn_ended, state) {
                        (true, ..) => "stopping",
                        (false, true, _) => "idle between turns",
                        (false, false, WorkerState::Starting) => "starting",
                        (false, false, WorkerState::WaitingApproval) => "waiting for an answer",
                        (false, false, _) => "working",
                    };
                    format!("{worker_id} ({doing})")
                })
                .collect();
            return Err(format!(
                "refusing the live handoff: headless workers are running: {}. Their pipes \
                 are not handed over, so the handoff would end them. End them first: \
                 `herdr worker stop <id>` (safe for one idle between turns; one in a turn \
                 loses that turn), then `herdr worker wait <id> --exit`. Or hand off anyway \
                 with `herdr server live-handoff --force`.",
                named.join(", ")
            ));
        }
        for (worker_id, ..) in &running {
            if let Err(error) = self.stop(worker_id) {
                warn!(%error, worker_id, "cannot stop worker before a forced handoff");
            }
        }
        Ok(running
            .into_iter()
            .map(|(worker_id, ..)| worker_id)
            .collect())
    }

    /// After a live handoff succeeded: lets go of every worker this server
    /// reads through a broker, without closing its input or recording an
    /// exit, so the journal's lock goes to the new server, which waits for
    /// it ([`Self::adopt_when_released`]) and re-attaches. Returns the
    /// workers it detached.
    #[cfg(unix)]
    pub(crate) fn detach_for_handoff(&self) -> Vec<String> {
        let lives: Vec<(u64, Arc<Live>)> = lock(&self.shared.registry)
            .workers
            .iter()
            .filter(|(_, entry)| !entry.status.is_gone())
            .filter_map(|(number, entry)| Some((*number, Arc::clone(entry.live.as_ref()?))))
            .collect();
        // Outside the registry lock: a sender holds the input's lock while
        // it takes the registry's.
        let numbers: Vec<u64> = lives
            .into_iter()
            .filter(|(_, live)| live.detach())
            .map(|(number, _)| number)
            .collect();
        let mut registry = lock(&self.shared.registry);
        numbers
            .into_iter()
            .filter_map(|number| {
                let entry = registry.workers.get_mut(&number)?;
                entry.live = None;
                entry.foreign = true;
                Some(entry.status.worker_id.clone())
            })
            .collect()
    }

    /// The drain before an install: `start` stops admitting new turns
    /// (`worker.prompt` and `worker.start` are refused with
    /// `workers_draining`, naming `reason`), `cancel` admits them again,
    /// `status` changes nothing. Turns already running go on, and answers
    /// and interrupts still reach them. Starting an active drain keeps it.
    /// The drain lives in this server's memory only, so it ends with a
    /// handoff: the new server admits turns.
    pub(crate) fn drain(&self, action: WorkerDrainAction, reason: Option<&str>) -> WorkerDrain {
        let (drain, changed) = {
            let mut registry = lock(&self.shared.registry);
            let changed = match action {
                WorkerDrainAction::Start if registry.drain.is_none() => {
                    let reason = reason
                        .map(str::trim)
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or("an install");
                    registry.drain = Some(Drain {
                        reason: one_line(reason, 200),
                        started_ms: now_ms(),
                    });
                    true
                }
                WorkerDrainAction::Cancel => registry.drain.take().is_some(),
                _ => false,
            };
            (Self::drain_locked(&registry, &[]), changed)
        };
        if changed {
            info!(?action, reason = drain.reason.as_deref(), "worker drain");
            self.shared.changed.notify_all();
        }
        drain
    }

    /// Blocks until the workers in a turn differ from `params.in_turn` (one
    /// of them ended its turn, or another began one), a drain starts or ends
    /// when `params.draining` says what the caller saw, or at once when none
    /// is in a turn and no admitted start is pending. Woken by the workers' events,
    /// never decided by a timer: the state is checked under the registry
    /// lock right before each wait, and every commit notifies after taking
    /// that lock. `keep_waiting` runs at least every `liveness_check`, only
    /// so that a caller whose client went away can give up (`None`).
    pub(crate) fn wait_drained(
        &self,
        params: &WorkerWaitDrainedParams,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Option<WorkerDrain> {
        let seen = &params.in_turn;
        let seen_ids: BTreeSet<&str> = seen.iter().map(String::as_str).collect();
        let reached = |registry: &Registry| {
            let drain = Self::drain_locked(registry, seen);
            let drain_changed = params
                .draining
                .is_some_and(|draining| draining != drain.draining);
            let in_turn: BTreeSet<&str> = drain
                .in_turn
                .iter()
                .map(|worker| worker.worker_id.as_str())
                .collect();
            let settled = in_turn.is_empty() && drain.starting == 0;
            (settled || drain_changed || in_turn != seen_ids).then_some(drain)
        };
        let mut registry = lock(&self.shared.registry);
        loop {
            if let Some(drain) = reached(&registry) {
                return Some(drain);
            }
            drop(registry);
            if !keep_waiting() {
                return None;
            }
            registry = lock(&self.shared.registry);
            if let Some(drain) = reached(&registry) {
                return Some(drain);
            }
            registry = self
                .shared
                .changed
                .wait_timeout(registry, liveness_check)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// The drain and the workers in a turn; `ended` lists those of `seen`
    /// that are not in one any more.
    pub(in crate::workers) fn drain_locked(registry: &Registry, seen: &[String]) -> WorkerDrain {
        let in_turn: Vec<WorkerInfo> = registry
            .workers
            .values()
            .filter(|entry| entry.live.is_some() && !entry.status.turn_ended())
            // A handoff keeps those: their turns go on through the new
            // server.
            .filter(|entry| !entry.survives_handoff())
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect();
        let ended = seen
            .iter()
            .filter(|id| !in_turn.iter().any(|worker| &worker.worker_id == *id))
            .filter_map(|id| registry.workers.get(&worker_number(id)?))
            .map(|entry| entry.status.info(&entry.journal_path))
            .collect();
        WorkerDrain {
            draining: registry.drain.is_some(),
            reason: registry.drain.as_ref().map(|drain| drain.reason.clone()),
            started_ms: registry.drain.as_ref().map(|drain| drain.started_ms),
            in_turn,
            starting: registry.starts_admitted,
            ended,
        }
    }

    /// Refuses a new turn while the server drains.
    pub(in crate::workers) fn admits_turns(registry: &Registry) -> Result<(), WorkerError> {
        match &registry.drain {
            None => Ok(()),
            Some(drain) => Err(WorkerError::Draining(format!(
                "workers are draining for {}: no new turns until it hands off or \
                 `herdr worker drain cancel` admits them again",
                drain.reason
            ))),
        }
    }

    /// Re-attaches to the broker of a worker a gone server ran, when that
    /// broker still serves it: this server reads its output and writes its
    /// input from now on. Takes the worker's journal lock, and gives it back
    /// when there is nothing to re-attach to (no broker, or it is gone, and
    /// with it the worker).
    ///
    /// It attaches after the last broker seq the store holds, so the broker
    /// sends again the lines the gone server read and did not store, then
    /// those written since. A broker that is gone left its spool: the lines
    /// in it are recorded, and the worker's exit when the spool holds it
    /// (then the lock is let go and `None` returned, as after a re-attach).
    pub(in crate::workers) fn reattach(&self, number: u64, owner_lock: File) -> Option<File> {
        #[cfg(unix)]
        return self.reattach_broker(number, owner_lock);
        #[cfg(not(unix))]
        {
            let _ = number;
            Some(owner_lock)
        }
    }

    #[cfg(unix)]
    pub(in crate::workers) fn reattach_broker(
        &self,
        number: u64,
        owner_lock: File,
    ) -> Option<File> {
        let (worker_id, record, pid, cwd_real, after, is_pi) = {
            let registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get(&number) else {
                return Some(owner_lock);
            };
            let status = &entry.status;
            match (&status.broker, status.pid) {
                (Some(record), Some(pid)) if !status.is_gone() && entry.live.is_none() => (
                    status.worker_id.clone(),
                    record.clone(),
                    pid,
                    PathBuf::from(&status.cwd),
                    status.broker_seq,
                    // A pi worker's extension stays beside its journal
                    // until its exit is stored.
                    pi_extension_path(&self.shared.dir, &status.worker_id).exists(),
                ),
                (Some(record), _) if status.is_gone() => {
                    // Left by a server that stored the exit and ended
                    // before it removed the spool.
                    let _ = std::fs::remove_file(broker::spool_path(&record.socket));
                    return Some(owner_lock);
                }
                _ => return Some(owner_lock),
            }
        };
        let link = match broker::connect(&record.socket, after) {
            Ok(link) if link.pid == pid => link,
            Ok(link) => {
                warn!(
                    worker_id,
                    pid,
                    broker_pid = link.pid,
                    "worker broker serves another process"
                );
                return Some(owner_lock);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                // A broker of another build: its worker runs on, but this
                // server cannot read it, nor prove what it missed.
                warn!(worker_id, %error, "cannot re-attach to the worker broker");
                self.record_continuity_gap(number, &error.to_string());
                return Some(owner_lock);
            }
            Err(error) => {
                info!(worker_id, %error, "worker broker gone");
                return self.recover_spool(number, &worker_id, &record, owner_lock);
            }
        };
        let gap = broker::attach_gap(link.floor, after);
        let (input, messages) = match link.split() {
            Ok(split) => split,
            Err(error) => {
                warn!(worker_id, %error, "cannot re-attach to the worker broker");
                return Some(owner_lock);
            }
        };
        let temp_dir = temp_dir_path(&self.shared.dir, &worker_id);
        // A pi worker's gate was proven before its first prompt (or the
        // worker ended), so its next prompts go at once.
        let live = Arc::new(if is_pi {
            Live::new_pi(number, Input::Broker(input), true)
        } else {
            Live::new(number, Input::Broker(input))
        });
        let reader = Reader {
            supervisor: self.clone(),
            number,
            pid,
            policy: policy::Policy::new(&record.cwd, &cwd_real, &temp_dir),
            checks: pre_tool_checks::Runner::new(record.pre_tool_checks.clone(), &cwd_real),
            temp_dir,
            spool: Some(broker::spool_path(&record.socket)),
            live: Arc::clone(&live),
            owner_lock,
            pi: is_pi.then(Default::default),
            pi_extension: is_pi.then(|| pi_extension_path(&self.shared.dir, &worker_id)),
            #[cfg(unix)]
            credit_proxy: None,
        };
        {
            let mut registry = lock(&self.shared.registry);
            if let Some(entry) = registry.workers.get_mut(&number) {
                entry.live = Some(Arc::clone(&live));
                entry.foreign = false;
            }
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&json!({"type": "reattached", "broker_pid": record.pid})),
            );
            if let Some(gap) = &gap {
                warn!(worker_id, gap, "worker output continuity not proven");
                self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&continuity_gap_event(gap)),
                );
            }
        }
        self.shared.changed.notify_all();
        notify_clients();
        reader.settle_stored_requests();
        if let Err(((reader, _), error)) = crate::thread_spawn::spawn_named_with(
            "herdr-worker",
            (reader, messages),
            |(reader, messages)| reader.run_broker(messages, None),
        ) {
            warn!(worker_id, %error, "worker reader unavailable");
            if let Some(entry) = lock(&self.shared.registry).workers.get_mut(&number) {
                entry.live = None;
            }
            return Some(reader.owner_lock);
        }
        self.resend_answers(number, &live);
        self.watch_held_output(number, pid);
        None
    }

    /// Sends again the answers a gone server stored (`answer_intent`) and
    /// did not confirm sent: the broker writes each to the worker unless it
    /// already did ([`input_id`]), so the worker reads it once either way,
    /// and `answer_sent` settles the question; the `input_written` event
    /// says which happened. An answer whose response is not stored (an
    /// intent from before slice 4) cannot be sent again: the worker is
    /// marked degraded, as it may or may not have received it.
    #[cfg(unix)]
    pub(in crate::workers) fn resend_answers(&self, number: u64, live: &Live) {
        let worker_id = format!("w{number}");
        let answering: Vec<String> = lock(&self.shared.registry)
            .workers
            .get(&number)
            .map(|entry| {
                entry
                    .status
                    .questions
                    .iter()
                    .filter(|pending| pending.answering)
                    .map(|pending| pending.question.request_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut unknown = Vec::new();
        for request_id in answering {
            let response = match &self.shared.store {
                Ok(store) => store.answer_response(&worker_id, &request_id),
                Err(_) => Ok(None),
            };
            let response = match response {
                Ok(Some(response)) => response,
                Ok(None) => {
                    unknown.push(request_id);
                    continue;
                }
                Err(error) => {
                    warn!(worker_id, request_id, %error, "stored answer unreadable");
                    unknown.push(request_id);
                    continue;
                }
            };
            let answer = control_response(&request_id, response);
            let outcome = match live.resend(&answer, &input_id(&answer, None, 0)) {
                Ok(()) => json!({"type": "answer_sent", "request_id": request_id, "resent": true}),
                Err(error) => json!({
                    "type": "answer_failed",
                    "request_id": request_id,
                    "error": error.to_string(),
                }),
            };
            self.record(number, Direction::Herdr, &outcome);
        }
        if !unknown.is_empty() {
            if let Some(entry) = lock(&self.shared.registry).workers.get_mut(&number) {
                entry.status.degraded = Some(format!(
                    "the answer to {} was not confirmed sent before a server restart; the \
                     worker may or may not have received it",
                    unknown.join(", ")
                ));
            }
        }
    }

    /// Records what the spool of a worker whose broker is gone holds past
    /// the store's last broker seq; when that ends with the worker's exit,
    /// records it as the reader would have and lets go of `owner_lock`
    /// (`None`), else gives the lock back for the worker to be marked lost.
    #[cfg(unix)]
    pub(in crate::workers) fn recover_spool(
        &self,
        number: u64,
        worker_id: &str,
        record: &BrokerRecord,
        owner_lock: File,
    ) -> Option<File> {
        let spool = broker::spool_path(&record.socket);
        // This server holds the journal lock: its writes are the worker's
        // record now.
        if let Some(entry) = lock(&self.shared.registry).workers.get_mut(&number) {
            entry.foreign = false;
        }
        let Some((seq, exited)) = self.replay_spool(number, &spool) else {
            return Some(owner_lock);
        };
        remove_temp_dir(&temp_dir_path(&self.shared.dir, worker_id));
        let spooled = self.record_spooled(
            number,
            seq,
            Direction::Herdr,
            store::Recorded::Event(&exited),
        );
        if spooled.is_stored() {
            let _ = std::fs::remove_file(&spool);
        }
        drop(owner_lock);
        None
    }

    /// Records the lines in a worker's spool after the store's last broker
    /// seq, for a broker that is gone, and returns the worker's exit with its
    /// seq when the spool holds it. The worker is gone too, so a permission
    /// request among them is recorded, not answered.
    #[cfg(unix)]
    pub(in crate::workers) fn replay_spool(
        &self,
        number: u64,
        spool: &Path,
    ) -> Option<(u64, Value)> {
        let after = lock(&self.shared.registry)
            .workers
            .get(&number)?
            .status
            .broker_seq;
        let records = match broker::read_spool(spool, after) {
            Ok(read) => {
                if let Some(gap) = &read.gap {
                    self.record_continuity_gap(number, gap);
                }
                read.records
            }
            Err(error) => {
                let gap = if error.kind() == std::io::ErrorKind::NotFound {
                    "the worker's broker is gone and left no output spool".to_owned()
                } else {
                    format!(
                        "the worker's broker is gone and its output spool cannot be read: {error}"
                    )
                };
                self.record_continuity_gap(number, &gap);
                return None;
            }
        };
        for (seq, message) in records {
            match message {
                broker::Message::Out(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&line) {
                        Ok(event) => self.record_spooled(
                            number,
                            seq,
                            Direction::Out,
                            store::Recorded::Event(&event),
                        ),
                        Err(_) => self.record_spooled(
                            number,
                            seq,
                            Direction::Out,
                            store::Recorded::Raw(&line),
                        ),
                    };
                }
                broker::Message::Err(line) => {
                    self.record_spooled(number, seq, Direction::Err, store::Recorded::Raw(&line));
                }
                broker::Message::Lost(lines) => {
                    self.record_spooled(
                        number,
                        seq,
                        Direction::Herdr,
                        store::Recorded::Event(&output_lost(lines)),
                    );
                }
                broker::Message::Written { id, again } => {
                    self.record_spooled(
                        number,
                        seq,
                        Direction::Herdr,
                        store::Recorded::Event(&input_written(&id, again)),
                    );
                }
                broker::Message::Exit(exited) => return Some((seq, exited)),
            }
        }
        None
    }

    /// Records that the worker's stored record is not proven to hold every
    /// line it wrote, and why ([`Status::continuity_gap`]).
    #[cfg(unix)]
    pub(in crate::workers) fn record_continuity_gap(&self, number: u64, gap: &str) {
        warn!(
            worker_id = format!("w{number}"),
            gap, "worker output continuity not proven"
        );
        self.record(number, Direction::Herdr, &continuity_gap_event(gap));
    }

    /// Claims a worker for a takeover and journals it. Refused while a
    /// question waits on the user (answer it first, or the interrupt would
    /// throw the answer away), for a worker without a session yet, for one
    /// that was lost while its process still runs (it is not ours to end),
    /// and while another
    /// takeover holds the claim or has opened its tab. The claim is kept in
    /// every state, so an exited worker is taken over once too; a failed
    /// takeover releases it ([`Self::fail_takeover`]). One an earlier server
    /// left unfinished, whose tab recovery did not find
    /// ([`Self::adopt_takeover_tab`]), is tried again only with `force`:
    /// that server may have opened a tab nobody saw, and a second tab would
    /// be a second writer of the session. The new claim replaces it under
    /// the lock, so concurrent retries are serialized.
    /// The refusal of a prompt or takeover of a worker whose output after a
    /// server restart is not proven complete.
    pub(in crate::workers) fn continuity_gap(worker_id: &str, gap: &str) -> WorkerError {
        WorkerError::ContinuityGap(format!(
            "worker {worker_id}'s output since a server restart is not proven complete ({gap}), \
             so its state and session may be stale: it takes no new prompt and no takeover; \
             stop it and start a new worker"
        ))
    }
}
