//! Todo runs and wait edges, driven through the client state: a run's line
//! under its coordinator's space opens its step's log, and a tab whose
//! agent waits shows `waiting: <name>` and the job's row.

use super::*;
use crate::api::schema::Method;
use crate::protocol::{ClientShellTodoRun, ClientShellWait};

const WIDTH: u16 = 120;
const HEIGHT: u16 = 30;

fn left_click(state: &mut ClientShellState, (column, row): (u16, u16)) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn methods(outcome: &ClientShellInput) -> Vec<Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.method.clone()),
            _ => None,
        })
        .collect()
}

fn state_with(projected: ClientShellSnapshot) -> ClientShellState {
    let mut config = config_with_sidebar_width(60);
    config.spaces.tabs = true;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(WIDTH, HEIGHT).unwrap();
    state
}

fn running_install() -> ClientShellTodoRun {
    ClientShellTodoRun {
        run_id: "r-abcd2345".into(),
        repo: "/repo".into(),
        item: "t-aaaaaaaa".into(),
        step: "install".into(),
        status: "running".into(),
        attempt: 1,
        workspace_id: Some("ws_1".into()),
        owner_pane_id: Some("pane_1".into()),
        step_started_ms: Some(1000),
        step_command: Some("just clean-install".into()),
        has_log: true,
    }
}

fn sidebar_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    frame_rows(&frame).join("\n")
}

#[test]
fn a_runs_line_shows_its_step_and_opens_its_log() {
    let mut projected = snapshot();
    projected.todo_runs = vec![running_install()];
    // Pin the label at the step's start, not at the machine's wall clock:
    // an epoch-based fixture otherwise reserves space for decades of age.
    let line = super::super::todo_runs::run_lines_at(&projected, &projected.workspaces[0], 1_000)
        .remove(0);
    assert_eq!(line.label, "run r-abcd2345 · install · just clean-install");
    let mut state = state_with(projected);
    let line = state
        .hits
        .space_tabs
        .iter()
        .find(|(_, id)| super::super::todo_runs::run_line_run(id) == Some("r-abcd2345"))
        .map(|(rect, _)| *rect)
        .expect("the run's line");

    // A server without `todo.open_log` gets nothing.
    state.set_endpoint_methods(Some(Vec::new()));
    let outcome = left_click(&mut state, (line.x + 6, line.y));
    assert!(methods(&outcome).is_empty());

    state.set_endpoint_methods(Some(vec!["todo.open_log".into()]));
    let outcome = left_click(&mut state, (line.x + 6, line.y));
    assert!(methods(&outcome).iter().any(
        |method| matches!(method, Method::TodoOpenLog(target) if target.run_id == "r-abcd2345")
    ));
}

fn gate_question(reason: &str) -> crate::protocol::ClientShellWorkerQuestion {
    // This fixture models a gate addressed to the coordinator. The current
    // pending_usage producer still emits quiet=false (a user cancel request);
    // that must change at the producer, never be hidden by the tool's label.
    crate::protocol::ClientShellWorkerQuestion {
        worker_id: "r-abcd2345".into(),
        request_id: "herdr-usage-r-abcd2345".into(),
        cwd: "/repo".into(),
        tool_name: "usage gate".into(),
        text: reason.into(),
        choice: true,
        since_ms: 1000,
        quiet: false,
        owner_seen_ms: None,
    }
}

