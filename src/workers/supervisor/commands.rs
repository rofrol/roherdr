//! Recording a worker's events and committing them to the store, the
//! command receipts that make a client command run once, and the commands
//! sent to a running worker: prompt, interrupt and the owner's ack.

use super::super::*;

impl WorkerSupervisor {
    /// Records one line of a worker's output that its broker numbered
    /// `broker_seq`, unless the store already holds it (a line the broker
    /// sends again after a re-attach): the seq goes into the store in the
    /// line's event's transaction, so the two never disagree.
    pub(in crate::workers) fn record_spooled(
        &self,
        number: u64,
        broker_seq: u64,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> Spooled {
        let committed = {
            let mut registry = lock(&self.shared.registry);
            match registry.workers.get_mut(&number) {
                None => return Spooled::NotStored,
                Some(entry) if entry.status.broker_seq >= broker_seq => return Spooled::Duplicate,
                Some(entry) => entry.status.broker_seq = broker_seq,
            }
            self.commit_command_locked(&mut registry, number, direction, record, None)
        };
        self.shared.changed.notify_all();
        if committed.shown_changed {
            notify_clients();
        }
        if committed.stored {
            Spooled::Stored
        } else {
            Spooled::NotStored
        }
    }

    /// Records one event of a worker and wakes those waiting on it.
    pub(in crate::workers) fn record(&self, number: u64, direction: Direction, event: &Value) {
        self.record_any(number, direction, store::Recorded::Event(event), None);
    }

    /// Records one event of a client command ([`Self::commit_command_locked`]).
    pub(in crate::workers) fn record_command(
        &self,
        number: u64,
        direction: Direction,
        event: &Value,
        receipt: Option<&Receipt>,
    ) {
        self.record_any(number, direction, store::Recorded::Event(event), receipt);
    }

    /// Records a line the CLI wrote that is not a JSON event.
    pub(in crate::workers) fn record_raw(&self, number: u64, direction: Direction, line: &str) {
        self.record_any(number, direction, store::Recorded::Raw(line), None);
    }

    pub(in crate::workers) fn record_any(
        &self,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
        receipt: Option<&Receipt>,
    ) {
        let committed = {
            let mut registry = lock(&self.shared.registry);
            self.commit_command_locked(&mut registry, number, direction, record, receipt)
        };
        self.shared.changed.notify_all();
        if committed.shown_changed {
            notify_clients();
        }
    }

    /// The one way an event enters a worker's record, under the registry
    /// lock, so the recorded order is the order it is folded in: the event
    /// is folded into the worker's status, then appended to the store with
    /// the projections it changes in one transaction (which gives it its
    /// `seq`), then exported to the JSONL journal. A failed write is not
    /// swallowed: the status keeps the event (it happened) and is marked
    /// degraded with the error. The caller wakes the waiters.
    ///
    /// The event's `seq` in memory is the store's, which only grows; after a
    /// failed write it counts on from the worker's last one instead, so a
    /// wait still sees the event as new, and a later stored event still
    /// counts higher than that.
    pub(in crate::workers) fn commit_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
    ) -> Committed {
        self.commit_command_locked(registry, number, direction, record, None)
    }

