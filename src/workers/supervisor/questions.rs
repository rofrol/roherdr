//! The questions workers ask: their answers, their escalation to the user
//! and what an owner pane's events mean for them.

use super::super::*;

impl WorkerSupervisor {
    /// Hands every question of worker `number` that waits quietly for its
    /// owner to the user, as one `escalated` event naming them and `cause`.
    /// Records nothing when none waits quietly.
    pub(in crate::workers) fn escalate_locked(
        &self,
        registry: &mut Registry,
        number: u64,
        cause: &str,
    ) -> Option<Committed> {
        let quiet = registry.workers.get(&number)?.status.quiet_questions();
        if quiet.is_empty() {
            return None;
        }
        let escalated = json!({"type": "escalated", "request_ids": quiet, "cause": cause});
        Some(self.commit_locked(
            registry,
            number,
            Direction::Herdr,
            store::Recorded::Event(&escalated),
        ))
    }

    pub(in crate::workers) fn pending_questions(&self) -> Vec<PendingWorkerQuestion> {
        let registry = lock(&self.shared.registry);
        let mut pending: Vec<PendingWorkerQuestion> = registry
            .workers
            .values()
            .flat_map(|entry| {
                entry
                    .status
                    .questions
                    .iter()
                    .filter(|pending| !pending.answering)
                    .map(|pending| PendingWorkerQuestion {
                        worker_id: entry.status.worker_id.clone(),
                        cwd: entry.status.cwd.clone(),
                        question: pending.shown(),
                        quiet: entry.status.is_quiet(pending),
                        owner_seen_ms: entry
                            .status
                            .owner_pane
                            .as_ref()
                            .and_then(|pane| registry.owner_seen_ms.get(pane))
                            .copied(),
                    })
            })
            .collect();
        // A todo run's review escalations, listed as its worker's.
        pending.extend(runs::escalations::pending_questions());
        pending.extend(runs::pending_usage::pending_questions());
        // The definitions waiting for the user's grant.
        pending.extend(grant_questions::pending_questions());
        // The supervision findings nobody acknowledged yet.
        pending.extend(supervision::pending_questions());
        pending
    }

    /// The owned workers with an event their owner has not acknowledged
    /// ([`Status::obligation`]), only those `owner_pane` owns when given.
    /// A worker started by a coordinator is owed by its tenure, not by a
    /// pane: it belongs to the pane its tenure is bound to now, so a
    /// coordinator resumed elsewhere keeps it and another agent in its old
    /// pane does not get it. Derived from the workers' state on every call:
    /// nothing to deliver, nothing to lose.
    pub(crate) fn obligations(&self, owner_pane: Option<&str>) -> Vec<WorkerObligation> {
        let tenure = owner_pane
            .and_then(|pane| self.coordinator_of_pane(pane))
            .map(|tenure| tenure.coordinator_id);
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .filter_map(|entry| {
                let status = &entry.status;
                let owner = status.owner_pane.as_deref()?;
                let owns = |pane: &str| match &status.owner_coordinator {
                    Some(coordinator) => tenure.as_ref() == Some(coordinator),
                    None => pane == owner,
                };
                if owner_pane.is_some_and(|pane| !owns(pane)) {
                    return None;
                }
                let (reason, questions) = status.obligation()?;
                Some(WorkerObligation {
                    worker_id: status.worker_id.clone(),
                    name: status.name.clone(),
                    reason,
                    seq: status.last_seq,
                    questions,
                    owner_pane_id: owner.to_owned(),
                    owner_session_id: status.owner_session.clone(),
                })
            })
            .collect()
    }

