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
        step_started_ms: Some(super::super::workers::unix_now_ms()),
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
    let mut state = state_with(projected);
    assert!(
        sidebar_text(&mut state).contains("run r-abcd2345 · install · just"),
        "{}",
        sidebar_text(&mut state)
    );
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
    assert!(text.contains('⧖'), "{text}");

    // Without the edge the tab is plainly idle again.
    projected.waits.clear();
    let mut state = state_with(projected);
    let text = sidebar_text(&mut state);
    assert!(!text.contains("waiting:"), "{text}");
    assert!(!text.contains('⧖'), "{text}");
}
