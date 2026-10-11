//! Todo runs in progress as lines under their coordinator's space (`run
//! r-x · install · just clean-install · 4m`; a click opens the step's log,
//! read-only), and what a waiting agent waits on: its tab shows `waiting:
//! <job or run>` with an hourglass instead of the finished dot, and for a
//! job a row `↳ job <name> · running · since 17:30 · last output 17:33`.
//! Times are absolute or elapsed, never judged: no line says a run or a job
//! is stale.

use super::*;
use crate::api::schema::{Method, TodoRunTarget};
use crate::protocol::{ClientShellTodoRun, ClientShellWait};

/// What a run's line knows beyond a tab line's parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunLine {
    pub(super) run_id: String,
    /// What its glyph and label say on hover.
    pub(super) tooltip: String,
}

const RUN_LINE_PREFIX: &str = "todo-run:";

/// The `tab_id` of a run's line, which its line hit carries.
pub(super) fn run_line_id(run_id: &str) -> String {
    format!("{RUN_LINE_PREFIX}{run_id}")
}

/// The run whose line a hit's id names.
pub(super) fn run_line_run(id: &str) -> Option<&str> {
    id.strip_prefix(RUN_LINE_PREFIX)
}

/// The longest job name the tab line's `waiting:` shows; the row under it
/// and the tooltip show it whole.
const WAITING_NAME_WIDTH: usize = 14;

/// The space a run's line goes under: its coordinator's (the run's
/// `workspace_id`, else its owner pane's space), else the space whose
/// directory is in the repository, the shallowest one.
pub(super) fn run_space<'a>(
    snapshot: &'a ClientShellSnapshot,
    run: &ClientShellTodoRun,
) -> Option<&'a str> {
    let known = |id: &str| {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == id)
            .map(|workspace| workspace.workspace_id.as_str())
    };
    if let Some(workspace) = run.workspace_id.as_deref().and_then(known) {
        return Some(workspace);
    }
    if let Some(workspace) = run.owner_pane_id.as_deref().and_then(|owner| {
        snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == owner)
            .and_then(|pane| known(&pane.workspace_id))
    }) {
        return Some(workspace);
    }
    let repo = std::path::Path::new(&run.repo);
    snapshot
        .workspaces
        .iter()
        .filter(|workspace| {
            !workspace.new_workspace_cwd.is_empty()
                && std::path::Path::new(&workspace.new_workspace_cwd).starts_with(repo)
        })
        .min_by_key(|workspace| workspace.new_workspace_cwd.len())
        .map(|workspace| workspace.workspace_id.as_str())
}

/// The run's step as its line says it: `cherry-pick`, not `cherry_pick`.
fn step_text(step: &str) -> String {
    if step.is_empty() {
        "unknown step".to_owned()
    } else {
        step.replace('_', "-")
    }
}

/// One line per todo run in progress of `workspace`, oldest first.
pub(super) fn run_lines(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
) -> Vec<space_tabs::SpaceTabLine> {
    run_lines_at(snapshot, workspace, super::workers::unix_now_ms())
}

/// [`run_lines`] with the step's time counted to `now_ms`.
pub(super) fn run_lines_at(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    now_ms: u64,
) -> Vec<space_tabs::SpaceTabLine> {
    use crate::api::schema::AgentStatus;
    snapshot
        .todo_runs
        .iter()
        .filter(|run| run_space(snapshot, run) == Some(workspace.workspace_id.as_str()))
        .map(|run| {
            let running = run.status == "running";
            let step = step_text(&run.step);
            let mut label = format!("run {} · {step}", run.run_id);
            if let Some(command) = run.step_command.as_deref().filter(|_| running) {
                label.push_str(" · ");
                label.push_str(command);
            }
            let age = run
                .step_started_ms
                .map(|since| super::workers::age_text(now_ms.saturating_sub(since)));
            let (status, said) = match run.status.as_str() {
                "running" => (AgentStatus::Working, "running"),
                "waiting" => (AgentStatus::Done, "waits for its coordinator"),
                _ => (AgentStatus::Idle, "unknown status"),
            };
            let mut tooltip = format!(
                "run {} · item {} · attempt {} · {step}: {said}",
                run.run_id, run.item, run.attempt
            );
            if let Some(command) = run.step_command.as_deref().filter(|_| running) {
                tooltip.push_str(&format!(" · {command}"));
            }
            if let Some(age) = &age {
                tooltip.push_str(&format!(" · at this step for {age}"));
            }
            if run.has_log {
                tooltip.push_str(" · click: the step's log");
            }
            space_tabs::SpaceTabLine {
                tab_id: run_line_id(&run.run_id),
                state: Some((status, AgentMark::None)),
                label,
                agent: None,
                active: false,
                jobs: Vec::new(),
                // Where a worker's age sits; a run on its coordinator says
                // so.
                plan: match (running, age) {
                    (true, age) => age,
                    (false, Some(age)) => Some(format!("waits {age}")),
                    (false, None) => Some("waits".to_owned()),
                },
                squares: Vec::new(),
                unfolded: false,
                hidden_focus: None,
                quiet: Vec::new(),
                role: None,
                exited_parent: None,
                ask: None,
                worker: None,
                items: None,
                background: None,
                background_count: 0,
                restart: None,
                run: Some(RunLine {
                    run_id: run.run_id.clone(),
                    tooltip,
                }),
                wait: None,
            }
        })
        .collect()
}