    /// The owner hands one pending question to the user (`worker.escalate`).
    /// Idempotent: a question already escalated, or of a worker without an
    /// owner (which asks the user anyway), records nothing.
    pub(crate) fn escalate(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerInfo, WorkerError> {
        if runs::escalations::is_escalation(request_id) {
            // A review escalation is the user's already.
            return self.escalation_reply(worker_id);
        }
        if grant_questions::is_grant_question(request_id) {
            // So is a grant question.
            return self.grant_reply_of(worker_id);
        }
        if supervision::is_finding_question(request_id) {
            // And a supervision finding.
            return self.finding_reply_of(worker_id);
        }
        self.escalate_because(worker_id, request_id, "its coordinator escalated it")
    }

    /// [`Self::escalate`] with the cause the user's `?` list shows: a todo
    /// run's automatic answer names its run, its item and the model's
    /// question with its options.
    pub(crate) fn escalate_because(
        &self,
        worker_id: &str,
        request_id: &str,
        cause: &str,
    ) -> Result<WorkerInfo, WorkerError> {
        let committed = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let status = &registry.workers[&number].status;
            if !status
                .questions
                .iter()
                .any(|pending| pending.question.request_id == request_id)
            {
                return Err(match status.resolution(request_id) {
                    Some(how) => WorkerError::QuestionGone(format!(
                        "question {request_id} of worker {worker_id} is no longer pending: {how}"
                    )),
                    None => WorkerError::NoQuestion(format!(
                        "worker {worker_id} has no question {request_id}"
                    )),
                });
            }
            if let Some(pane) = status.owner_pane.clone() {
                registry.owner_seen_ms.insert(pane, now_ms());
            }
            let status = &registry.workers[&number].status;
            let pending = status
                .questions
                .iter()
                .find(|pending| pending.question.request_id == request_id);
            if pending.is_some_and(|pending| status.is_quiet(pending)) {
                let escalated = json!({
                    "type": "escalated",
                    "request_ids": [request_id],
                    "cause": cause,
                });
                Some(self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(&escalated),
                ))
            } else {
                None
            }
        };
        if let Some(committed) = committed {
            self.shared.changed.notify_all();
            if committed.shown_changed {
                notify_clients();
            }
        }
        self.status(worker_id)
    }

    /// One question the worker waits on, with the tool's whole input, for an
    /// answer dialog. One whose answer is being sent or that has ended is
    /// refused with `worker_question_gone`, saying how it ended.
    pub(crate) fn question_detail(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerQuestionDetail, WorkerError> {
        if let Some(detail) = runs::pending_usage::detail(worker_id, request_id) {
            return detail;
        }
        if runs::escalations::is_escalation(request_id) {
            return self.escalation_detail(worker_id, request_id);
        }
        if grant_questions::is_grant_question(request_id) {
            return self.grant_question_detail(worker_id, request_id);
        }
        if supervision::is_finding_question(request_id) {
            return self.finding_question_detail(worker_id, request_id);
        }
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        let status = &registry.workers[&number].status;
        let Some(pending) = status
            .questions
            .iter()
            .find(|pending| pending.question.request_id == request_id)
        else {
            return Err(match status.resolution(request_id) {
                Some(how) => WorkerError::QuestionGone(format!(
                    "question {request_id} of worker {worker_id} is no longer pending: {how}"
                )),
                None => WorkerError::NoQuestion(format!(
                    "worker {worker_id} has no question {request_id}"
                )),
            });
        };
        if pending.answering {
            return Err(WorkerError::QuestionGone(format!(
                "question {request_id} of worker {worker_id} is no longer pending: its answer \
                 is being sent"
            )));
        }
        Ok(WorkerQuestionDetail {
            worker_id: status.worker_id.clone(),
            name: status.name.clone(),
            cwd: status.cwd.clone(),
            state: status.state,
            question: pending.shown(),
            input_text: full_input_text(&pending.input),
            owner_pane_id: status.owner_pane.clone(),
            owner_coordinator_id: status.owner_coordinator.clone(),
            quiet: status.is_quiet(pending),
        })
    }

    /// Denies the question `request_id` names, then stops the worker. A
    /// question that is no longer pending (answered or ended meanwhile)
    /// does not keep the worker running: the user asked to end it.
    pub(crate) fn deny_and_stop(
        &self,
        params: &WorkerDenyAndStopParams,
    ) -> Result<WorkerInfo, WorkerError> {
        if runs::pending_usage::is_question(&params.request_id) {
            return Err(WorkerError::Invalid("cancel usage waits from the user's client or todo cancel-pending as the run's owner".into()));
        }
        // A grant question is answered only from the user's client
        // ([`Self::deny_and_stop_from_client`]).
        if grant_questions::is_grant_question(&params.request_id) {
            return Err(grant_questions::needs_user(&params.request_id));
        }
        // A supervision finding is acknowledged only by the user's client
        // ([`Self::deny_and_stop_from_client`]) or the herdr coordinator
        // (`supervise.acknowledge`), never through this API.
        if supervision::is_finding_question(&params.request_id) {
            return self.refuse_finding_answer(&params.worker_id, &params.request_id);
        }
        // A review escalation's worker has already exited: the denial
        // leaves the review to the coordinator.
        if runs::escalations::is_escalation(&params.request_id) {
            self.answer_escalation(
                &params.worker_id,
                &params.request_id,
                Some(WorkerDecision::Deny),
                &[],
                params.message.as_deref(),
            )?;
            return self.escalation_reply(&params.worker_id);
        }
        let deny = WorkerAnswerParams {
            worker_id: params.worker_id.clone(),
            request_id: Some(params.request_id.clone()),
            decision: Some(WorkerDecision::Deny),
            answers: Vec::new(),
            message: Some(
                params
                    .message
                    .clone()
                    .filter(|message| !message.trim().is_empty())
                    .unwrap_or_else(|| "The user stopped this worker.".to_owned()),
            ),
            command_id: None,
        };
        match self.answer(&deny) {
            Ok(_) | Err(WorkerError::QuestionGone(_) | WorkerError::NoQuestion(_)) => {}
            Err(error) => return Err(error),
        }
        self.stop(&params.worker_id)
    }

    /// The owner panes of the workers that have not ended and whose owner is
    /// not gone, the panes of the active coordination tenures and the
    /// owner panes of the `todo.run`s not ended whose owner is not gone:
    /// those [`Self::owner_event`] can still act on.
    pub(in crate::workers) fn owner_panes(&self) -> Vec<String> {
        let tenure_panes = self.coordinator_panes().unwrap_or_else(|error| {
            warn!(%error, "cannot read the coordination tenures' panes");
            Vec::new()
        });
        let run_panes = self.run_owner_panes();
        let registry = lock(&self.shared.registry);
        let run_panes = run_panes
            .into_iter()
            .filter(|pane| !registry.gone_owners.contains_key(pane));
        let mut panes: Vec<String> = registry
            .workers
            .values()
            .filter(|entry| !entry.status.is_gone() && entry.status.owner_gone.is_none())
            .filter_map(|entry| entry.status.owner_pane.clone())
            .chain(tenure_panes)
            .chain(run_panes)
            .collect();
        panes.sort();
        panes.dedup();
        panes
    }

    /// Why owner pane `pane_id` is gone (its pane closed or its agent
    /// exited), as herdr's events reported it; `None` while it is there.
    pub(in crate::workers) fn gone_owner(&self, pane_id: &str) -> Option<String> {
        lock(&self.shared.registry)
            .gone_owners
            .get(pane_id)
            .cloned()
    }

    /// [`owners_at_start`] for this supervisor.
    pub(in crate::workers) fn owners_at_start(&self, state: impl Fn(&str) -> Option<OwnerEvent>) {
        for pane in self.owner_panes() {
            if let Some(event) = state(&pane) {
                self.owner_event(&pane, event, " (found when herdr started)");
            }
        }
    }

    /// What the agent in owner pane `pane_id` just did, as herdr's events
    /// report it, and what that means for its workers' questions that wait
    /// quietly for it ([`OwnerEvent`]). `cause_suffix` is added to each
    /// escalation's cause (" (found when herdr started)").
    ///
    /// Escalation follows events only, never a timer (the user's decision,
    /// 2026-10-08). Known limitation: an owner that is alive but hung, its
    /// agent reported `working` for ever with no event, keeps its questions
    /// quiet; the `?` list shows them as awaiting the coordinator with the
    /// age of the owner's last event, so the user can see it and act.
    pub(crate) fn owner_event(&self, pane_id: &str, event: OwnerEvent, cause_suffix: &str) {
        // The pane's coordination tenure ends with its pane or agent.
        if let (OwnerEvent::PaneClosed | OwnerEvent::AgentExited, Some(cause)) =
            (event, event.cause())
        {
            self.orphan_coordinator(pane_id, &format!("{cause}{cause_suffix}"));
        }
        let mut any = false;
        {
            let mut registry = lock(&self.shared.registry);
            // Also for an owner of no worker now: a run it owns may start
            // its next attempt's worker later, or be resumed elsewhere.
            match (event, event.cause()) {
                (OwnerEvent::PaneClosed | OwnerEvent::AgentExited, Some(cause)) => {
                    registry
                        .gone_owners
                        .insert(pane_id.to_owned(), format!("{cause}{cause_suffix}"));
                }
                // An agent works in the pane again: it is there.
                (OwnerEvent::Working, _) => {
                    registry.gone_owners.remove(pane_id);
                }
                _ => {}
            }
            let owned: Vec<u64> = registry
                .workers
                .iter()
                .filter(|(_, entry)| {
                    !entry.status.is_gone()
                        && entry.status.owner_pane.as_deref() == Some(pane_id)
                        && entry.status.owner_gone.is_none()
                })
                .map(|(number, _)| *number)
                .collect();
            if owned.is_empty() {
                return;
            }
            registry.owner_seen_ms.insert(pane_id.to_owned(), now_ms());
            let cause = event.cause().map(|cause| format!("{cause}{cause_suffix}"));
            match event {
                OwnerEvent::PaneClosed | OwnerEvent::AgentExited => {
                    registry.stuck_owners.remove(pane_id);
                }
                OwnerEvent::Limited | OwnerEvent::Blocked => {
                    if let Some(cause) = &cause {
                        registry
                            .stuck_owners
                            .insert(pane_id.to_owned(), cause.clone());
                    }
                }
                OwnerEvent::TurnEnded => {}
                OwnerEvent::Working => {
                    registry.stuck_owners.remove(pane_id);
                }
            }
            for number in owned {
                let committed = match (&event, &cause) {
                    (OwnerEvent::PaneClosed | OwnerEvent::AgentExited, Some(cause)) => {
                        let gone = json!({"type": "owner_gone", "cause": cause});
                        Some(self.commit_locked(
                            &mut registry,
                            number,
                            Direction::Herdr,
                            store::Recorded::Event(&gone),
                        ))
                    }
                    (_, Some(cause)) => self.escalate_locked(&mut registry, number, cause),
                    (_, None) => None,
                };
                any |= committed.is_some();
            }
        }
        if any {
            self.shared.changed.notify_all();
        }
        // Also when nothing escalated: a quiet entry shows the owner's last
        // event's age.
        notify_clients();
    }

    /// Answers the worker's pending question that `request_id` names, with
    /// the user's decision. Without `request_id` it answers the only pending
    /// question and refuses when several are pending. Only a `pending`
    /// question is answered.
    ///
    /// The answer goes out through an outbox: its decision is stored as
    /// `answer_intent` (the question is then `answering`), then the
    /// control_response is written to the worker, then `answer_sent` settles
    /// the question as answered. A failed write records `answer_failed` and
    /// makes the question pending again, so it can be answered again.
    pub(crate) fn answer(&self, params: &WorkerAnswerParams) -> Result<WorkerInfo, WorkerError> {
        let answered = self.answer_for_run(params)?;
        // A todo run that answers its worker's questions itself goes on
        // once the user answered one it escalated (a run's own escalation
        // wakes its driver as it is answered).
        if !params
            .request_id
            .as_deref()
            .is_some_and(runs::escalations::is_escalation)
        {
            self.question_settled(&params.worker_id);
        }
        Ok(answered)
    }

    /// [`Self::answer`] without waking the todo run of the worker: for the
    /// run's own `todo.resume`, which records the run going on itself.
    pub(crate) fn answer_for_run(
        &self,
        params: &WorkerAnswerParams,
    ) -> Result<WorkerInfo, WorkerError> {
        if params
            .request_id
            .as_deref()
            .is_some_and(runs::pending_usage::is_question)
        {
            return Err(WorkerError::Invalid("cancel usage waits from the user's client or todo cancel-pending as the run's owner".into()));
        }
        // A grant question is answered only from the user's client
        // ([`Self::answer_from_client`]), never through the API.
        if let Some(request_id) = params
            .request_id
            .as_deref()
            .filter(|request_id| grant_questions::is_grant_question(request_id))
        {
            return Err(grant_questions::needs_user(request_id));
        }
        if let Some(request_id) = params
            .request_id
            .as_deref()
            .filter(|request_id| supervision::is_finding_question(request_id))
        {
            // Only the user's client or the herdr coordinator acknowledges
            // a finding ([`Self::answer_from_client`],
            // `supervise.acknowledge`).
            return self.refuse_finding_answer(&params.worker_id, request_id);
        }
        if let Some(request_id) = params
            .request_id
            .as_deref()
            .filter(|request_id| runs::escalations::is_escalation(request_id))
        {
            self.answer_escalation(
                &params.worker_id,
                request_id,
                params.decision,
                &params.answers,
                params.message.as_deref(),
            )?;
            return self.escalation_reply(&params.worker_id);
        }
        self.command(
            params.command_id.as_deref(),
            "worker.answer",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.answer_once(params, None, receipt),
        )
    }

    /// [`Self::answer`] sent by tenure `params.coordinator_id`
    /// (`worker.answer_as`): applied only while that tenure owns the worker.
    /// A worker another tenure owns now refuses it with
    /// `ownership_transferred`, naming that tenure, and keeps it as a
    /// `late_answer` event, which reaches the owner's inbox; it is never
    /// applied. The ownership check and the record (of the answer or of the
    /// late answer) share one hold of the registry lock, as a handoff's move
    /// does, so an answer lands either before the move or after it. A review
    /// escalation is the user's question, not the worker's owner's: it is
    /// answered as `worker.answer` answers it.
    pub(crate) fn answer_as(
        &self,
        params: &WorkerAnswerAsParams,
    ) -> Result<WorkerInfo, WorkerError> {
        let answer = &params.answer;
        if answer.request_id.as_deref().is_some_and(|request_id| {
            runs::escalations::is_escalation(request_id)
                || grant_questions::is_grant_question(request_id)
                || supervision::is_finding_question(request_id)
        }) {
            return self.answer(answer);
        }
        let answered = self.command(
            answer.command_id.as_deref(),
            "worker.answer_as",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.answer_once(answer, Some(&params.coordinator_id), receipt),
        )?;
        self.question_settled(&answer.worker_id);
        Ok(answered)
    }

    /// `answering`: the tenure that sends the answer, which must own the
    /// worker ([`Self::answer_as`]).
    pub(in crate::workers) fn answer_once(
        &self,
        params: &WorkerAnswerParams,
        answering: Option<&str>,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        let worker_id = params.worker_id.as_str();
        let (number, live, request_id, response) = {
            let mut registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            let owner = entry.status.owner_coordinator.clone();
            if let (Some(from), Some(owner)) = (answering, owner) {
                if owner != from {
                    return Err(self.refuse_late_answer(
                        &mut registry,
                        number,
                        params,
                        from,
                        &owner,
                        receipt,
                    ));
                }
            }
            let entry = registry
                .workers
                .get_mut(&number)
                .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))?;
            if let Some(request_id) = params.request_id.as_deref() {
                let open = entry
                    .status
                    .questions
                    .iter()
                    .find(|pending| pending.question.request_id == request_id);
                match (open, entry.status.resolution(request_id)) {
                    (Some(pending), _) if pending.answering => {
                        return Err(WorkerError::QuestionGone(format!(
                            "question {request_id} of worker {worker_id} is no longer pending: \
                             its answer is being sent"
                        )))
                    }
                    (None, Some(how)) => {
                        return Err(WorkerError::QuestionGone(format!(
                        "question {request_id} of worker {worker_id} is no longer pending: {how}"
                    )))
                    }
                    _ => {}
                }
            }
            let live = match (&entry.live, entry.status.is_gone()) {
                (Some(live), false) => Arc::clone(live),
                _ => {
                    return Err(WorkerError::NotRunning(format!(
                        "worker {worker_id} is not running"
                    )))
                }
            };
            if entry.status.takeover_ms.is_some() {
                return Err(Self::taken_over(worker_id));
            }
            let questions: Vec<&Pending> = entry
                .status
                .questions
                .iter()
                .filter(|pending| !pending.answering)
                .collect();
            let pending = match params.request_id.as_deref() {
                Some(request_id) => questions
                    .iter()
                    .find(|pending| pending.question.request_id == request_id),
                None if questions.len() > 1 => {
                    let ids: Vec<&str> = questions
                        .iter()
                        .map(|pending| pending.question.request_id.as_str())
                        .collect();
                    return Err(WorkerError::Invalid(format!(
                        "worker {worker_id} has {} pending questions; name one with request_id: {}",
                        ids.len(),
                        ids.join(", ")
                    )));
                }
                None => questions.first(),
            }
            .ok_or_else(|| {
                WorkerError::NoQuestion(format!("worker {worker_id} has no such pending question"))
            })?;
            let (response, answers) = answer_response(pending, params)?;
            let request_id = pending.question.request_id.clone();
            let intent = json!({
                "type": "answer_intent",
                "request_id": request_id,
                "tool_name": pending.question.tool_name,
                "decision": response["behavior"],
                "answers": answers,
                "by": "user",
                // What a server re-attaching after a crash sends again.
                "response": response,
            });
            // Stored under the lock, so a second answer finds the question
            // answering and is refused.
            self.commit_command_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&intent),
                receipt,
            );
            (number, live, request_id, response)
        };
        self.shared.changed.notify_all();
        notify_clients();
        match live.send(self, &control_response(&request_id, response)) {
            Ok(_) => self.record(
                number,
                Direction::Herdr,
                &json!({"type": "answer_sent", "request_id": request_id}),
            ),
            Err(error) => {
                self.record(
                    number,
                    Direction::Herdr,
                    &json!({
                        "type": "answer_failed",
                        "request_id": request_id,
                        "error": error.to_string(),
                    }),
                );
                return Err(error);
            }
        }
        self.status(worker_id)
    }

    /// Keeps the answer tenure `from` sent to worker `number`, which tenure
    /// `owner` owns now, as a `late_answer` event (with the command's
    /// receipt, so a repeated command id replays the refusal and keeps it
    /// once), and returns the refusal. Under the registry lock.
    pub(in crate::workers) fn refuse_late_answer(
        &self,
        registry: &mut Registry,
        number: u64,
        params: &WorkerAnswerParams,
        from: &str,
        owner: &str,
        receipt: Option<&Receipt>,
    ) -> WorkerError {
        let late = json!({
            "type": "late_answer",
            "from_coordinator_id": from,
            "to_coordinator_id": owner,
            "request_id": params.request_id,
            "decision": params.decision,
            "answers": params.answers,
            "message": params.message,
        });
        self.commit_command_locked(
            registry,
            number,
            Direction::Herdr,
            store::Recorded::Event(&late),
            receipt,
        );
        self.shared.changed.notify_all();
        WorkerError::OwnershipTransferred(format!(
            "worker {} belongs to coordinator {owner} now, not to {from}: the answer was not \
             applied; it is passed to {owner} as information",
            params.worker_id
        ))
    }
}

