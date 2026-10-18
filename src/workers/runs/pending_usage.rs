//! Durable wait-capable starts use ordinary run ids and `todo.wait`.
//! Only measurement events wake them; a fresh gate read is required at dispatch.
use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::json;

use super::{activity, lock, new_event, usage_gate, Run, RUN_ENV};
use crate::api::schema::{
    ProviderUsage, TodoEventKind, TodoRunInfo, TodoRunStatus, TodoStep, WorkerChoiceQuestion,
    WorkerQuestion, WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionState, WorkerState,
};
use crate::workers::{now_ms, PendingWorkerQuestion, WorkerError, WorkerSupervisor};

const PREFIX: &str = "herdr-usage-";
// Presentation is published on writes, never read from disk by a render loop.
static LISTED: Mutex<BTreeMap<String, Run>> = Mutex::new(BTreeMap::new());

#[cfg(all(test, unix))]
static CANCEL_BEFORE_DISPATCH: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(all(test, unix))]
pub(in crate::workers) fn cancel_before_dispatch_for_test(run_id: &str) {
    lock(&CANCEL_BEFORE_DISPATCH).push(run_id.into());
}

pub(super) fn observe(run: &Run) {
    let mut listed = lock(&LISTED);
    if run.finish.pending_usage {
        listed.insert(run.info.run_id.clone(), run.clone());
    } else {
        listed.remove(&run.info.run_id);
    }
}

fn question(run: &Run) -> WorkerQuestion {
    WorkerQuestion {
        request_id: format!("{PREFIX}{}", run.info.run_id),
        kind: WorkerQuestionKind::Choice,
        tool_name: "usage gate".into(),
        text: run
            .info
            .error
            .clone()
            .unwrap_or_else(|| "waiting for usage measurement".into()),
        reason: Some(format!(
            "item {}; cancel with `herdr todo cancel-pending {}`",
            run.info.item, run.info.run_id
        )),
        questions: vec![WorkerChoiceQuestion {
            question: "The start will resume after a successful measurement. Cancel it instead?"
                .into(),
            header: Some("Usage gate".into()),
            options: vec!["Cancel pending start".into()],
            multi_select: false,
        }],
        since_ms: run.info.created_ms,
        state: WorkerQuestionState::Pending,
        escalated: Some("Waiting for a successful measurement; no worker has started".into()),
    }
}

pub(in crate::workers) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    lock(&LISTED)
        .values()
        .map(|run| PendingWorkerQuestion {
            worker_id: run.info.run_id.clone(),
            cwd: run.info.repo.clone(),
            question: question(run),
            quiet: false,
            owner_seen_ms: None,
        })
        .collect()
}

pub(in crate::workers) fn is_question(request_id: &str) -> bool {
    request_id.starts_with(PREFIX)
}

pub(in crate::workers) fn detail(
    worker_id: &str,
    request_id: &str,
) -> Option<Result<WorkerQuestionDetail, WorkerError>> {
    if !request_id.starts_with(PREFIX) {
        return None;
    }
    Some(
        lock(&LISTED)
            .get(worker_id)
            .filter(|run| request_id == format!("{PREFIX}{}", run.info.run_id))
            .map(|run| WorkerQuestionDetail {
                worker_id: worker_id.into(),
                name: format!("pending start of {}", run.info.item),
                cwd: run.info.repo.clone(),
                state: WorkerState::WaitingApproval,
                question: question(run),
                input_text: question(run).text,
                owner_pane_id: run.owner_pane.clone(),
                owner_coordinator_id: run.owner_coordinator.clone(),
                quiet: false,
            })
            .ok_or_else(|| {
                WorkerError::QuestionGone("pending start no longer waits for usage".into())
            }),
    )
}