    /// [`Self::commit_locked`] for an event of a client command: its
    /// `receipt`, if not reserved yet, is reserved in the same transaction
    /// as the event.
    pub(in crate::workers) fn commit_command_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        direction: Direction,
        record: store::Recorded<'_>,
        receipt: Option<&Receipt>,
    ) -> Committed {
        let receipt = receipt.filter(|receipt| !receipt.reserved.get());
        let Some(entry) = registry.workers.get_mut(&number) else {
            return Committed {
                shown_changed: false,
                seq: None,
                stored: false,
            };
        };
        let shown_before = Self::shown(&entry.status);
        let before = entry.status.before();
        if let store::Recorded::Event(event) = record {
            entry.status.apply(direction, event);
        }
        let ts_ms = now_ms();
        let status = &mut entry.status;
        let foreign = entry.foreign;
        let written = match &self.shared.store {
            Ok(store) => store
                .transaction(|tx| {
                    let seq = tx.event(&store::EventRow {
                        worker_id: &status.worker_id,
                        direction,
                        record: &record,
                        ts_ms,
                    })?;
                    status.mark_seq(
                        seq.max(status.last_seq + 1),
                        ts_ms,
                        &before,
                        direction,
                        &record,
                    );
                    if !foreign {
                        tx.questions(seq, &before.pending, status)?;
                        tx.worker(status, seq)?;
                        inbox::write(tx, seq, &before, status, direction, &record)?;
                    }
                    if let Some(receipt) = receipt {
                        tx.reserve_receipt(&receipt.row(), &status.worker_id, ts_ms)?;
                    }
                    Ok(seq)
                })
                .map_err(|error| format!("a worker store write failed: {error}")),
            Err(error) => Err(error.clone()),
        };
        let seq = match written {
            Ok(seq) => {
                if let Some(receipt) = receipt {
                    receipt.reserved.set(true);
                }
                Some(seq)
            }
            Err(error) => {
                warn!(%error, worker_id = status.worker_id, "worker event not stored");
                status.degraded = Some(error);
                // Marked again past whatever a failed commit marked.
                status.mark_seq(status.last_seq + 1, ts_ms, &before, direction, &record);
                if !foreign {
                    // Its inbox rows are lost with it: their owners resync.
                    registry
                        .inbox_losses
                        .record(inbox::missed_by(&before, status, direction, &record));
                }
                None
            }
        };
        let in_memory = status.last_seq;
        let export = match &entry.export {
            Some(journal) => Ok(Arc::clone(journal)),
            None => Journal::open(&entry.journal_path).map(Arc::new),
        };
        let exported = export.and_then(|journal| {
            entry.export = Some(Arc::clone(&journal));
            journal.export(seq, ts_ms, direction, record)
        });
        if let Err(error) = exported {
            warn!(%error, worker_id = entry.status.worker_id, "worker journal write failed");
            entry.status.degraded = Some(format!("a worker journal write failed: {error}"));
        }
        let mut shown_changed = Self::shown(&entry.status) != shown_before;
        // A question asked while its owner cannot handle one goes to the
        // user at once, as one asked before would have when the owner got
        // stuck.
        let asked = matches!(record, store::Recorded::Event(event)
            if direction == Direction::Herdr && event["type"].as_str() == Some("question"));
        let stuck = entry
            .status
            .owner_pane
            .as_ref()
            .and_then(|pane| registry.stuck_owners.get(pane))
            .cloned();
        if let (true, Some(cause)) = (asked, stuck) {
            if let Some(escalated) = self.escalate_locked(registry, number, &cause) {
                shown_changed |= escalated.shown_changed;
            }
        }
        Committed {
            shown_changed,
            seq: Some(in_memory),
            stored: seq.is_some(),
        }
    }

    /// Writes `moved` (an `owner_moved` event) in `tx` for every worker
    /// tenure `from` owns that its owner still has to handle (running, or
    /// ended and not acknowledged), on copies of their statuses: the
    /// caller puts them in with [`Self::settle_moves`] once `tx` commits.
    pub(in crate::workers) fn stage_owner_moves(
        registry: &Registry,
        tx: &store::Tx<'_>,
        from: &str,
        moved: &Value,
        ts_ms: u64,
    ) -> store::StoreResult<StagedMoves> {
        let mut staged = Vec::new();
        for (number, entry) in &registry.workers {
            let status = &entry.status;
            if status.owner_coordinator.as_deref() != Some(from) || !status.listed() {
                continue;
            }
            let mut status = status.clone();
            let before = status.before();
            status.apply(Direction::Herdr, moved);
            let record = store::Recorded::Event(moved);
            let seq = tx.event(&store::EventRow {
                worker_id: &status.worker_id,
                direction: Direction::Herdr,
                record: &record,
                ts_ms,
            })?;
            status.mark_seq(
                seq.max(status.last_seq + 1),
                ts_ms,
                &before,
                Direction::Herdr,
                &record,
            );
            if !entry.foreign {
                tx.questions(seq, &before.pending, &status)?;
                tx.worker(&status, seq)?;
                inbox::write(tx, seq, &before, &status, Direction::Herdr, &record)?;
            }
            staged.push((*number, status, seq));
        }
        Ok(staged)
    }

    /// Puts in the statuses [`Self::stage_owner_moves`] wrote, once their
    /// transaction committed, and exports their events.
    pub(in crate::workers) fn settle_moves(
        registry: &mut Registry,
        staged: StagedMoves,
        moved: &Value,
        ts_ms: u64,
    ) {
        for (number, status, seq) in staged {
            let Some(entry) = registry.workers.get_mut(&number) else {
                continue;
            };
            entry.status = status;
            let export = match &entry.export {
                Some(journal) => Ok(Arc::clone(journal)),
                None => Journal::open(&entry.journal_path).map(Arc::new),
            };
            let exported = export.and_then(|journal| {
                entry.export = Some(Arc::clone(&journal));
                journal.export(
                    Some(seq),
                    ts_ms,
                    Direction::Herdr,
                    store::Recorded::Event(moved),
                )
            });
            if let Err(error) = exported {
                warn!(%error, worker_id = entry.status.worker_id, "worker journal write failed");
                entry.status.degraded = Some(format!("a worker journal write failed: {error}"));
            }
        }
    }

    pub(in crate::workers) fn live(
        &self,
        worker_id: &str,
    ) -> Result<(u64, Arc<Live>, Status), WorkerError> {
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        let entry = &registry.workers[&number];
        match (&entry.live, entry.status.is_gone()) {
            (Some(live), false) => Ok((number, Arc::clone(live), entry.status.clone())),
            _ => Err(WorkerError::NotRunning(format!(
                "worker {worker_id} is not running"
            ))),
        }
    }

    /// [`Self::prompt_command`] without a command id.
    #[cfg(all(test, unix))]
    pub(crate) fn prompt(&self, worker_id: &str, text: &str) -> Result<WorkerInfo, WorkerError> {
        self.prompt_command(&WorkerPromptParams {
            caller_pane_id: None,
            worker_id: worker_id.to_owned(),
            text: text.to_owned(),
            command_id: None,
        })
    }

    /// Sends the next user message. Accepted only between turns, checked in
    /// the same hold as its record, so a `worker.wait` after it waits for
    /// this turn's end and two prompts never both start one. The reply's
    /// `turn_seq` is the `seq` of the message it appended.
    pub(crate) fn prompt_command(
        &self,
        params: &WorkerPromptParams,
    ) -> Result<WorkerInfo, WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.prompt",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.prompt_once(&params.worker_id, &params.text, receipt),
        )
    }

    pub(in crate::workers) fn prompt_once(
        &self,
        worker_id: &str,
        text: &str,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        if text.trim().is_empty() {
            return Err(WorkerError::Invalid("text must not be empty".into()));
        }
        let (_, live, _) = self.live(worker_id)?;
        let turn_seq =
            live.send_checked(self, &user_message(text), receipt, |status, registry| {
                if status.takeover_ms.is_some() {
                    return Err(Self::taken_over(worker_id));
                }
                if let Some(gap) = &status.continuity_gap {
                    return Err(Self::continuity_gap(worker_id, gap));
                }
                if status.is_gone() {
                    return Err(WorkerError::NotRunning(format!(
                        "worker {worker_id} is not running"
                    )));
                }
                if !status.turn_ended() {
                    return Err(WorkerError::Busy(format!(
                        "worker {worker_id} is in a turn; wait for it or interrupt it first"
                    )));
                }
                Self::admits_turns(registry)
            })?;
        let mut info = self.status(worker_id)?;
        info.turn_seq = Some(turn_seq);
        Ok(info)
    }

    /// Asks the CLI to interrupt its turn (a control request). The turn
    /// then ends with a `result` whose `terminal_reason` is `aborted_*`.
    /// With `turn`, only that turn: once it has ended the interrupt is
    /// refused with `worker_turn_ended` and nothing is sent, so a repeated
    /// interrupt never aborts the next turn. The reply's `turn_seq` names
    /// the turn it interrupted (none when no turn ran).
    pub(crate) fn interrupt(
        &self,
        params: &WorkerInterruptParams,
    ) -> Result<WorkerInfo, WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.interrupt",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.interrupt_once(params, receipt),
        )
    }

    pub(in crate::workers) fn interrupt_once(
        &self,
        params: &WorkerInterruptParams,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        let worker_id = params.worker_id.as_str();
        let (number, live, _) = self.live(worker_id)?;
        let mut interrupted = None;
        let request = json!({
            "type": "control_request",
            "request_id": format!("herdr-interrupt-{number}-{}", now_ms()),
            "request": {"subtype": "interrupt"},
        });
        live.send_checked(self, &request, receipt, |status, _| {
            let running = (!status.turn_ended()).then_some(status.turn_seq).flatten();
            if let Some(turn) = params.turn {
                match status.turn_seq {
                    _ if running == Some(turn) => {}
                    Some(last) if turn <= last => {
                        return Err(WorkerError::TurnEnded(format!(
                            "turn {turn} of worker {worker_id} has already ended; nothing was sent"
                        )))
                    }
                    _ => {
                        return Err(WorkerError::Invalid(format!(
                            "worker {worker_id} has no turn {turn}"
                        )))
                    }
                }
            }
            interrupted = running;
            Ok(())
        })?;
        let mut info = self.status(worker_id)?;
        info.turn_seq = interrupted;
        Ok(info)
    }

    /// Runs a client command once per `command_id` (T3 Code's command
    /// receipts). Without an id, `run` just runs. With one, the id is
    /// claimed under the registry lock: new, `run` gets its receipt, which
    /// the command's first event reserves in the store in the same
    /// transaction, and the outcome (the reply or the refusal) is stored
    /// with it. Seen before, the stored outcome is returned and nothing
    /// runs; one that a server restart cut off goes to `cut_off`. A repeat
    /// while it runs in this server waits for its outcome. The same id with
    /// another method or other parameters is refused.
    pub(in crate::workers) fn command<T: Serialize + DeserializeOwned>(
        &self,
        command_id: Option<&str>,
        method: &'static str,
        params: &impl Serialize,
        cut_off: impl FnOnce(&store::StoredReceipt) -> Result<T, WorkerError>,
        run: impl FnOnce(Option<&Receipt>) -> Result<T, WorkerError>,
    ) -> Result<T, WorkerError> {
        let Some(command_id) = command_id else {
            return run(None);
        };
        if command_id.trim().is_empty() {
            return Err(WorkerError::Invalid("command_id must not be empty".into()));
        }
        let store = self.shared.store.as_ref().map_err(|error| {
            WorkerError::Io(std::io::Error::other(format!(
                "{error}; a command_id needs it (send the command without one)"
            )))
        })?;
        let params = command_params(params);
        let worker_id = params["worker_id"].as_str().map(str::to_owned);
        let receipt = Receipt {
            command_id: command_id.to_owned(),
            method,
            params: params.to_string(),
            reserved: Cell::new(false),
        };
        let in_flight = match self.claim_command(store, &receipt)? {
            Claim::Stored(stored) => {
                return match stored.state {
                    store::ReceiptState::Accepted => stored
                        .result
                        .as_deref()
                        .and_then(|result| serde_json::from_str(result).ok())
                        .ok_or_else(|| {
                            WorkerError::Io(std::io::Error::other(format!(
                                "the stored reply of command {command_id} is unreadable"
                            )))
                        }),
                    store::ReceiptState::Rejected => {
                        let refusal: Value = stored
                            .result
                            .as_deref()
                            .and_then(|result| serde_json::from_str(result).ok())
                            .unwrap_or(Value::Null);
                        Err(WorkerError::replayed(
                            refusal["code"].as_str().unwrap_or(""),
                            refusal["message"].as_str().unwrap_or("refused").to_owned(),
                        ))
                    }
                    store::ReceiptState::Pending => cut_off(&stored),
                };
            }
            Claim::Run(in_flight) => in_flight,
        };
        let outcome = run(Some(&receipt));
        // A drain ends: refused before any event, the id stays unused, so
        // the same command can be sent again once the drain is over.
        if matches!(outcome, Err(WorkerError::Draining(_))) && !receipt.reserved.get() {
            drop(in_flight);
            return outcome;
        }
        let (state, result) = match &outcome {
            Ok(reply) => (
                store::ReceiptState::Accepted,
                serde_json::to_string(reply).unwrap_or_else(|_| "null".into()),
            ),
            Err(error) => (
                store::ReceiptState::Rejected,
                json!({"code": error.code(), "message": error.to_string()}).to_string(),
            ),
        };
        let settled = store.transaction(|tx| {
            tx.settle_receipt(
                &receipt.row(),
                worker_id.as_deref(),
                state,
                &result,
                now_ms(),
            )
        });
        if let Err(error) = settled {
            warn!(%error, command_id, "worker command outcome not stored");
        }
        drop(in_flight);
        outcome
    }

    /// Claims a command id for this call, or finds its stored outcome; see
    /// [`Self::command`]. Waits, woken by the outcome, while this server
    /// runs the same command.
    pub(in crate::workers) fn claim_command<'a>(
        &'a self,
        store: &store::Store,
        receipt: &Receipt,
    ) -> Result<Claim<'a>, WorkerError> {
        let conflict = |method: &str| {
            WorkerError::CommandConflict(format!(
                "command id {} was used for {method} with other parameters; use a new id",
                receipt.command_id
            ))
        };
        let mut registry = lock(&self.shared.registry);
        loop {
            if let Some((method, params)) = registry.commands.get(&receipt.command_id) {
                if (*method, params) != (receipt.method, &receipt.params) {
                    return Err(conflict(method));
                }
                registry = self
                    .shared
                    .changed
                    .wait(registry)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }
            let stored = store
                .receipt(&receipt.command_id)
                .map_err(|error| WorkerError::Io(std::io::Error::other(error.to_string())))?;
            return match stored {
                Some(stored)
                    if (stored.method.as_str(), &stored.params)
                        != (receipt.method, &receipt.params) =>
                {
                    Err(conflict(&stored.method))
                }
                Some(stored) => Ok(Claim::Stored(stored)),
                None => {
                    registry.commands.insert(
                        receipt.command_id.clone(),
                        (receipt.method, receipt.params.clone()),
                    );
                    Ok(Claim::Run(InFlight {
                        supervisor: self,
                        command_id: receipt.command_id.clone(),
                    }))
                }
            };
        }
    }

    /// Why a repeated command id has no outcome: a server restart cut its
    /// command off.
    pub(in crate::workers) fn cut_off(stored: &store::StoredReceipt) -> WorkerError {
        WorkerError::CommandInterrupted(format!(
            "this command id's {} was cut off by a server restart before its outcome was \
             stored; {} shows what it did",
            stored.method,
            match &stored.worker_id {
                Some(worker_id) => format!("`herdr worker status {worker_id}`"),
                None => "the worker's status".to_owned(),
            }
        ))
    }

    /// Records that the worker's owner handled its events up to `seq`
    /// (an `acked` event). Idempotent and monotonic: an ack at or below the
    /// acknowledged `seq` records nothing. A `seq` past the worker's latest
    /// event is refused, since it would hide events that have not happened.
    ///
    /// An ack that leaves a live owned worker between turns with nothing
    /// to handle ([`Status::handled_between_turns`]) stops it as its owner:
    /// an ack means handled, so its process and its folder slot go, and
    /// its exit counts as acknowledged. An owner that wants another turn
    /// prompts before it acknowledges the turn's end.
    pub(crate) fn ack(&self, worker_id: &str, seq: i64) -> Result<WorkerInfo, WorkerError> {
        self.acknowledge(worker_id, seq, true)
    }

    /// [`Self::ack`] for herdr's own drivers (a todo run, an item
    /// coordinator's chain), which stop the worker themselves at their own
    /// step: with `stop_handled` false the ack never stops it.
    pub(crate) fn acknowledge(
        &self,
        worker_id: &str,
        seq: i64,
        stop_handled: bool,
    ) -> Result<WorkerInfo, WorkerError> {
        let (committed, stop_as) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let status = &registry.workers[&number].status;
            if seq > status.last_seq {
                return Err(WorkerError::Invalid(format!(
                    "seq {seq} is past worker {worker_id}'s latest event ({})",
                    status.last_seq
                )));
            }
            if let Some(pane) = status.owner_pane.clone() {
                registry.owner_seen_ms.insert(pane, now_ms());
            }
            let status = &registry.workers[&number].status;
            let committed = if seq <= status.acked_seq {
                None
            } else {
                Some(self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&json!({"type": "acked", "seq": seq})),
                ))
            };
            let status = &registry.workers[&number].status;
            let stop_as = (stop_handled && committed.is_some())
                .then_some(status)
                .filter(|status| status.handled_between_turns())
                .and_then(|status| status.owner_pane.clone());
            (committed, stop_as)
        };
        if let Some(committed) = committed {
            self.shared.changed.notify_all();
            if committed.shown_changed {
                notify_clients();
            }
        }
        if let Some(owner) = stop_as {
            match self.stop_once(worker_id, Some(&owner), None) {
                Ok(_) | Err(WorkerError::NotRunning(_)) => {}
                Err(error) => warn!(%error, worker_id, "cannot stop the acknowledged worker"),
            }
        }
        self.status(worker_id)
    }
}