pub(in crate::workers) fn control_response(request_id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
}

/// The `can_use_tool` answer for the user's decision, and the answers it
/// carries (for the journal). An `AskUserQuestion` is allowed with
/// `updatedInput.answers`, a map from each question to its answer (trial 1,
/// case 3).
pub(in crate::workers) fn answer_response(
    pending: &Pending,
    params: &WorkerAnswerParams,
) -> Result<(Value, Value), WorkerError> {
    let question = &pending.question;
    let deny = |default: &str| {
        let message = params.message.as_deref().unwrap_or(default);
        Ok((json!({"behavior": "deny", "message": message}), Value::Null))
    };
    match question.kind {
        WorkerQuestionKind::Choice => {
            if params.decision == Some(WorkerDecision::Deny) {
                return deny("The user declined to answer.");
            }
            if params.answers.len() != question.questions.len() {
                return Err(WorkerError::Invalid(format!(
                    "this question needs {} answer(s), got {}",
                    question.questions.len(),
                    params.answers.len()
                )));
            }
            let mut answers = serde_json::Map::new();
            for (choice, raw) in question.questions.iter().zip(&params.answers) {
                let answer = resolve_choice(choice, raw).ok_or_else(|| {
                    WorkerError::Invalid(format!("an empty answer to: {}", choice.question))
                })?;
                answers.insert(choice.question.clone(), Value::String(answer));
            }
            let mut input = pending.input.clone();
            if let Some(object) = input.as_object_mut() {
                object.insert("answers".into(), Value::Object(answers.clone()));
            }
            Ok((
                json!({"behavior": "allow", "updatedInput": input}),
                Value::Object(answers),
            ))
        }
        WorkerQuestionKind::Approval | WorkerQuestionKind::Unknown => {
            if !params.answers.is_empty() {
                return Err(WorkerError::Invalid(
                    "an approval takes allow or deny, not answers".into(),
                ));
            }
            match params.decision {
                Some(WorkerDecision::Allow) => Ok((
                    json!({"behavior": "allow", "updatedInput": pending.input}),
                    Value::Null,
                )),
                Some(WorkerDecision::Deny) => deny("The user denied this tool use."),
                None => Err(WorkerError::Invalid(
                    "an approval needs the decision allow or deny".into(),
                )),
            }
        }
    }
}

