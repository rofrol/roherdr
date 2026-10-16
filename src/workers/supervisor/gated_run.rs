//! `worker.run`: a gated, blocking headless run for an orchestrator outside
//! herdr's TODO driver. It passes the same usage gate as `todo.run`
//! ([`WorkerSupervisor::usage_gate`], one gate, never a second), starts the
//! worker as `worker.start` does without a folder slot, runs one turn,
//! stops the worker and answers with the turn's text, usage and outcome.
//! While the repository has an active coordination tenure, only a run with
//! that tenure's current token is admitted
//! ([`WorkerSupervisor::check_tenure_token`]); with none active, any is.

use super::super::*;
use crate::api::schema::{
    WorkerGatedRun, WorkerGatedRunOutcome, WorkerGatedRunParams, WorkerModelUsage, WorkerQuestion,
    WorkerTurnResult,
};

/// What ended the run's turn, read from the worker's status.
struct TurnEnd {
    state: WorkerState,
    last_result: Option<WorkerTurnResult>,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    question: Option<WorkerQuestion>,
}

impl WorkerSupervisor {
    /// Runs `params` to its end: refused as `usage_gate` when the gate is
    /// closed (nothing starts), as `invalid` for a directory `worker.start`
    /// refuses, one outside a git repository or inside one of herdr's folder
    /// slots. Blocks until the turn ends, the worker asks a question its
    /// policy leaves to the user, or the worker's process ends, all woken by
    /// the worker's events; then stops the worker and waits for its exit.
    /// `keep_waiting` runs at least every `liveness_check`, only so a caller
    /// whose client went away or whose server stops can give up: the worker
    /// is then killed and `None` returned.
    pub(crate) fn gated_run(
        &self,
        params: &WorkerGatedRunParams,
        liveness_check: Duration,
        mut keep_waiting: impl FnMut() -> bool,
    ) -> Result<Option<WorkerGatedRun>, WorkerError> {
        let agent = params.agent.unwrap_or_default();
        let repo = self.gated_run_repository(params)?;
        // The repository's tenure, when one is active, admits only its
        // holder's current token: a refusal starts nothing.
        self.check_tenure_token(&repo, params.tenure_token.as_deref())?;
        // The gate as `todo.run` passes it: the same state, the same
        // recording; a refusal starts nothing.
        let mut usage_gate = self.usage_gate(&repo, agent, params.ignore_usage)?;
        usage_gate["ignore_usage"] = json!(params.ignore_usage);
        let start = WorkerStartParams {
            cwd: params.cwd.clone(),
            prompt: params.prompt.clone(),
            model: params.model.clone(),
            agent: Some(agent),
            name: params.name.clone(),
            workspace_id: params.workspace_id.clone(),
            folder_slot: None,
            branch: None,
            base: None,
            fresh_build: false,
            owner_pane_id: None,
            owner_session_id: None,
            item: params.item.clone(),
            command_id: None,
        };
        let worker = self.start_once(&start, None, None)?;
        let worker_id = worker.worker_id;
        if let Some(number) = worker_number(&worker_id) {
            let mut event = json!({"type": "usage_gate", "method": "worker.run", "repo": repo});
            if let (Some(event), Value::Object(fields)) = (event.as_object_mut(), &usage_gate) {
                event.extend(fields.clone());
            }
            self.record(number, Direction::Herdr, &event);
        }
        let ended = self.wait_on(&worker_id, liveness_check, &mut keep_waiting, |entry| {
            let status = &entry.status;
            let question = status
                .questions
                .iter()
                .find(|pending| !pending.answering)
                .map(Pending::shown);
            (question.is_some() || status.turn_ended()).then(|| TurnEnd {
                state: status.state,
                last_result: status.last_result.clone(),
                exit_code: status.exit_code,
                exit_signal: status.exit_signal,
                question,
            })
        });
        let ended = match ended {
            Ok(Some(ended)) => ended,
            Ok(None) => {
                self.kill_gated_run(&worker_id);
                return Ok(None);
            }
            Err(error) => {
                self.kill_gated_run(&worker_id);
                return Err(error);
            }
        };
        // Stopped whatever ended the turn, so no process of the worker is
        // left in the caller's directory once this answers.
        self.end_gated_run(&worker_id)?;
        let exited = self.wait(
            &worker_id,
            WorkerWaitUntil::Exit,
            liveness_check,
            &mut keep_waiting,
        );
        if !matches!(exited, Ok(Some(_))) {
            self.kill_gated_run(&worker_id);
            exited?;
            return Ok(None);
        }
        let (usage, total_cost_usd) = journal_usage(&self.journal_path(&worker_id));
        let (outcome, message) = outcome_of(&ended);
        Ok(Some(WorkerGatedRun {
            worker_id,
            outcome,
            message,
            text: ended
                .last_result
                .as_ref()
                .and_then(|result| result.text.clone()),
            usage,
            total_cost_usd,
            usage_gate,
            question: ended.question,
        }))
    }