#[test]
fn attention_is_a_user_request_not_a_queued_run_or_gate_reason() {
    use crate::api::schema::AgentStatus;
    for reason in [
        None,
        Some("usage unreadable; resumes on a successful measurement"),
        Some("worker cap 2 of 2; resumes when r-aaaaaaaa or r-bbbbbbbb exits"),
    ] {
        for request in [false, true] {
            for quiet in [false, true] {
                let mut projected = snapshot();
                // Isolate the run from the fixture's ordinary agents.
                projected.agents.clear();
                projected.open_questions.clear();
                let mut run = running_install();
                run.status = "waiting".into();
                run.step = "preflight".into();
                run.has_log = false;
                projected.todo_runs = vec![run];
                projected.worker_questions = reason.map(gate_question).into_iter().collect();
                if request {
                    let mut question = gate_question("Which task should this run draft?");
                    question.tool_name = "herdr draft".into();
                    question.request_id = "herdr-review-r-abcd2345-1".into();
                    question.quiet = quiet;
                    projected.worker_questions.push(question);
                }
                let mut state = state_with(projected.clone());
                let line = super::super::todo_runs::run_lines_at(
                    &projected,
                    &projected.workspaces[0],
                    1000,
                )
                .remove(0);
                assert_eq!(
                    line.state == Some((AgentStatus::Blocked, AgentMark::None)),
                    request && !quiet
                );
                assert_eq!(line.ask.is_some(), request && !quiet);
                assert_eq!(
                    state.agent_indicator_counts().1,
                    usize::from(reason.is_some()) + usize::from(request && !quiet)
                );
                assert!(line.label.contains(reason.unwrap_or("reason unknown")));

                // Release is a producing snapshot, never a status fetch.
                projected.worker_questions.clear();
                projected.todo_runs[0].status = "running".into();
                state.set_snapshot(Box::new(projected.clone()));
                let line = super::super::todo_runs::run_lines_at(
                    &projected,
                    &projected.workspaces[0],
                    1000,
                )
                .remove(0);
                assert_eq!(line.state, Some((AgentStatus::Working, AgentMark::None)));
                assert!(line.ask.is_none());
                assert!(!line.label.contains("queued"));
                assert_eq!(state.agent_indicator_counts().1, 0);
            }
        }
    }
}

#[test]
fn clicking_a_queued_run_fetches_status_once_and_renders_the_full_reason() {
    for reason in [
        "worker cap 2 of 2; resumes when r-aaaaaaaa or r-bbbbbbbb exits",
        "usage unreadable; resumes on a successful measurement",
    ] {
        let mut projected = snapshot();
        let mut run = running_install();
        run.status = "waiting".into();
        run.has_log = false;
        let mut other = run.clone();
        other.run_id = "r-other234".into();
        projected.todo_runs = vec![other, run];
        projected.worker_questions = vec![gate_question(reason)];
        let mut state = state_with(projected.clone());
        state.set_endpoint_methods(Some(vec!["todo.status".into()]));
        state.set_snapshot(Box::new(projected));
        assert!(methods(&state.handle_raw_events(Vec::new())).is_empty());
        let rect = state
            .hits
            .space_tabs
            .iter()
            .find(|(_, id)| super::super::todo_runs::run_line_run(id) == Some("r-abcd2345"))
            .unwrap()
            .0;
        let outcome = left_click(&mut state, (rect.x + 6, rect.y));
        let sent = methods(&outcome);
        assert_eq!(sent.len(), 1);
        assert!(matches!(&sent[0], Method::TodoStatus(target) if target.run_id == "r-abcd2345"));
        let request = outcome
            .actions
            .iter()
            .find_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => Some(request),
                _ => None,
            })
            .unwrap();
        let run = serde_json::from_value(serde_json::json!({
            "run_id": "r-abcd2345", "repo": "/repo", "item": "t-aaaaaaaa",
            "step": "preflight", "status": "waiting", "attempt": 1,
            "task": "task", "message": "message", "paths": [], "checks": [],
            "created_ms": 1000, "updated_ms": 1000, "error": reason,
        }))
        .unwrap();
        let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
        let (repaint, actions) = state.handle_endpoint_result(
            &boot,
            &request.id,
            Ok(crate::api::schema::ResponseResult::TodoRun { run }),
        );
        assert!(repaint);
        assert!(actions.is_empty());
        let text = sidebar_text(&mut state);
        let details = match state.overlay.as_ref().unwrap() {
            ClientShellOverlay::RunDetails(details) => details,
            _ => panic!("run details overlay"),
        };
        assert!(details.text.contains(reason));
        for word in reason.split_whitespace() {
            assert!(text.contains(word), "missing {word}: {text}");
        }
        assert!(text.contains("preflight"), "{text}");
        assert!(text.contains("waiting"), "{text}");
        assert!(methods(&state.handle_raw_events(Vec::new())).is_empty());
    }
}

fn click_run(state: &mut ClientShellState) -> ClientShellInput {
    state.compose(WIDTH, HEIGHT).unwrap();
    let rect = state
        .hits
        .space_tabs
        .iter()
        .find(|(_, id)| super::super::todo_runs::run_line_run(id) == Some("r-abcd2345"))
        .unwrap()
        .0;
    left_click(state, (rect.x + 6, rect.y))
}