/// One answer: an option's 1-based number or label (any case) becomes the
/// label, anything else is the user's own text. A multi-select answer is a
/// comma-separated list of those, joined with `, `.
pub(in crate::workers) fn resolve_choice(
    choice: &WorkerChoiceQuestion,
    raw: &str,
) -> Option<String> {
    let resolve = |part: &str| {
        let part = part.trim();
        part.parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| choice.options.get(index))
            .or_else(|| {
                choice
                    .options
                    .iter()
                    .find(|label| label.eq_ignore_ascii_case(part))
            })
            .cloned()
            .unwrap_or_else(|| part.to_owned())
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if !choice.multi_select || choice.options.iter().any(|label| label == raw) {
        return Some(resolve(raw));
    }
    let parts: Vec<String> = raw
        .split(',')
        .map(resolve)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The question a `can_use_tool` request the policy left to the user asks.
pub(in crate::workers) fn question_from_request(
    request_id: &str,
    request: &Value,
    reason: &str,
) -> WorkerQuestion {
    let tool_name = request["tool_name"].as_str().unwrap_or("").to_owned();
    let input = &request["input"];
    let questions: Vec<WorkerChoiceQuestion> = if tool_name == "AskUserQuestion" {
        input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|question| WorkerChoiceQuestion {
                question: question["question"].as_str().unwrap_or("").to_owned(),
                header: string_field(question, "header"),
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| option["label"].as_str().map(str::to_owned))
                    .collect(),
                multi_select: question["multiSelect"].as_bool().unwrap_or(false),
            })
            .collect()
    } else {
        Vec::new()
    };
    let text = if tool_name == "AskUserQuestion" {
        questions
            .iter()
            .map(|question| format!("{} [{}]", question.question, question.options.join(" | ")))
            .collect::<Vec<_>>()
            .join(" / ")
    } else if let Some(command) = input["command"].as_str() {
        command.to_owned()
    } else if let Some(path) = ["file_path", "notebook_path", "path", "url"]
        .iter()
        .find_map(|key| input[*key].as_str())
    {
        path.to_owned()
    } else {
        input.to_string()
    };
    WorkerQuestion {
        request_id: request_id.to_owned(),
        kind: if tool_name == "AskUserQuestion" {
            WorkerQuestionKind::Choice
        } else {
            WorkerQuestionKind::Approval
        },
        tool_name,
        text: one_line(&text, 300),
        reason: Some(reason.to_owned()),
        questions,
        since_ms: now_ms(),
        state: WorkerQuestionState::Pending,
        escalated: None,
    }
}