impl WorkerSupervisor {
    pub(in crate::workers) fn dispatch_pending_usage(&self, run: &mut Run) -> Result<(), String> {
        let mut preflight_cause = None;
        match self.usage_gate(
            &run.info.repo,
            run.finish.agent.unwrap_or_default(),
            run.finish.worker_model(),
            run.finish.ignore_usage,
            Some(&usage_gate::Holder::Run {
                run_id: run.info.run_id.clone(),
                attempt: run.info.attempt,
            }),
        ) {
            Ok(_) => {
                // A gate that closed after drafting must not draft a second task.
                let needs_draft = run.finish.draft && run.info.step == TodoStep::Preflight;
                run.finish.pending_usage = false;
                match self.refresh_preflight(run, needs_draft) {
                    Ok(()) => {
                        run.info.status = TodoRunStatus::Running;
                        run.info.step = if needs_draft {
                            TodoStep::Draft
                        } else {
                            TodoStep::Start
                        };
                        run.info.error = None;
                    }
                    Err(error) => {
                        preflight_cause = Some(error.cause);
                        run.info.status = TodoRunStatus::Blocked;
                        run.info.error = Some(error.to_string());
                    }
                }
            }
            Err(WorkerError::UsageGate(why)) => run.info.error = Some(why),
            Err(error) => return Err(error.to_string()),
        }
        #[cfg(all(test, unix))]
        {
            let cancel = {
                let mut ids = lock(&CANCEL_BEFORE_DISPATCH);
                ids.iter()
                    .position(|id| id == &run.info.run_id)
                    .map(|at| ids.remove(at))
                    .is_some()
            };
            if cancel {
                self.todo_cancel_pending(&run.info.run_id)
                    .map_err(|error| error.to_string())?;
            }
        }
        let store = self.run_store().map_err(|error| error.to_string())?;
        let current = store
            .transaction(|tx| {
                let Some(current) = tx.run(&run.info.run_id)? else {
                    return Ok(None);
                };
                if !current.finish.pending_usage {
                    // Cancellation won while the provider was being read.
                    return Ok(Some(current));
                }
                if current.finish.queued
                    && tx
                        .queue(&run.info.repo)?
                        .is_none_or(|queue| queue.mode != "on")
                {
                    run.finish.pending_usage = true;
                    run.info.status = TodoRunStatus::Waiting;
                    run.info.error =
                        Some("queue paused; run `herdr todo queue on` to resume".into());
                }
                let event = if run.info.status == TodoRunStatus::Blocked {
                    let mut event = new_event(TodoEventKind::Blocked);
                    event.error = run.info.error.clone();
                    event.preflight_cause = preflight_cause;
                    json!({"type": "run_event", "kind": event.kind, "event": event})
                } else {
                    json!({"type": "run_usage_measurement"})
                };
                tx.run_event(run, &event, false, now_ms())?;
                Ok(Some(run.clone()))
            })
            .map_err(|error| error.to_string())?;
        if let Some(current) = current {
            *run = current;
        }
        observe(run);
        activity::observe(&run.info, None, now_ms());
        super::announce();
        Ok(())
    }

    pub(crate) fn todo_cancel_pending_owned(
        &self,
        params: &crate::api::schema::TodoCancelPendingParams,
    ) -> Result<TodoRunInfo, WorkerError> {
        let run = self.load_run(&params.run_id)?;
        let tenure = params
            .caller_pane_id
            .as_deref()
            .and_then(|pane| self.coordinator_of_pane(pane));
        self.run_takeover(
            &run,
            params.caller_pane_id.as_deref(),
            tenure.as_ref().map(|tenure| tenure.coordinator_id.as_str()),
        )?;
        self.todo_cancel_pending(&params.run_id)
    }

    pub(crate) fn cancel_usage_question_from_client(
        &self,
        params: &crate::api::schema::WorkerAnswerParams,
    ) -> Result<crate::api::schema::WorkerInfo, WorkerError> {
        if params.decision == Some(crate::api::schema::WorkerDecision::Deny)
            || params.answers.as_slice() == ["Cancel pending start"]
            || params.answers.as_slice() == ["1"]
        {
            self.todo_cancel_pending(&params.worker_id)?;
            return self.escalation_reply(&params.worker_id);
        }
        Err(WorkerError::Invalid("usage waits are opened by measurements, not approval; choose Cancel pending start to cancel".into()))
    }