fn details_text(state: &ClientShellState) -> &str {
    match state.overlay.as_ref().unwrap() {
        ClientShellOverlay::RunDetails(details) => &details.text,
        _ => panic!("run details"),
    }
}

#[test]
fn coordinator_waits_are_not_queued_and_worker_identity_is_not_guessed() {
    for step in ["review", "draft", "retry_conflict"] {
        let mut projected = snapshot();
        let mut run = running_install();
        run.status = "waiting".into();
        run.step = step.into();
        projected.todo_runs = vec![run];
        let line =
            super::super::todo_runs::run_lines_at(&projected, &projected.workspaces[0], 1000)
                .remove(0);
        assert!(line.label.contains("waits for its coordinator"));
        assert!(!line.label.contains("queued"));
        assert!(line.ask.is_none());
    }
    // A real worker request cannot be tied to a run by the frozen snapshot.
    // The permitted fallback keeps it in the badge/list, never guesses by cwd/item.
    for quiet in [true, false] {
        let mut projected = snapshot();
        projected.agents.clear();
        projected.open_questions.clear();
        projected.todo_runs = vec![running_install()];
        let mut question = gate_question("Please choose");
        question.worker_id = "w-worker".into();
        question.request_id = "request-1".into();
        question.quiet = quiet;
        // Even a gate-labelled request counts when addressed to the user.
        projected.worker_questions = vec![question];
        let state = state_with(projected.clone());
        let line =
            super::super::todo_runs::run_lines_at(&projected, &projected.workspaces[0], 1000)
                .remove(0);
        assert!(line.ask.is_none());
        assert_eq!(state.agent_indicator_counts().1, usize::from(!quiet));
    }
}

#[test]
fn a_cancelable_gate_request_stays_in_asking_but_not_on_the_run_line() {
    let mut projected = snapshot();
    projected.agents.clear();
    projected.open_questions.clear();
    let mut run = running_install();
    run.status = "waiting".into();
    run.step = "preflight".into();
    projected.todo_runs = vec![run];
    let mut question = gate_question("Cancel pending start?");
    question.quiet = false;
    projected.worker_questions = vec![question];
    let state = state_with(projected.clone());
    let line =
        super::super::todo_runs::run_lines_at(&projected, &projected.workspaces[0], 1000).remove(0);
    assert!(line.ask.is_none());
    assert_eq!(
        line.state,
        Some((crate::api::schema::AgentStatus::Idle, AgentMark::None))
    );
    assert!(line.label.contains("queued · Cancel pending start?"));
    assert_eq!(state.agent_indicator_counts().1, 1);
}

#[test]
fn details_errors_unsupported_and_replaced_overlays_are_correlated() {
    let mut projected = snapshot();
    let mut run = running_install();
    run.status = "waiting".into();
    run.step = "preflight".into();
    projected.todo_runs = vec![run];
    let mut state = state_with(projected);
    state.set_endpoint_methods(Some(vec![]));
    assert!(methods(&click_run(&mut state)).is_empty());
    assert!(details_text(&state).contains("does not advertise todo.status"));
    state.overlay = None;
    state.set_endpoint_methods(Some(vec!["todo.status".into()]));
    let first = click_run(&mut state);
    assert_eq!(methods(&first).len(), 1);
    assert!(methods(&click_run(&mut state)).is_empty());
    let request_id = first
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.id.clone()),
            _ => None,
        })
        .unwrap();
    let boot = state.snapshot.as_ref().unwrap().boot_id.clone();
    state.overlay = None;
    // The first request still owns the lane; reopening cannot send a second.
    assert!(methods(&click_run(&mut state)).is_empty());
    let before = details_text(&state).to_owned();
    let (repaint, _) = state.handle_endpoint_result(
        &boot,
        &request_id,
        Err(ClientShellEndpointError {
            code: None,
            message: "stale reply".into(),
        }),
    );
    assert!(!repaint);
    assert_eq!(details_text(&state), before);
    state.overlay = None;
    let next = click_run(&mut state);
    let request_id = next
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.id.clone()),
            _ => None,
        })
        .unwrap();
    let (repaint, _) = state.handle_endpoint_result(
        &boot,
        &request_id,
        Err(ClientShellEndpointError {
            code: None,
            message: "status unavailable".into(),
        }),
    );
    assert!(repaint);
    assert!(details_text(&state).contains("status unavailable"));
}

