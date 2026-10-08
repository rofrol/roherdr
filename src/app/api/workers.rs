//! The worker actions a client starts that need the app: `worker.open_log`
//! (a popup) and `worker.take_over` (a tab), and `worker.runs` for its
//! Items popup. The other `worker.*` methods, and `worker.runs` from the
//! JSON API, run on the API connection's thread (`crate::api::workers`).

use super::responses::{encode_error, encode_success};
use crate::api::schema::{ResponseResult, WorkerRunsParams, WorkerTarget};
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
    pub(super) fn handle_worker_take_over(&mut self, id: String, target: WorkerTarget) -> String {
        let supervisor = crate::workers::supervisor().clone();
        let takeover = match supervisor.begin_takeover(&target.worker_id) {
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
        let response = self.create_tab_in_workspace(
            format!("worker-takeover:{}", takeover.worker_id),
            ws_idx,
            super::tabs::NewTabPlace::End,
            Some(takeover.cwd.clone()).filter(|cwd| !cwd.is_empty()),
            true,
            Some(takeover.name.clone()).filter(|name| !name.is_empty()),
            Default::default(),
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

/// Releases a takeover claim that failed, so the user can try again.
fn release_takeover(worker_id: &str, error: &str) {
    if let Err(err) = crate::workers::supervisor().fail_takeover(worker_id, error) {
        tracing::warn!(%err, worker = worker_id, "takeover claim not released");
    }
}
