//! What the client snapshot says panes wait on, and the todo runs in
//! progress with their owners by canonical pane id. A wait edge is a
//! runtime fact: a `herdr-job wait` the waiting agent's Stop hook reported
//! (`bg_wait_jobs`, the job ids) while that job's tab runs, or a
//! `todo.wait` that blocks now. The job's start and last output come from
//! the job's own pane (`job_started_ms`, `job_output_ms`, which its
//! executor reports at the start and at the first output of each minute):
//! one watcher per job, whose reports rebuild the snapshot like any
//! metadata change.

use crate::api::schema::TabStatus;
use crate::app::App;
use crate::protocol::{ClientShellTodoRun, ClientShellWait};

/// The pane metadata token in which an agent's Stop hook names the
/// herdr-job jobs its background waits wait on, space-separated.
pub(crate) const WAIT_JOBS_TOKEN: &str = "bg_wait_jobs";
/// The tokens a job's executor reports on the job's own pane: when it
/// started and when it last printed, Unix milliseconds.
pub(crate) const JOB_STARTED_TOKEN: &str = "job_started_ms";
pub(crate) const JOB_OUTPUT_TOKEN: &str = "job_output_ms";

fn millis(value: Option<&str>) -> Option<u64> {
    value?.trim().parse().ok()
}

/// A running job's tab, found by its job id.
struct JobTab<'a> {
    tab_id: String,
    name: &'a str,
    started_ms: Option<u64>,
    last_output_ms: Option<u64>,
}

impl App {
    fn running_job_tab(&self, job_id: &str) -> Option<JobTab<'_>> {
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                let Some(job) = tab.job.as_ref().filter(|job| job.id == job_id) else {
                    continue;
                };
                if tab.status != Some(TabStatus::Running) {
                    return None;
                }
                let token = |key: &str| {
                    tab.panes.values().find_map(|pane| {
                        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                        millis(terminal.metadata_tokens.get(key))
                    })
                };
                return Some(JobTab {
                    tab_id: self.public_tab_id(ws_idx, tab_idx)?,
                    name: &job.name,
                    started_ms: token(JOB_STARTED_TOKEN),
                    last_output_ms: token(JOB_OUTPUT_TOKEN),
                });
            }
        }
        None
    }

    /// Every wait edge now, by the waiting pane's canonical public id: the
    /// jobs whose tabs still run that an agent's background waits wait on,
    /// then the `todo.wait`s that block.
    pub(crate) fn client_shell_waits(
        &self,
        run_waits: Vec<crate::workers::RunWait>,
    ) -> Vec<ClientShellWait> {
        let mut waits = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(jobs) = self
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .and_then(|terminal| terminal.metadata_tokens.get(WAIT_JOBS_TOKEN))
                    else {
                        continue;
                    };
                    let Some(waiter) = self.public_pane_id(ws_idx, *pane_id) else {
                        continue;
                    };
                    for job_id in jobs.split_whitespace() {
                        if waits.iter().any(|wait: &ClientShellWait| {
                            wait.pane_id == waiter && wait.target == job_id
                        }) {
                            continue;
                        }
                        let Some(job) = self.running_job_tab(job_id) else {
                            continue;
                        };
                        waits.push(ClientShellWait {
                            pane_id: waiter.clone(),
                            kind: "job".into(),
                            target: job_id.to_owned(),
                            name: job.name.to_owned(),
                            tab_id: Some(job.tab_id),
                            since_ms: job.started_ms,
                            last_output_ms: job.last_output_ms,
                        });
                    }
                }
            }
        }
        for wait in run_waits {
            let pane_id = self.canonical_pane_id(&wait.waiter_pane);
            if waits
                .iter()
                .any(|known| known.pane_id == pane_id && known.target == wait.run_id)
            {
                continue;
            }
            waits.push(ClientShellWait {
                pane_id,
                kind: "run".into(),
                name: wait.run_id.clone(),
                target: wait.run_id,
                tab_id: None,
                since_ms: Some(wait.since_ms),
                last_output_ms: None,
            });
        }
        waits
    }

    /// The todo runs in progress, and the stopped ones not acknowledged
    /// yet, with their owners' panes by canonical id.
    pub(crate) fn client_shell_todo_runs(
        &self,
        runs: Vec<crate::workers::ActiveRun>,
    ) -> Vec<ClientShellTodoRun> {
        runs.into_iter()
            .map(|run| ClientShellTodoRun {
                step: snake_name(&run.step),
                status: snake_name(&run.status),
                owner_pane_id: run
                    .owner_pane
                    .as_deref()
                    .map(|pane| self.canonical_pane_id(pane)),
                run_id: run.run_id,
                repo: run.repo,
                item: run.item,
                attempt: run.attempt,
                workspace_id: run.workspace_id,
                step_started_ms: run.step_started_ms,
                step_command: run.step_command,
                has_log: run.step_log.is_some(),
                stop_reason: run.stop_reason,
            })
            .collect()
    }
}