#[test]
fn details_keep_recorded_errors_separate_and_ignore_another_runs_reply() {
    let mut state = state_with(snapshot());
    state.overlay = Some(ClientShellOverlay::RunDetails(
        super::super::todo_runs::RunDetails {
            run_id: "r-abcd2345".into(),
            generation: 7,
            text: "loading".into(),
            scroll: 0,
        },
    ));
    let response = |run_id: &str| crate::api::schema::ResponseResult::TodoRun {
        run: serde_json::from_value(serde_json::json!({
            "run_id": run_id, "repo": "/repo", "item": "t-aaaaaaaa",
            "step": "review", "status": "waiting", "attempt": 2,
            "task": "task", "message": "message", "paths": [], "checks": [],
            "created_ms": 1000, "updated_ms": 1000, "error": "old attempt failed",
        }))
        .unwrap(),
    };
    assert!(
        !state
            .complete_todo_status("r-abcd2345".into(), 7, Ok(response("r-other234")))
            .0
    );
    assert_eq!(details_text(&state), "loading");
    assert!(
        !state
            .complete_todo_status("r-other234".into(), 7, Ok(response("r-other234")))
            .0
    );
    assert!(
        !state
            .complete_todo_status("r-abcd2345".into(), 6, Ok(response("r-abcd2345")))
            .0
    );
    assert_eq!(details_text(&state), "loading");
    assert!(
        state
            .complete_todo_status("r-abcd2345".into(), 7, Ok(response("r-abcd2345")))
            .0
    );
    let text = details_text(&state);
    assert!(text.contains("Point-in-time"));
    assert!(text.contains("Step: review\nStatus: waiting\n\nwaits for its coordinator"));
    assert!(text.contains("Recorded error: old attempt failed"));
}

#[test]
fn a_tab_whose_agent_waits_names_what_it_waits_on() {
    let mut projected = snapshot();
    projected.waits = vec![ClientShellWait {
        pane_id: "pane_1".into(),
        kind: "job".into(),
        target: "j1".into(),
        name: "Build b17".into(),
        tab_id: Some("tab_2".into()),
        since_ms: None,
        last_output_ms: None,
    }];
    let mut state = state_with(projected.clone());
    let text = sidebar_text(&mut state);
    assert!(text.contains("waiting: Build b17"), "{text}");
    assert!(text.contains("↳ job Build b17 · running"), "{text}");
    let waiting = text
        .lines()
        .find(|line| line.contains("waiting: Build b17"))
        .expect("the waiting line");
    assert!(!waiting.contains('⧖'), "{waiting}");
    assert!(
        waiting.contains('◐'),
        "the job glyph, first frame: {waiting}"
    );

    // Without the edge the tab is plainly idle again.
    projected.waits.clear();
    let mut state = state_with(projected);
    let text = sidebar_text(&mut state);
    assert!(!text.contains("waiting:"), "{text}");
}

#[test]
fn a_tab_waiting_on_a_run_turns_its_glyph_with_the_clock() {
    let mut projected = snapshot();
    for agent in &mut projected.agents {
        agent.agent_status = crate::api::schema::AgentStatus::Idle;
    }
    for tab in &mut projected.tabs {
        tab.agent_status = crate::api::schema::AgentStatus::Idle;
    }
    projected.waits = vec![ClientShellWait {
        pane_id: "pane_1".into(),
        kind: "run".into(),
        target: "r-abcd2345".into(),
        name: "r-abcd2345".into(),
        tab_id: None,
        since_ms: None,
        last_output_ms: None,
    }];
    let mut state = state_with(projected);
    // No agent works and no job runs: the wait alone keeps the glyph turning.
    assert!(state.motion_active);
    let waiting = |state: &mut ClientShellState| {
        sidebar_text(state)
            .lines()
            .find(|line| line.contains("waiting: run r-abcd2345"))
            .map(str::to_owned)
            .expect("the waiting line")
    };
    assert!(waiting(&mut state).contains('◐'), "{}", waiting(&mut state));
    // delay: not a wait, a clock reading one frame (160 ms) past the epoch.
    let next_frame = state.motion_epoch + std::time::Duration::from_millis(170);
    assert!(state.tick_motion(next_frame));
    assert!(waiting(&mut state).contains('◒'), "{}", waiting(&mut state));
}