/// When a run's shown step time next changes, so the client redraws then
/// and not on a timer of its own.
pub(super) fn run_times_change_at<'a>(
    runs: impl IntoIterator<Item = &'a ClientShellTodoRun>,
    now_ms: u64,
) -> Option<u64> {
    runs.into_iter()
        .filter_map(|run| run.step_started_ms)
        .map(|since| super::workers::age_text_changes_at(since, now_ms))
        .min()
}

/// What the pane waits on now: a job before a run.
pub(super) fn pane_wait<'a>(
    snapshot: &'a ClientShellSnapshot,
    pane_id: &str,
) -> Option<&'a ClientShellWait> {
    let mut waits = snapshot.waits.iter().filter(|wait| wait.pane_id == pane_id);
    let first = waits.next()?;
    if first.kind == "job" {
        return Some(first);
    }
    waits.find(|wait| wait.kind == "job").or(Some(first))
}

/// What a pane of the tab waits on now.
pub(super) fn tab_wait<'a>(
    snapshot: &'a ClientShellSnapshot,
    tab_id: &str,
) -> Option<&'a ClientShellWait> {
    snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab_id)
        .find_map(|pane| pane_wait(snapshot, &pane.pane_id))
}

/// `waiting: <job name>` or `waiting: run r-x`.
pub(super) fn waiting_text(wait: &ClientShellWait) -> String {
    match wait.kind.as_str() {
        "job" => {
            let name = space_tabs::truncate(&wait.name, WAITING_NAME_WIDTH);
            format!("waiting: {}", name.trim_end())
        }
        "run" => format!("waiting: run {}", wait.target),
        _ => "waiting".to_owned(),
    }
}

/// The tab line's `waiting:` text, while its state shows the wait.
pub(super) fn tab_waiting_text(
    snapshot: &ClientShellSnapshot,
    tab: &ClientShellTab,
    state: Option<(crate::api::schema::AgentStatus, AgentMark)>,
) -> Option<String> {
    if !matches!(state, Some((_, AgentMark::Waits))) {
        return None;
    }
    tab_wait(snapshot, &tab.tab_id).map(waiting_text)
}

/// `job <name> · running · since 17:30 · last output 17:33`, the times in
/// the local offset `utc_offset_secs`, a time left out when unknown.
pub(super) fn job_wait_row(wait: &ClientShellWait, now_unix: u64, utc_offset_secs: i64) -> String {
    let clock = |unix_ms: u64| {
        let day = super::notification_log::notification_day(unix_ms, now_unix, utc_offset_secs);
        let time = super::notification_log::notification_time(unix_ms, utc_offset_secs);
        if day == "Today" {
            time
        } else {
            format!("{day} {time}")
        }
    };
    let mut row = format!("job {} · running", wait.name);
    if let Some(since) = wait.since_ms {
        row.push_str(&format!(" · since {}", clock(since)));
    }
    if let Some(output) = wait.last_output_ms {
        row.push_str(&format!(" · last output {}", clock(output)));
    }
    row
}

/// The row under a waiting tab's line for the job it waits on.
pub(super) fn tab_wait_row(
    snapshot: &ClientShellSnapshot,
    tab: &ClientShellTab,
    state: Option<(crate::api::schema::AgentStatus, AgentMark)>,
) -> Option<String> {
    if !matches!(state, Some((_, AgentMark::Waits))) {
        return None;
    }
    let wait = tab_wait(snapshot, &tab.tab_id).filter(|wait| wait.kind == "job")?;
    Some(job_wait_row(
        wait,
        super::workers::unix_now_ms() / 1000,
        super::usage::local_utc_offset_secs(),
    ))
}