/// An enum's serde name, as `todo.status` writes it.
fn snake_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{TabJobMetadata, TodoRunStatus, TodoStep};

    fn app_with_job_and_waiter() -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("jobs");
        let coordinator_pane = workspace.tabs[0].root_pane;
        let job_tab = workspace.test_add_tab(Some("Build b17"));
        let tab = &mut workspace.tabs[job_tab];
        tab.status = Some(TabStatus::Running);
        tab.job = Some(TabJobMetadata {
            id: "20261011-173000-ab12".into(),
            name: "Build b17".into(),
            why: None,
            origin: "test".into(),
            owner_pane: None,
        });
        let job_pane = tab.root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let set = |app: &mut App, pane: crate::layout::PaneId, key: &str, value: &str| {
            let terminal_id = app.state.workspaces[0]
                .pane_state(pane)
                .expect("the pane")
                .attached_terminal_id
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).expect("terminal");
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([(key.to_owned(), Some(value.to_owned()))]),
                None,
                std::time::Instant::now(),
            );
        };
        set(
            &mut app,
            coordinator_pane,
            WAIT_JOBS_TOKEN,
            "20261011-173000-ab12 gone-job",
        );
        set(&mut app, job_pane, JOB_STARTED_TOKEN, "1000");
        set(&mut app, job_pane, JOB_OUTPUT_TOKEN, "180000");
        let waiter = app.public_pane_id(0, coordinator_pane).expect("public id");
        (app, waiter)
    }

    #[test]
    fn a_wait_on_a_running_job_names_it_with_its_start_and_last_output() {
        let (mut app, waiter) = app_with_job_and_waiter();
        let waits = app.client_shell_waits(Vec::new());
        assert_eq!(
            waits.len(),
            1,
            "a job without a running tab is no edge: {waits:?}"
        );
        let wait = &waits[0];
        assert_eq!(wait.pane_id, waiter);
        assert_eq!(wait.kind, "job");
        assert_eq!(wait.name, "Build b17");
        assert_eq!(wait.since_ms, Some(1000));
        assert_eq!(wait.last_output_ms, Some(180_000));
        assert!(wait.tab_id.is_some());

        // The job ended: the edge is gone with it.
        for tab in &mut app.state.workspaces[0].tabs {
            if tab.job.is_some() {
                tab.status = Some(TabStatus::Succeeded);
            }
        }
        assert!(app.client_shell_waits(Vec::new()).is_empty());
    }

    #[test]
    fn a_blocking_todo_wait_is_an_edge_of_its_pane() {
        let (app, waiter) = app_with_job_and_waiter();
        let waits = app.client_shell_waits(vec![crate::workers::RunWait {
            waiter_pane: waiter.clone(),
            run_id: "r-abcd2345".into(),
            since_ms: 5,
        }]);
        let run = waits
            .iter()
            .find(|wait| wait.kind == "run")
            .expect("the run's wait");
        assert_eq!(run.pane_id, waiter);
        assert_eq!(run.name, "r-abcd2345");
        assert_eq!(run.since_ms, Some(5));
    }

    #[test]
    fn a_run_in_progress_names_its_step_status_and_owner() {
        let (app, waiter) = app_with_job_and_waiter();
        let runs = app.client_shell_todo_runs(vec![crate::workers::ActiveRun {
            run_id: "r-abcd2345".into(),
            repo: "/repo".into(),
            item: "t-aaaaaaaa".into(),
            step: TodoStep::CherryPick,
            status: TodoRunStatus::Running,
            attempt: 2,
            workspace_id: None,
            owner_pane: Some(waiter.clone()),
            step_started_ms: Some(7),
            step_command: Some("just clean-install TODO.md".into()),
            step_log: Some("/state/run.log".into()),
            stop_reason: None,
        }]);
        assert_eq!(runs[0].step, "cherry_pick");
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].owner_pane_id.as_deref(), Some(waiter.as_str()));
        assert!(runs[0].has_log);
    }
}