    /// The repository whose usage gate admits `params`, after the checks
    /// that refuse it before the gate is read: those of `worker.start`, a
    /// directory in no git repository, and one in a folder slot of herdr's
    /// (the slot is herdr's checkout, not the caller's).
    fn gated_run_repository(&self, params: &WorkerGatedRunParams) -> Result<String, WorkerError> {
        let cwd = Path::new(&params.cwd);
        if !cwd.is_absolute() {
            return Err(WorkerError::Invalid(format!(
                "cwd must be absolute: {}",
                params.cwd
            )));
        }
        let cwd_real = cwd
            .canonicalize()
            .map_err(|error| WorkerError::Invalid(format!("cwd {}: {error}", params.cwd)))?;
        if !cwd_real.is_dir() {
            return Err(WorkerError::Invalid(format!(
                "cwd is not a directory: {}",
                params.cwd
            )));
        }
        if params.prompt.trim().is_empty() {
            return Err(WorkerError::Invalid("prompt must not be empty".into()));
        }
        if let Some(item) = &params.item {
            check_item_id(item)?;
        }
        let repo = repository_of(&cwd_real).ok_or_else(|| {
            WorkerError::Invalid(format!(
                "{} is not in a git repository; worker.run runs in a worktree the caller owns",
                params.cwd
            ))
        })?;
        home_repo::check_worker_writes(&[cwd_real.as_path()], Some(&repo), false)?;
        // A slot is refused by its path, whether or not a worker ran there:
        // every place a slot of this repository may be, under the name
        // `todo.run` uses and those the registry's workers used, and the
        // directories the registry's slot workers ran in, of any
        // repository.
        let (names, ran_in) = {
            let registry = lock(&self.shared.registry);
            let mut names = BTreeSet::from([runs::SLOT.to_owned()]);
            let mut ran_in = Vec::new();
            for status in registry.workers.values().map(|entry| &entry.status) {
                if let Some(slot) = &status.slot {
                    names.insert(slot.clone());
                    let dir = Path::new(&status.cwd);
                    ran_in.push((slot.clone(), dir.canonicalize().unwrap_or(dir.to_owned())));
                }
            }
            (names, ran_in)
        };
        let places = names.iter().flat_map(|name| {
            slot::places_of(&cwd_real, name)
                .into_iter()
                .map(move |place| (name.clone(), place))
        });
        let slot = places
            .chain(ran_in)
            .find(|(_, dir)| cwd_real.starts_with(dir));
        if let Some((name, dir)) = slot {
            return Err(WorkerError::Invalid(format!(
                "{} is in herdr's folder slot {name} ({}); worker.run runs only in a worktree \
                 the caller owns, outside herdr's folder slots",
                params.cwd,
                dir.display()
            )));
        }
        Ok(repo)
    }

    /// Ends a run whose caller gave up, or whose wait failed: the worker is
    /// killed, so none is left running for nobody.
    fn kill_gated_run(&self, worker_id: &str) {
        if let Err(error) = self.gated_run_kill(worker_id) {
            tracing::debug!(%error, worker_id, "worker.run: the worker is already gone");
        }
    }

    /// Stops the run's worker after its turn, so that its exit event comes.
    /// A stop that fails while the worker still runs is followed by a kill;
    /// when that fails too, or nothing watches the worker's process to
    /// report its exit, the caller gets the error naming the worker instead
    /// of a wait on an exit nothing causes.
    fn end_gated_run(&self, worker_id: &str) -> Result<(), WorkerError> {
        let Err(stop_error) = self.gated_run_stop(worker_id) else {
            return Ok(());
        };
        let (gone, watched) = self.with_entry(worker_id, |entry| {
            (entry.status.is_gone(), entry.live.is_some())
        })?;
        if gone {
            tracing::debug!(%stop_error, worker_id, "worker.run: the worker is already gone");
            return Ok(());
        }
        warn!(%stop_error, worker_id, "worker.run could not stop the worker; killing it");
        if let Err(kill_error) = self.gated_run_kill(worker_id) {
            return Err(WorkerError::Io(std::io::Error::other(format!(
                "worker.run could not stop worker {worker_id} ({stop_error}) nor kill it \
                 ({kill_error}); it may still run"
            ))));
        }
        if !watched {
            return Err(WorkerError::Io(std::io::Error::other(format!(
                "worker.run killed worker {worker_id} after its stop failed ({stop_error}), but \
                 nothing watches its process to report the exit"
            ))));
        }
        Ok(())
    }

    fn gated_run_stop(&self, worker_id: &str) -> Result<(), WorkerError> {
        #[cfg(all(test, unix))]
        if lock(&self.shared.gated_run_end_failure).fails_stop() {
            return Err(WorkerError::Io(std::io::Error::other(
                "the stop failed (test hook)",
            )));
        }
        self.stop(worker_id).map(drop)
    }