    /// The store transaction is the linearization point for cancel versus dispatch.
    /// It never waits for a provider read or an already started worker.
    pub(crate) fn todo_cancel_pending(&self, run_id: &str) -> Result<TodoRunInfo, WorkerError> {
        let store = self.run_store()?;
        let canceled = store
            .transaction(|tx| {
                let Some(mut run) = tx.run(run_id)? else {
                    return Ok(None);
                };
                if run.info.status == TodoRunStatus::Aborted && run.finish.usage_dispatch {
                    return Ok(Some(run));
                }
                if !run.finish.pending_usage {
                    return Ok(None);
                }
                run.finish.pending_usage = false;
                run.info.status = TodoRunStatus::Aborted;
                run.info.error = Some("pending start canceled".into());
                if tx
                    .queue(&run.info.repo)?
                    .is_some_and(|queue| queue.mode == "on")
                {
                    tx.queue_paused(
                        &run.info.repo,
                        "pending start canceled; run `herdr todo queue on` to resume",
                        now_ms(),
                    )?;
                }
                let mut event = new_event(TodoEventKind::Aborted);
                event.error = run.info.error.clone();
                tx.run_event(
                    &mut run,
                    &json!({"type": "run_event", "kind": event.kind, "event": event}),
                    false,
                    now_ms(),
                )?;
                Ok(Some(run))
            })
            .map_err(super::store_error)?;
        let run = canceled.ok_or_else(|| WorkerError::Invalid("pending start not found or already dispatched; use todo resume to abort its worker".into()))?;
        observe(&run);
        activity::observe(&run.info, None, now_ms());
        super::announce();
        lock(&RUN_ENV).remove(run_id);
        self.queue_after_run(&run.info.repo);
        Ok(run.info)
    }

    pub(in crate::workers) fn pending_after_measurement(&self) {
        if let Ok(store) = self.run_store() {
            if let Ok(runs) = store.runs_in_progress() {
                for run in runs.into_iter().filter(|run| run.finish.pending_usage) {
                    observe(&run);
                    activity::observe(&run.info, None, now_ms());
                    if !run.finish.queued || self.queue_is_on(&run.info.repo) {
                        self.spawn_driver(&run.info.run_id);
                    }
                }
            }
        }
    }

    /// Records an actual provider measurement, not a report assembled from cached readings.
    /// Caller serializes this with start-time reads using the provider's measurement lock.
    pub(in crate::workers) fn record_usage_measurement(
        &self,
        provider_id: &str,
        answer: Result<&ProviderUsage, &str>,
        skip: Option<&str>,
    ) -> Result<(), WorkerError> {
        let store = self.run_store()?;
        let thresholds = self.usage_gate_thresholds();
        for key in store.usage_gate_keys().map_err(super::store_error)? {
            if skip == Some(key.as_str()) {
                continue;
            }
            let Some(provider) = usage_gate::GateProvider::of_store_key(&key) else {
                continue;
            };
            if provider.id() != provider_id {
                continue;
            }
            let previous = store.usage_gate_body(&key).map_err(super::store_error)?;
            let state = usage_gate::State::from_record(
                store.usage_gate_closed(&key).map_err(super::store_error)?,
                &previous,
            );
            let decision = usage_gate::decide(
                provider,
                thresholds,
                answer,
                state == usage_gate::State::High,
            );
            let next = state.after(&decision);
            let mut recorded = usage_gate::decision_json(&decision, false);
            recorded["provider"] = json!(provider_id);
            recorded["state"] = json!(next.id());
            recorded["last_good"] = match answer {
                Ok(usage) if !usage.windows.is_empty() || !usage.balances.is_empty() => {
                    json!(usage)
                }
                _ => previous["last_good"].clone(),
            };
            store
                .set_usage_gate(&key, next != usage_gate::State::Open, &recorded, now_ms())
                .map_err(super::store_error)?;
        }
        Ok(())
    }

    pub(crate) fn pending_after_usage_measurement(
        &self,
        provider_id: &str,
        answer: Result<&ProviderUsage, &str>,
    ) {
        let check = usage_gate::measurement_lock(provider_id);
        if let Err(error) = self.record_usage_measurement(provider_id, answer, None) {
            tracing::warn!(%error, "cannot record usage measurement");
            return;
        }
        drop(check);
        if let Ok(usage) = answer {
            self.wake_pending_after_success(provider_id, usage);
        }
    }

