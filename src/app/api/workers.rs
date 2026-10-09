//! The worker actions a client starts that need the app: `worker.open_log`
//! (a popup) and `worker.take_over` (a tab), and `worker.runs`,
//! `history.list`, `history.item` and `todo.review` for its Items popup.
//! The other `worker.*` methods, and those from the JSON API, run on the
//! API connection's thread (`crate::api::workers`).

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    HistoryItemParams, HistoryListParams, ResponseResult, TodoReviewParams, WorkerRunsParams,
    WorkerTarget,
};
use crate::app::App;
use crate::events::AppEvent;
use crate::popup_size::PopupSize;

impl App {
    /// The workers' runs by item, for a client's Items popup, which asks
    /// when the user opens it.
    pub(super) fn handle_worker_runs(&mut self, id: String, params: WorkerRunsParams) -> String {
        match crate::workers::supervisor().runs(&params) {
            Ok((items, unassigned)) => {
                encode_success(id, ResponseResult::WorkerRuns { items, unassigned })
            }
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// The recorded items, for the Items popup's finished items, asked when
    /// the user opens it.
    pub(super) fn handle_history_list(&mut self, id: String, params: HistoryListParams) -> String {
        match crate::workers::supervisor().history_list(params.repo.as_deref()) {
            Ok(items) => encode_success(id, ResponseResult::HistoryList { items }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// An item's timeline, asked when the user opens it in the Items popup.
    pub(super) fn handle_history_item(&mut self, id: String, params: HistoryItemParams) -> String {
        match crate::workers::supervisor().history_item(&params.item, params.repo.as_deref()) {
            Ok(item) => encode_success(id, ResponseResult::HistoryItem { item }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// One attempt of a run, asked when the user opens it in the Items
    /// popup's timeline.
    pub(super) fn handle_todo_review(&mut self, id: String, params: TodoReviewParams) -> String {
        match crate::workers::supervisor().todo_review(&params.run_id, params.attempt, params.diff)
        {
            Ok(review) => encode_success(
                id,
                ResponseResult::TodoReview {
                    review: Box::new(review),
                },
            ),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// Opens the worker's journal in a popup running `herdr worker log
    /// --follow`, read-only, which follows new events until it is closed.
    pub(super) fn handle_worker_open_log(&mut self, id: String, target: WorkerTarget) -> String {
        if let Err(error) = crate::workers::supervisor().status(&target.worker_id) {
            return encode_error(id, error.code(), error.to_string());
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe.display().to_string(),
            Err(error) => return encode_error(id, "worker_io_error", error.to_string()),
        };
        let argv = vec![
            exe,
            "worker".to_owned(),
            "log".to_owned(),
            "--follow".to_owned(),
            target.worker_id,
        ];
        // The socket path, so the log asks this server for the journal.
        let (env, _) = self.custom_command_env();
        let geometry = crate::app::popup::PopupGeometry {
            width: Some(PopupSize::Percent(90)),
            height: Some(PopupSize::Percent(85)),
        };
        match self.spawn_popup_argv_command(&argv, None, env, geometry) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(error) => encode_error(id, "popup_failed", error.to_string()),
        }
    }

    /// Claims the worker for a takeover and ends it on a thread of its own
    /// (see [`crate::workers::WorkerSupervisor::end_for_takeover`]); the
    /// tab that resumes its session opens once it has exited
    /// ([`AppEvent::WorkerTakenOver`]).
    ///
    /// A takeover an earlier server left unfinished is first looked for
    /// ([`Self::find_unfinished_takeover_tab`]): a tab found is adopted and
    /// the worker returned as taken over; a process resuming the session
    /// outside herdr's tabs, or nothing found, refuses the retry unless
    /// `force` (`worker.force_take_over`).
    pub(super) fn handle_worker_take_over(
        &mut self,
        id: String,
        target: WorkerTarget,
        force: bool,
    ) -> String {
        let supervisor = crate::workers::supervisor().clone();
        let unfinished = match supervisor.unfinished_takeover(&target.worker_id) {
            Ok(unfinished) => unfinished,
            Err(error) => return encode_error(id, error.code(), error.to_string()),
        };
        if let Some(unfinished) = unfinished {
            match self.find_unfinished_takeover_tab(&unfinished) {
                crate::workers::TakeoverFound::Tab(tab_id) => {
                    return match supervisor
                        .adopt_takeover_tab(&unfinished, &tab_id)
                        .and_then(|()| supervisor.status(&target.worker_id))
                    {
                        Ok(worker) => encode_success(id, ResponseResult::WorkerInfo { worker }),
                        Err(error) => encode_error(id, error.code(), error.to_string()),
                    };
                }
                crate::workers::TakeoverFound::Process(pid) if !force => {
                    return encode_error(
                        id,
                        "worker_busy",
                        format!(
                            "worker {}: process {pid} already resumes its session {} outside \
                             herdr's tabs; end it first. Retrying with --force opens another \
                             writer of the same session, which forks its transcript",
                            target.worker_id,
                            unfinished.session_id.as_deref().unwrap_or_default(),
                        ),
                    );
                }
                // `begin_takeover` refuses it without force.
                _ => {}
            }
        }
        let takeover = match supervisor.begin_takeover(&target.worker_id, force) {
            Ok(takeover) => takeover,
            Err(error) => return encode_error(id, error.code(), error.to_string()),
        };
        let worker = match supervisor.status(&target.worker_id) {
            Ok(worker) => worker,
            Err(error) => {
                release_takeover(&target.worker_id, &error.to_string());
                return encode_error(id, error.code(), error.to_string());
            }
        };
        let event_tx = self.event_tx.clone();
        let ending = supervisor.clone();
        let worker_id = takeover.worker_id.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-worker-takeover", move || {
            let error = ending
                .end_for_takeover(&takeover.worker_id)
                .err()
                .map(|error| error.to_string());
            let worker_id = takeover.worker_id.clone();
            if event_tx
                .blocking_send(AppEvent::WorkerTakenOver(Box::new((takeover, error))))
                .is_err()
            {
                release_takeover(&worker_id, "the app stopped before opening the tab");
            }
        });
        match spawned {
            Ok(_) => encode_success(id, ResponseResult::WorkerInfo { worker }),
            Err(error) => {
                let message = format!("could not start the takeover: {error}");
                release_takeover(&worker_id, &message);
                encode_error(id, "worker_io_error", message)
            }
        }
    }

    /// Opens a tab in the worker's space, in its directory, that resumes its
    /// session with `claude --resume`; Claude asks there whether to trust
    /// the directory. Only once the worker has exited: two writers of one
    /// session fork its transcript.
    pub(super) fn open_taken_over_worker(
        &mut self,
        takeover: crate::workers::Takeover,
        error: Option<String>,
    ) {
        let exited = crate::workers::supervisor()
            .status(&takeover.worker_id)
            .is_ok_and(|worker| {
                // `failed` with an exit code: the exit failed the last turn.
                matches!(worker.state, crate::api::schema::WorkerState::Exited)
                    || worker.exit_code.is_some()
                    || worker.exit_signal.is_some()
            });
        if !exited {
            let error = error.as_deref().unwrap_or("the worker did not exit");
            tracing::warn!(
                worker = takeover.worker_id,
                error,
                "worker takeover stopped before the worker exited; no tab opened"
            );
            release_takeover(
                &takeover.worker_id,
                &format!("the worker was not ended: {error}"),
            );
            return;
        }
        let Some(ws_idx) = takeover
            .workspace_id
            .as_deref()
            .and_then(|workspace_id| self.parse_workspace_id(workspace_id))
            .or(self.state.active)
        else {
            tracing::warn!(
                worker = takeover.worker_id,
                "no space to take the worker over in"
            );
            release_takeover(&takeover.worker_id, "no space to open the tab in");
            return;
        };
        // The claim's id in the tab's title and its shell's environment, so
        // a server that starts before the tab is journaled finds the tab.
        let title = match takeover.name.as_str() {
            "" => takeover.takeover_id.clone(),
            name => format!("{name} {}", takeover.takeover_id),
        };
        let env = std::collections::HashMap::from([(
            crate::workers::TAKEOVER_ID_ENV.to_owned(),
            takeover.takeover_id.clone(),
        )]);
        let response = self.create_tab_in_workspace(
            format!("worker-takeover:{}", takeover.worker_id),
            ws_idx,
            super::tabs::NewTabPlace::End,
            Some(takeover.cwd.clone()).filter(|cwd| !cwd.is_empty()),
            true,
            Some(title),
            env,
        );
        let Ok(crate::api::schema::SuccessResponse {
            result: ResponseResult::TabCreated { tab, root_pane },
            ..
        }) = serde_json::from_str(&response)
        else {
            tracing::warn!(
                worker = takeover.worker_id,
                response,
                "takeover tab not created"
            );
            release_takeover(
                &takeover.worker_id,
                &format!("the tab was not created: {response}"),
            );
            return;
        };
        // Recorded before typing: the tab exists now, so a second takeover
        // would open another writer of the session.
        if let Err(err) =
            crate::workers::supervisor().takeover_tab_opened(&takeover.worker_id, &tab.tab_id)
        {
            tracing::warn!(%err, worker = takeover.worker_id, "takeover tab not journaled");
        }
        let args = ["--resume".to_owned(), takeover.session_id.clone()];
        if let Err(err) =
            self.type_agent_launch(&root_pane.pane_id, crate::detect::Agent::Claude, &args)
        {
            tracing::warn!(
                err,
                worker = takeover.worker_id,
                "could not resume the worker"
            );
        }
    }
}

impl App {
    /// Adopts the tabs of the takeovers an earlier server left unfinished,
    /// found by their ids or by a process resuming their sessions; the rest
    /// stay reported as `takeover_unfinished`. Run once at server start,
    /// with the restored and handed-over panes in place.
    pub(crate) fn recover_unfinished_takeovers_at_start(&self) {
        let supervisor = crate::workers::supervisor();
        for unfinished in supervisor.unfinished_takeovers() {
            match self.find_unfinished_takeover_tab(&unfinished) {
                crate::workers::TakeoverFound::Tab(tab_id) => {
                    if let Err(err) = supervisor.adopt_takeover_tab(&unfinished, &tab_id) {
                        tracing::warn!(%err, worker = unfinished.worker_id, "takeover tab not adopted");
                    }
                }
                crate::workers::TakeoverFound::Process(pid) => tracing::warn!(
                    worker = unfinished.worker_id,
                    pid,
                    "an unfinished takeover's session resumes outside herdr's tabs"
                ),
                crate::workers::TakeoverFound::Nothing => tracing::info!(
                    worker = unfinished.worker_id,
                    "an unfinished takeover's tab was not found"
                ),
            }
        }
    }

    /// Looks for an unfinished takeover's tab among every space's tabs
    /// ([`crate::workers::find_takeover_tab`]).
    fn find_unfinished_takeover_tab(
        &self,
        unfinished: &crate::workers::UnfinishedTakeover,
    ) -> crate::workers::TakeoverFound {
        let mut tabs = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
                    continue;
                };
                let shell_pids = tab
                    .panes
                    .keys()
                    .filter_map(|pane_id| self.state.terminal_id_for_pane(ws_idx, *pane_id))
                    .filter_map(|terminal_id| self.terminal_runtimes.get(&terminal_id))
                    .filter_map(|runtime| runtime.child_pid())
                    .collect();
                tabs.push(crate::workers::TakeoverTabCandidate {
                    tab_id,
                    title: tab.custom_name.clone().unwrap_or_default(),
                    shell_pids,
                });
            }
        }
        crate::workers::find_takeover_tab(unfinished, &tabs, &crate::workers::LiveProcesses)
    }
}

/// Releases a takeover claim that failed, so the user can try again.
fn release_takeover(worker_id: &str, error: &str) {
    if let Err(err) = crate::workers::supervisor().fail_takeover(worker_id, error) {
        tracing::warn!(%err, worker = worker_id, "takeover claim not released");
    }
}