impl ClientShellState {
    /// The run whose line is at `point`.
    pub(super) fn run_line_at(&self, point: (u16, u16)) -> Option<String> {
        if self.sidebar_collapsed {
            return None;
        }
        self.hits
            .space_tabs
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
            .and_then(|(_, id)| run_line_run(id))
            .map(str::to_owned)
    }

    /// Asks the server for a popup that shows the run's step log and
    /// follows it; a run without a log, or a server without
    /// `todo.open_log`, opens nothing.
    pub(super) fn open_run_log(&mut self, run_id: String, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let has_log = self.snapshot.as_deref().is_some_and(|snapshot| {
            snapshot
                .todo_runs
                .iter()
                .any(|run| run.run_id == run_id && run.has_log)
        });
        let method = Method::TodoOpenLog(TodoRunTarget { run_id });
        if !has_log || !self.supports_endpoint_method(&method) {
            return;
        }
        self.popup_pending = true;
        self.popup_pending_deadline = None;
        self.popup_pending_dismissable = true;
        if !self.push_endpoint_method_with_kind(method, PendingEndpointKind::PopupCommand, outcome)
        {
            self.popup_pending = false;
            self.popup_pending_dismissable = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(run_id: &str, status: &str) -> ClientShellTodoRun {
        ClientShellTodoRun {
            run_id: run_id.into(),
            repo: "/repo".into(),
            item: "t-aaaaaaaa".into(),
            step: "install".into(),
            status: status.into(),
            attempt: 1,
            workspace_id: Some("w1".into()),
            owner_pane_id: None,
            step_started_ms: Some(1_000),
            step_command: Some("just clean-install TODO.md".into()),
            has_log: true,
        }
    }

    #[test]
    fn a_running_run_shows_its_step_command_and_time_under_its_space() {
        let mut snapshot = super::super::tests::snapshot();
        let workspace = |id: &str| ClientShellWorkspace {
            workspace_id: id.into(),
            new_workspace_cwd: format!("/elsewhere/{id}"),
            ..snapshot.workspaces[0].clone()
        };
        snapshot.workspaces = vec![workspace("w1"), workspace("w2")];
        snapshot.todo_runs = vec![run("r-abcd2345", "running")];
        let lines = run_lines_at(&snapshot, &snapshot.workspaces[0], 1_000 + 4 * 60_000);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(
            line.label,
            "run r-abcd2345 · install · just clean-install TODO.md"
        );
        assert_eq!(line.plan.as_deref(), Some("4m"));
        assert_eq!(run_line_run(&line.tab_id), Some("r-abcd2345"));
        assert!(line
            .run
            .as_ref()
            .is_some_and(|run| run.tooltip.contains("click: the step's log")));
        assert!(run_lines_at(&snapshot, &snapshot.workspaces[1], 0).is_empty());

        // On its coordinator: the command no longer runs.
        snapshot.todo_runs = vec![run("r-abcd2345", "waiting")];
        let lines = run_lines_at(&snapshot, &snapshot.workspaces[0], 1_000);
        assert_eq!(lines[0].label, "run r-abcd2345 · install");
        // delay: not a wait, the line's displayed text.
        assert_eq!(lines[0].plan.as_deref(), Some("waits <1m"));
        // The step time moves at the next whole minute, not on a timer.
        assert_eq!(
            run_times_change_at(&snapshot.todo_runs, 1_500),
            Some(61_000)
        );
    }

    #[test]
    fn a_wait_on_a_job_names_it_with_absolute_times() {
        let wait = ClientShellWait {
            pane_id: "w1:p1".into(),
            kind: "job".into(),
            target: "j1".into(),
            name: "Build herdr with a very long name".into(),
            tab_id: Some("w1:t2".into()),
            since_ms: Some((17 * 3600 + 30 * 60) * 1000),
            last_output_ms: Some((17 * 3600 + 33 * 60) * 1000),
        };
        assert_eq!(
            job_wait_row(&wait, 18 * 3600, 0),
            "job Build herdr with a very long name · running · since 17:30 · last output 17:33"
        );
        assert!(waiting_text(&wait).starts_with("waiting: Build herdr"));
        let run = ClientShellWait {
            kind: "run".into(),
            target: "r-abcd2345".into(),
            name: "r-abcd2345".into(),
            tab_id: None,
            last_output_ms: None,
            ..wait.clone()
        };
        assert_eq!(waiting_text(&run), "waiting: run r-abcd2345");
        let mut snapshot = super::super::tests::snapshot();
        snapshot.waits = vec![run.clone(), wait.clone()];
        assert_eq!(pane_wait(&snapshot, "w1:p1"), Some(&wait), "a job wins");
        assert_eq!(pane_wait(&snapshot, "w1:p9"), None);
    }
}