    /// A worker's exit or loss, once stored, freed the slot the usage
    /// gate's cap counted for it: the starts waiting on the gate whose
    /// provider's latch is open (refused for the cap, not for usage, which
    /// a measurement reopens) dispatch again, each reading usage and
    /// counting anew. Runs in a thread of its own, since the exit is
    /// stored under the registry's lock.
    pub(in crate::workers) fn wake_pending_after_release(&self) {
        let supervisor = self.clone();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-gate-release", move || {
            supervisor.wake_pending_for_open_gates();
        }) {
            tracing::warn!(%error, "cannot wake the starts waiting on the usage gate");
        }
    }

    pub(in crate::workers) fn wake_pending_for_open_gates(&self) {
        let Ok(store) = self.run_store() else {
            return;
        };
        let open = |provider: usage_gate::GateProvider| {
            store
                .usage_gate_closed(&provider.store_key())
                .is_ok_and(|closed| !closed)
        };
        if open(usage_gate::GateProvider::Claude) {
            if let Ok(queues) = store.queues() {
                for queue in queues.into_iter().filter(|queue| {
                    queue.mode == "on" && queue.status.as_deref() == Some("usage_gate")
                }) {
                    self.queue_event(&queue.repo);
                }
            }
        }
        let candidates = lock(&LISTED).values().cloned().collect::<Vec<_>>();
        for run in candidates {
            if run.finish.queued && !self.queue_is_on(&run.info.repo) {
                continue;
            }
            if open(Self::run_gate_provider(&run)) {
                self.spawn_driver(&run.info.run_id);
            }
        }
    }

    /// The same held rows the cap counts, across all providers in a git
    /// repository. Pending admissions are visible before a worker exists.
    /// The repository key is [`super::repo_identity`], the gate's
    /// ([`Self::usage_gate`] admits under the same), so the list cannot miss
    /// or mix one repository's rows whether it is asked from a worktree, a
    /// subdirectory or outside git.
    pub(crate) fn admissions(
        &self,
        cwd: &str,
    ) -> Result<Vec<crate::api::schema::WorkerAdmission>, WorkerError> {
        let repo = super::repo_identity(std::path::Path::new(cwd))?;
        self.run_store()?
            .repository_admissions(&repo)
            .map_err(super::store_error)
            .map(|rows| {
                rows.into_iter()
                    .map(|row| crate::api::schema::WorkerAdmission {
                        bound: row.worker_id.is_some(),
                        holder: row.holder,
                        provider: row.provider,
                        repo: row.repo,
                        run_id: row.run_id,
                        attempt: row.attempt,
                        worker_id: row.worker_id,
                        admitted_ms: row.admitted_ms,
                        server_pid: row.server_pid,
                        server_start: row.server_start,
                    })
                    .collect()
            })
    }

    /// At a server's start: ends the usage gate's admissions whose start no
    /// row records and that no live server still makes
    /// ([`Self::reconcile_handoff_admissions`]). A worker that survived the
    /// restart keeps its slot, since its row shows it running; a lost worker
    /// also keeps its slot while its process lives.
    pub(in crate::workers) fn reconcile_admissions(&self) {
        self.reconcile_handoff_admissions(None);
    }

    /// Ends the usage gate's admissions whose worker row the store does not
    /// hold and whose admitting server provably cannot still make them: a
    /// `worker.run` admitted by a server process that is gone, and one bound
    /// to a worker the store does not hold when the server that bound it is
    /// gone (it ended between naming the worker and recording its start).
    ///
    /// The evidence is the identity recorded with the row
    /// (`server_pid`/`server_start`), never a clock: the row keeps its slot
    /// while that process still runs with that start token, since it may be
    /// between binding the worker and writing its row, and is retired once
    /// the process is gone — or, for the source of a committed handoff
    /// (`retiring`), at the import, even while it finishes shutting down. A
    /// bound row of that source is kept until it exits, because it may be
    /// spawning a worker right then; the exit reconcile
    /// ([`crate::workers::reconcile_admissions_after_handoff`]) hands the
    /// same identity again, which retires what the source admitted after the
    /// import.
    ///
    /// Two rows carry no usable identity: one from a server that recorded
    /// none, and one from a journal that predates it. A bound one is judged
    /// by its journal's ownership lock alone (that lock cannot tell a spawn
    /// in flight from one a dead server left), and an unbound one is
    /// retired. A handoff whose manifest names no source identity falls back
    /// to the pid its socket's peer names
    /// ([`crate::server::handoff::admission_server`]); a source whose start
    /// token cannot be read at all leaves its bound rows to the next server
    /// start, which retires them then.
    pub(in crate::workers) fn reconcile_handoff_admissions(
        &self,
        retiring: Option<(u32, Option<u64>)>,
    ) {
        let Ok(store) = self.run_store() else {
            return;
        };
        let admissions = match store.unrecorded_admissions() {
            Ok(admissions) => admissions,
            Err(error) => {
                tracing::warn!(%error, "cannot read the usage gate's admissions");
                return;
            }
        };
        let mut released = false;
        for admission in admissions {
            let named_worker = admission.worker_id.is_some();
            // The source of a committed handoff: its starts that name no
            // worker can no longer become a worker of this session.
            let source = retiring.is_some_and(|(pid, start)| {
                admission.server_pid == Some(pid) && admission.server_start == start
            });
            let alive = match (admission.server_pid, admission.server_start) {
                (Some(pid), Some(start)) => {
                    crate::platform::process_start_token(pid) == Some(start)
                }
                // A row from before the admitting server was recorded: only
                // the worker's journal names the server holding it.
                _ => match &admission.worker_id {
                    Some(worker_id) => matches!(
                        crate::workers::journal::journal_ownership(&self.journal_path(worker_id)),
                        Ok(crate::workers::journal::Ownership::Owned(_))
                    ),
                    None => false,
                },
            };
            // A start that names no worker is retired for a handoff's
            // source whatever its process does; any other row keeps its
            // slot while the server that admitted it may still make it.
            let keep = if source && !named_worker {
                false
            } else {
                alive
            };
            if keep {
                continue;
            }
            let why = match (named_worker, source) {
                (true, true) => {
                    "its server was retired by a handoff before the worker was recorded"
                }
                (true, false) => "its server ended before the worker was recorded",
                (false, true) => {
                    "its server was retired by a handoff before the worker was started"
                }
                (false, false) => "its server ended before the worker was started",
            };
            match store.retire_unrecorded_admission(&admission.holder, why, now_ms()) {
                Ok(ended) => released |= ended,
                Err(error) => {
                    tracing::warn!(%error, holder = admission.holder, "cannot end an admission")
                }
            }
        }
        if released {
            self.wake_pending_after_release();
        }
    }

    /// A successful direct read is a wake hint, never permission to start.
    /// Only hints that could open a gate wake it, avoiding self-wake loops on high readings.
    pub(super) fn wake_pending_after_success(&self, provider_id: &str, usage: &ProviderUsage) {
        let thresholds = self.usage_gate_thresholds();
        let Ok(store) = self.run_store() else {
            return;
        };
        if provider_id == "claude" {
            if let Ok(queues) = store.queues() {
                for queue in queues.into_iter().filter(|queue| {
                    queue.mode == "on" && queue.status.as_deref() == Some("usage_gate")
                }) {
                    if store
                        .usage_gate_closed(&usage_gate::GateProvider::Claude.store_key())
                        .is_ok_and(|closed| !closed)
                    {
                        self.queue_event(&queue.repo);
                    }
                }
            }
        }
        let candidates = lock(&LISTED).values().cloned().collect::<Vec<_>>();
        for run in candidates {
            if run.finish.queued && !self.queue_is_on(&run.info.repo) {
                continue;
            }
            let provider = Self::run_gate_provider(&run);
            if provider.id() != provider_id {
                continue;
            }
            let key = provider.store_key();
            let state = match (store.usage_gate_closed(&key), store.usage_gate_body(&key)) {
                (Ok(closed), Ok(body)) => usage_gate::State::from_record(closed, &body),
                _ => continue,
            };
            if matches!(
                usage_gate::decide(
                    provider,
                    thresholds,
                    Ok(usage),
                    state == usage_gate::State::High
                ),
                usage_gate::Decision::Admit { .. }
            ) {
                self.spawn_driver(&run.info.run_id);
            }
        }
    }
}