    fn gated_run_kill(&self, worker_id: &str) -> Result<(), WorkerError> {
        #[cfg(all(test, unix))]
        if *lock(&self.shared.gated_run_end_failure) == GatedRunEndFailure::StopAndKill {
            return Err(WorkerError::Io(std::io::Error::other(
                "the kill failed (test hook)",
            )));
        }
        self.kill_once(worker_id, false, None, None).map(drop)
    }

    /// Makes `worker.run`'s ending signals fail as `failure` says.
    #[cfg(all(test, unix))]
    pub(in crate::workers) fn set_gated_run_end_failure_for_test(
        &self,
        failure: GatedRunEndFailure,
    ) {
        *lock(&self.shared.gated_run_end_failure) = failure;
    }
}

/// Which of `worker.run`'s ending signals a test makes fail.
#[cfg(all(test, unix))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::workers) enum GatedRunEndFailure {
    #[default]
    None,
    /// The stop after the turn fails, the worker still running.
    Stop,
    /// The stop and the kill after it fail.
    StopAndKill,
}

#[cfg(all(test, unix))]
impl GatedRunEndFailure {
    fn fails_stop(self) -> bool {
        self != Self::None
    }
}

/// The outcome and its line for the caller's record.
fn outcome_of(ended: &TurnEnd) -> (WorkerGatedRunOutcome, String) {
    if let Some(question) = &ended.question {
        let reason = question
            .reason
            .as_deref()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default();
        return (
            WorkerGatedRunOutcome::PolicyRefused,
            format!(
                "the worker asked {} {}{reason}, which its policy leaves to the user; worker.run \
                 answers no question, so the worker was stopped",
                question.tool_name,
                one_line(&question.text, 200)
            ),
        );
    }
    let result = ended.last_result.as_ref();
    match ended.state {
        WorkerState::Finished => (
            WorkerGatedRunOutcome::Ended,
            "the turn ended with result/success".into(),
        ),
        WorkerState::Interrupted => (
            WorkerGatedRunOutcome::Stopped,
            format!(
                "the turn was interrupted ({})",
                result
                    .and_then(|result| result.terminal_reason.as_deref())
                    .unwrap_or("aborted")
            ),
        ),
        WorkerState::Failed => (
            WorkerGatedRunOutcome::Failed,
            match result {
                Some(result) => format!(
                    "the turn failed: {}",
                    result
                        .failure
                        .clone()
                        .or_else(|| result.terminal_reason.clone())
                        .unwrap_or_else(|| result.subtype.clone())
                ),
                None => "the turn failed".into(),
            },
        ),
        _ => (
            WorkerGatedRunOutcome::Failed,
            match (ended.exit_code, ended.exit_signal) {
                (Some(code), _) => {
                    format!("the worker's process exited with code {code} before its turn ended")
                }
                (None, Some(signal)) => {
                    format!("the worker's process ended by signal {signal} before its turn ended")
                }
                (None, None) => "the worker's process ended before its turn did".into(),
            },
        ),
    }
}

/// The usage per model and the total cost the CLI's `result` events in the
/// journal at `path` report, summed. A journal that cannot be read reports
/// none.
fn journal_usage(path: &Path) -> (Vec<WorkerModelUsage>, Option<f64>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            warn!(%error, path = %path.display(), "worker.run cannot read the worker's journal");
            return (Vec::new(), None);
        }
    };
    let mut usage: Vec<WorkerModelUsage> = Vec::new();
    let mut total_cost_usd: Option<f64> = None;
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event = &record["event"];
        if record["dir"] != "out" || event["type"] != "result" {
            continue;
        }
        if let Some(cost) = event["total_cost_usd"].as_f64() {
            *total_cost_usd.get_or_insert(0.0) += cost;
        }
        let Some(models) = event["modelUsage"].as_object() else {
            continue;
        };
        for (model, counts) in models {
            let index = match usage.iter().position(|entry| &entry.model == model) {
                Some(index) => index,
                None => {
                    usage.push(WorkerModelUsage {
                        model: model.clone(),
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_read_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cost_usd: None,
                    });
                    usage.len() - 1
                }
            };
            let entry = &mut usage[index];
            let count = |key: &str| counts[key].as_u64().unwrap_or(0);
            entry.input_tokens += count("inputTokens");
            entry.output_tokens += count("outputTokens");
            entry.cache_read_input_tokens += count("cacheReadInputTokens");
            entry.cache_creation_input_tokens += count("cacheCreationInputTokens");
            if let Some(cost) = counts["costUSD"].as_f64() {
                *entry.cost_usd.get_or_insert(0.0) += cost;
            }
        }
    }
    (usage, total_cost_usd)
}