/// A tool's whole input for the user to decide on: a `command` as written,
/// newlines kept, then the input's other fields one per line (a sandbox or
/// background flag changes what allowing means); any other input as
/// indented JSON. Never cut: an approval must not rest on a preview.
pub(in crate::workers) fn full_input_text(input: &Value) -> String {
    let Some(command) = input["command"].as_str() else {
        return serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    };
    let mut text = command.to_owned();
    let others: Vec<String> = input
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.as_str() != "command")
        .map(|(key, value)| match value.as_str() {
            Some(value) => format!("{key}: {value}"),
            None => format!("{key}: {value}"),
        })
        .collect();
    if !others.is_empty() {
        text.push_str("\n\n");
        text.push_str(&others.join("\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_input_keeps_a_commands_newlines_and_its_other_fields() {
        let input = serde_json::json!({
            "command": "git add -A\ngit commit -m 'x'",
            "dangerouslyDisableSandbox": true,
            "description": "Commit",
        });
        assert_eq!(
            full_input_text(&input),
            "git add -A\ngit commit -m 'x'\n\ndangerouslyDisableSandbox: true\ndescription: Commit"
        );
        let long = "x".repeat(1_000);
        let input = serde_json::json!({"file_path": "/tmp/a", "content": long});
        let text = full_input_text(&input);
        assert!(text.contains(&long), "never cut");
        assert!(text.contains("\"file_path\": \"/tmp/a\""), "{text}");
    }
}
