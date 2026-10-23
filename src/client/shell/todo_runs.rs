//! Todo runs in progress as lines under their coordinator's space (`run
//! r-x · install · just clean-install · 4m`; a click opens queued run
//! details or the running step's log, read-only), a run that stopped with a
//! reason until the user acknowledges it in its details (`run r-x · stopped
//! · <reason>`), and what a waiting agent waits on: its tab shows `waiting:
//! <job or run>` with the running job's turning half circle instead of the
//! finished dot, and for a job a row `↳ job <name> · running · since 17:30 ·
//! last output 17:33`.
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

/// Read-only, user-opened details. Only the click fetches status; snapshots
/// continue to drive the run line independently.
#[derive(Debug)]
pub(super) struct RunDetails {
    pub(super) run_id: String,
    pub(super) generation: u64,
    pub(super) text: String,
    pub(super) scroll: u16,
    /// The run stopped with a reason and the server advertises
    /// `todo.ack_stop`: the details offer to acknowledge it.
    pub(super) ack: bool,
    /// The acknowledgement was sent and has no reply yet.
    pub(super) acknowledging: bool,
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

/// What a stopped run's details say before anything else: the full reason.
fn stopped_text(reason: &str) -> String {
    format!("Stopped: no next item started\n\n{reason}")
}

/// The run's step as its line says it: `cherry-pick`, not `cherry_pick`.
fn step_text(step: &str) -> String {
    if step.is_empty() {
        "unknown step".to_owned()
    } else {
        step.replace('_', "-")
    }
}

/// A stopped run's reason on one line: its lines joined by spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The line of a run that stopped with `reason`, until it is acknowledged.
fn stopped_line(run: &ClientShellTodoRun, reason: &str) -> space_tabs::SpaceTabLine {
    use crate::api::schema::AgentStatus;
    let reason = one_line(reason);
    space_tabs::SpaceTabLine {
        tab_id: run_line_id(&run.run_id),
        state: Some((AgentStatus::Done, AgentMark::None)),
        label: format!("run {} · stopped · {reason}", run.run_id),
        agent: None,
        active: false,
        jobs: Vec::new(),
        plan: None,
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
        context: None,
        run: Some(RunLine {
            run_id: run.run_id.clone(),
            tooltip: format!(
                "run {} · item {} · stopped, no next item started: {reason} · click: run details",
                run.run_id, run.item
            ),
        }),
        wait: None,
    }
}

/// One line per todo run of `workspace`, oldest first: those in progress,
/// and those stopped with a reason not acknowledged yet.
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
    // Index once outside the run-scaled loop; a run reads one presentation fact.
    let is_usage_request = |question: &crate::protocol::ClientShellWorkerQuestion| {
        question.request_id.strip_prefix("herdr-usage-") == Some(question.worker_id.as_str())
    };
    let usage_reasons = snapshot
        .worker_questions
        .iter()
        .filter(|question| is_usage_request(question))
        .map(|question| (question.worker_id.as_str(), question.text.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let user_requests = snapshot
        .worker_questions
        .iter()
        // Run-addressed gate requests remain cancelable in the asking list,
        // but describe queued work rather than attention on a run's line.
        .filter(|question| !question.quiet && !is_usage_request(question))
        .map(|question| (question.worker_id.as_str(), question.text.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    snapshot
        .todo_runs
        .iter()
        .filter(|run| run_space(snapshot, run) == Some(workspace.workspace_id.as_str()))
        .map(|run| {
            if let Some(reason) = run.stop_reason.as_deref() {
                return stopped_line(run, reason);
            }
            let running = run.status == "running";
            let step = step_text(&run.step);
            let usage_reason = usage_reasons.get(run.run_id.as_str()).copied();
            let mut label = format!("run {} · {step}", run.run_id);
            let queued = run.status == "waiting"
                && (usage_reason.is_some()
                    || matches!(run.step.as_str(), "preflight" | "start" | "" | "unknown"));
            if queued {
                label.push_str(" · queued · ");
                label.push_str(usage_reason.unwrap_or("reason unknown"));
            } else if run.status == "waiting" {
                label.push_str(" · waits for its coordinator");
            }
            if let Some(command) = run.step_command.as_deref().filter(|_| running) {
                label.push_str(" · ");
                label.push_str(command);
            }
            let age = run
                .step_started_ms
                .map(|since| super::workers::age_text(now_ms.saturating_sub(since)));
            // The frozen snapshot has no run -> current worker identity. Ordinary
            // worker questions therefore remain in the asking badge/list, not an
            // ambiguous run's `ask`. Only run-addressed requests can match here.
            let request = user_requests.get(run.run_id.as_str()).copied();
            let (status, said) = if request.is_some() {
                (AgentStatus::Blocked, "waits for your answer")
            } else {
                match run.status.as_str() {
                    "running" => (AgentStatus::Working, "running"),
                    "waiting" if queued => (AgentStatus::Idle, "queued"),
                    "waiting" => (AgentStatus::Idle, "waits for its coordinator"),
                    _ => (AgentStatus::Idle, "unknown status"),
                }
            };
            let mut tooltip = format!(
                "run {} · item {} · attempt {} · {step}: {said}",
                run.run_id, run.item, run.attempt
            );
            if let Some(reason) = usage_reason {
                tooltip.push_str(&format!(" · {reason}"));
            }
            if let Some(command) = run.step_command.as_deref().filter(|_| running) {
                tooltip.push_str(&format!(" · {command}"));
            }
            if let Some(age) = &age {
                tooltip.push_str(&format!(" · at this step for {age}"));
            }
            if run.status == "waiting" {
                tooltip.push_str(" · click: run details");
            } else if run.has_log {
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
                ask: request.map(str::to_owned),
                worker: None,
                items: None,
                background: None,
                background_count: 0,
                restart: None,
                context: None,
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
    fn open_run_details(&mut self, run_id: String, outcome: &mut ClientShellInput) {
        if matches!(&self.overlay, Some(ClientShellOverlay::RunDetails(details)) if details.run_id == run_id)
        {
            return;
        }
        let generation = self.next_request_id;
        let method = Method::TodoStatus(TodoRunTarget {
            run_id: run_id.clone(),
        });
        let supported = self.supports_endpoint_method(&method);
        let stop_reason = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .todo_runs
                .iter()
                .find(|run| run.run_id == run_id)
                .and_then(|run| run.stop_reason.clone())
        });
        let ack = stop_reason.is_some()
            && self.supports_endpoint_method(&Method::TodoAckStop(TodoRunTarget {
                run_id: run_id.clone(),
            }));
        let text = match (&stop_reason, supported) {
            // The snapshot already has the reason; the status adds the rest.
            (Some(reason), _) => stopped_text(reason),
            (None, true) => "Loading run details…".into(),
            (None, false) => "This server does not advertise todo.status.".into(),
        };
        self.overlay = Some(ClientShellOverlay::RunDetails(RunDetails {
            run_id: run_id.clone(),
            generation,
            text,
            scroll: 0,
            ack,
            acknowledging: false,
        }));
        let busy = self
            .pending_requests
            .values()
            .any(|pending| matches!(pending.kind, PendingEndpointKind::TodoStatus { .. }));
        if supported
            && (busy
                || !self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::TodoStatus { run_id, generation },
                    outcome,
                ))
        {
            if let Some(ClientShellOverlay::RunDetails(details)) = self.overlay.as_mut() {
                // A stopped run keeps the reason its snapshot gave.
                if stop_reason.is_none() {
                    details.text = "Run details could not be requested.".into();
                }
            }
        }
    }

    pub(super) fn complete_todo_status(
        &mut self,
        run_id: String,
        generation: u64,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::RunDetails(details)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if details.run_id != run_id || details.generation != generation {
            return (false, Vec::new());
        }
        if matches!(&result, Ok(crate::api::schema::ResponseResult::TodoRun { run }) if run.run_id != run_id)
        {
            return (false, Vec::new());
        }
        details.text = match result {
            Ok(crate::api::schema::ResponseResult::TodoRun { run }) => {
                // Enum serialization is the API/sidebar vocabulary, never Rust Debug.
                let step = serde_json::to_value(run.step).ok();
                let status = serde_json::to_value(run.status).ok();
                let step = step.as_ref().and_then(|v| v.as_str()).unwrap_or("unknown");
                let status = status
                    .as_ref()
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let reason = if status == "waiting" && matches!(step, "preflight" | "start") {
                    run.error.as_deref().unwrap_or("reason unknown")
                } else if status == "waiting" {
                    "waits for its coordinator"
                } else {
                    status
                };
                let mut text = match run.stop_reason.as_deref() {
                    Some(stop) if status == "done" => {
                        format!("{}\n\nStep: {}", stopped_text(stop), step_text(step))
                    }
                    _ => format!(
                        "Point-in-time run details\nStep: {}\nStatus: {status}\n\n{reason}",
                        step_text(step),
                    ),
                };
                if let Some(error) = run.error.as_deref().filter(|text| !text.is_empty()) {
                    text.push_str(&format!("\n\nRecorded error: {error}"));
                }
                text
            }
            Ok(_) => "The server returned unexpected run details.".into(),
            Err(error) => error.message,
        };
        (true, Vec::new())
    }

    /// Acknowledges the stop reason of the run whose details are open, when
    /// they offer it: the server then stops listing the run, which removes
    /// its line.
    pub(super) fn acknowledge_run_stop(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::RunDetails(details)) = self.overlay.as_ref() else {
            return;
        };
        if !details.ack || details.acknowledging {
            return;
        }
        let run_id = details.run_id.clone();
        outcome.repaint = true;
        let method = Method::TodoAckStop(TodoRunTarget {
            run_id: run_id.clone(),
        });
        let sent = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::TodoAckStop {
                run_id: run_id.clone(),
            },
            outcome,
        );
        if let Some(ClientShellOverlay::RunDetails(details)) = self.overlay.as_mut() {
            if sent {
                details.acknowledging = true;
            } else {
                details.text =
                    format!("{}\n\nThe acknowledgement could not be sent.", details.text);
            }
        }
    }

    pub(super) fn complete_todo_ack_stop(
        &mut self,
        run_id: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::RunDetails(details)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if details.run_id != run_id {
            return (false, Vec::new());
        }
        match result {
            // The next snapshot no longer lists the run.
            Ok(_) => self.overlay = None,
            Err(error) => {
                details.acknowledging = false;
                details.text = format!("{}\n\nNot acknowledged: {}", details.text, error.message);
            }
        }
        (true, Vec::new())
    }

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
    /// follows it. Waiting and stopped runs instead open read-only details
    /// fetched with `todo.status`, even when they have no log. Running runs without a
    /// log or without `todo.open_log` support open nothing.
    pub(super) fn open_run_log(&mut self, run_id: String, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        if self.snapshot.as_deref().is_some_and(|snapshot| {
            snapshot.todo_runs.iter().any(|run| {
                run.run_id == run_id && (run.status == "waiting" || run.stop_reason.is_some())
            })
        }) {
            self.open_run_details(run_id, outcome);
            return;
        }
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
            stop_reason: None,
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
        assert_eq!(
            lines[0].label,
            "run r-abcd2345 · install · waits for its coordinator"
        );
        // delay: not a wait, the line's displayed text.
        assert_eq!(lines[0].plan.as_deref(), Some("waits <1m"));
        // The step time moves at the next whole minute, not on a timer.
        assert_eq!(
            run_times_change_at(&snapshot.todo_runs, 1_500),
            Some(61_000)
        );
    }

    #[test]
    fn a_usage_wait_shows_its_reason_and_history_without_a_wire_change() {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.workspaces[0].workspace_id = "w1".into();
        let mut pending = run("r-abcd2345", "waiting");
        pending.step = "preflight".into();
        snapshot.todo_runs = vec![pending];
        for reason in [
            "usage read failed (DNS: lookup failed), waiting for a successful measurement",
            "worker cap 2 of 2; resumes when a slot is released by r-aaaaaaaa or r-bbbbbbbb",
            "Claude usage 91%, resumes below 80%; last good reading (history only): 91%",
        ] {
            snapshot.worker_questions = vec![crate::protocol::ClientShellWorkerQuestion {
                worker_id: "r-abcd2345".into(),
                request_id: "herdr-usage-r-abcd2345".into(),
                cwd: "/repo".into(),
                tool_name: "usage gate".into(),
                text: reason.into(),
                choice: true,
                since_ms: 1000,
                // Match the producer: gate questions are visible and cancelable.
                quiet: false,
                owner_seen_ms: None,
            }];
            let lines = run_lines_at(&snapshot, &snapshot.workspaces[0], 1000);
            assert_eq!(lines.len(), 1);
            assert!(lines[0].label.contains(reason));
            assert!(lines[0].run.as_ref().unwrap().tooltip.contains(reason));
            assert!(lines[0].run.as_ref().unwrap().tooltip.contains("queued"));
            assert_eq!(
                lines[0].state,
                Some((crate::api::schema::AgentStatus::Idle, AgentMark::None))
            );
        }
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
