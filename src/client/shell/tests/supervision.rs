//! The Supervision tab pinned at the top of the sidebar, driven through the
//! client state: its row above the spaces, its view in the main pane, the
//! coordinator pane link, freshness, and the `?` entry's link to the same
//! finding.

use super::*;
use crate::api::schema::Method;
use crate::client::shell::worker_question::WorkerQuestionButton;
use crate::protocol::{
    ClientShellSupervisedCoordinator, ClientShellSupervision, ClientShellSupervisionFinding,
    ClientShellTodoRun,
};

const WIDTH: u16 = 140;
const HEIGHT: u16 = 40;

fn now() -> u64 {
    super::super::workers::unix_now_ms()
}

fn interval_ms() -> u64 {
    crate::workers::supervision::SWEEP_INTERVAL.as_millis() as u64
}

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

fn finding(id: &str, severity: &str) -> ClientShellSupervisionFinding {
    ClientShellSupervisionFinding {
        finding_id: id.into(),
        predicate: "coordinator_idle".into(),
        severity: severity.into(),
        status: "open".into(),
        summary: format!("{id}: the coordinator is idle with runnable items"),
        expected_event: "the coordinator starts the next item".into(),
        repo: Some("/repo".into()),
        coordinator_id: Some("c-1".into()),
        pane_id: Some("pane_1".into()),
        run_id: None,
        event_ids: vec!["pane pane_1 state change 41".into()],
        facts: vec!["agent_status: idle".into()],
        first_seen_ms: now(),
        last_seen_ms: now(),
        acknowledged_by: None,
    }
}

fn supervision(findings: Vec<ClientShellSupervisionFinding>) -> ClientShellSupervision {
    ClientShellSupervision {
        swept_ms: Some(now()),
        trigger: Some("timer".into()),
        sweep_error: None,
        sweep_interval_ms: interval_ms(),
        coordinators: vec![ClientShellSupervisedCoordinator {
            coordinator_id: "c-1".into(),
            repo: "/home/u/herdr".into(),
            pane_id: Some("pane_1".into()),
            agent_status: Some("idle".into()),
            workers: vec!["w7".into()],
            active_run: Some("r-abcd2345".into()),
            findings: findings
                .iter()
                .map(|finding| finding.finding_id.clone())
                .collect(),
            note: findings
                .is_empty()
                .then(|| "cannot establish progress".to_owned()),
        }],
        findings,
    }
}

fn state_with(projected: ClientShellSnapshot, config: ClientShellConfig) -> ClientShellState {
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(WIDTH, HEIGHT).unwrap();
    state
}

fn supervised(findings: Vec<ClientShellSupervisionFinding>) -> ClientShellState {
    let mut projected = snapshot();
    projected.supervision = Some(supervision(findings));
    projected.todo_runs = vec![ClientShellTodoRun {
        run_id: "r-abcd2345".into(),
        repo: "/home/u/herdr".into(),
        item: "t-aaaaaaaa".into(),
        step: "install".into(),
        status: "running".into(),
        attempt: 1,
        ..ClientShellTodoRun::default()
    }];
    state_with(projected, config_with_sidebar_width(40))
}

/// A snapshot composes only with the pane surface of its revision, so a
/// changed one keeps the revision here.
fn screen(state: &mut ClientShellState) -> Vec<String> {
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    frame_rows(&frame)
}

fn row_of(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(needle))
}

#[test]
fn the_row_is_pinned_above_the_spaces_with_its_counts() {
    let mut state = supervised(vec![
        finding("f-1", "critical"),
        finding("f-2", "warning"),
        finding("f-3", "warning"),
    ]);
    let rows = screen(&mut state);
    let row = state.hits.supervision_row;
    assert!(!row.is_empty(), "{}", rows.join("\n"));
    let text = &rows[usize::from(row.y)];
    assert!(text.contains("◆ Supervision"), "{text}");
    assert!(text.contains("◆ 1 ▲ 2"), "{text}");
    // Above the spaces' header and their list, outside their scroll.
    let space = row_of(&rows, "client-shell").expect("the space");
    assert!(usize::from(row.y) < space);
    assert!(row.y < state.hits.workspace_body.y);
    assert!(!super::super::contains(
        state.hits.workspace_body,
        (row.x + 2, row.y)
    ));

    // A sweep that changes the counts does not move the row's text.
    let at = text.find("◆ 1").unwrap();
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.supervision = Some(supervision(vec![finding("f-1", "critical")]));
    state.set_snapshot(Box::new(projected));
    let rows = screen(&mut state);
    let text = &rows[usize::from(state.hits.supervision_row.y)];
    assert_eq!(text.find("◆ 1"), Some(at), "{text}");
    assert!(text.contains("◆ 1 ▲ 0"), "{text}");
}

#[test]
fn no_finding_draws_no_all_clear_and_stale_or_failed_sweeps_say_so() {
    let mut state = supervised(Vec::new());
    let rows = screen(&mut state);
    let text = &rows[usize::from(state.hits.supervision_row.y)];
    assert!(text.contains("○ Supervision"), "{text}");
    assert!(
        !text.contains("stale") && !text.contains("unavailable"),
        "{text}"
    );

    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let mut stale = supervision(Vec::new());
    stale.swept_ms = Some(now() - 3 * interval_ms());
    projected.supervision = Some(stale);
    state.set_snapshot(Box::new(projected.clone()));
    let rows = screen(&mut state);
    let text = &rows[usize::from(state.hits.supervision_row.y)];
    assert!(text.contains("Supervision stale"), "{text}");

    let mut failed = supervision(Vec::new());
    failed.sweep_error = Some("database is locked".into());
    projected.supervision = Some(failed);
    state.set_snapshot(Box::new(projected.clone()));
    let rows = screen(&mut state);
    let text = &rows[usize::from(state.hits.supervision_row.y)];
    assert!(text.contains("Supervision unavailable"), "{text}");

    let mut unswept = supervision(Vec::new());
    unswept.swept_ms = None;
    projected.supervision = Some(unswept);
    state.set_snapshot(Box::new(projected));
    let rows = screen(&mut state);
    let text = &rows[usize::from(state.hits.supervision_row.y)];
    assert!(text.contains("Supervision no sweep yet"), "{text}");
}

#[test]
fn the_stale_moment_redraws_the_row() {
    let state = supervised(Vec::new());
    let swept = state
        .snapshot
        .as_deref()
        .and_then(|snapshot| snapshot.supervision.as_ref())
        .and_then(|supervision| supervision.swept_ms)
        .unwrap();
    let stale_at = swept + 2 * interval_ms();
    let change_at = state.worker_times_change_at.expect("a redraw moment");
    assert!(change_at <= stale_at);
    assert_eq!(state.supervision_times_change_at(swept), Some(stale_at));
}

#[test]
fn the_config_or_a_server_without_supervision_has_no_row() {
    // Where the row sits when it shows. The word alone is no evidence: the
    // sidebar's build line shows the commit subject, which may name the tab.
    let mut projected = snapshot();
    projected.supervision = Some(supervision(Vec::new()));
    let mut shown = state_with(projected.clone(), config_with_sidebar_width(40));
    let shown_rows = screen(&mut shown);
    let pinned = usize::from(shown.hits.supervision_row.y);
    assert!(shown_rows[pinned].contains("○ Supervision"));

    // Without the server's supervision the sidebar is drawn as before.
    let mut bare = state_with(snapshot(), config_with_sidebar_width(40));
    let bare_rows = screen(&mut bare);
    assert!(bare.hits.supervision_row.is_empty());
    assert!(
        !bare_rows[pinned].contains("○ Supervision"),
        "{}",
        bare_rows.join("\n")
    );

    // The config turns the row off: the same screen as without it.
    let mut config = config_with_sidebar_width(40);
    config.show_supervision_tab = false;
    let mut state = state_with(projected, config);
    let rows = screen(&mut state);
    assert!(state.hits.supervision_row.is_empty());
    assert_eq!(rows, bare_rows);
}

#[test]
fn a_click_shows_the_coordinators_and_findings_in_the_main_pane() {
    let mut state = supervised(vec![finding("f-1", "critical")]);
    let row = state.hits.supervision_row;
    let outcome = left_click(&mut state, (row.x + 3, row.y));
    assert!(outcome.repaint);
    assert!(state.supervision_view_shown());
    assert!(methods(&outcome).is_empty(), "the view asks nothing");
    let text = screen(&mut state).join("\n");
    assert!(
        text.contains("Supervision · 1 critical · 0 warning"),
        "{text}"
    );
    assert!(text.contains("read-only"), "{text}");
    assert!(text.contains("last sweep <1m ago (timer)"), "{text}");
    // The coordinators' table: repo, state, progress, run and its step,
    // workers, pane link.
    assert!(text.contains("REPO"), "{text}");
    assert!(text.contains("herdr"), "{text}");
    assert!(text.contains("idle"), "{text}");
    assert!(text.contains("findings f-1"), "{text}");
    assert!(text.contains("r-abcd2345 · install · running"), "{text}");
    assert!(text.contains("w7"), "{text}");
    assert!(text.contains("open pane pane_1"), "{text}");
    // The finding with its evidence.
    assert!(
        text.contains("◆ f-1 · critical · open · coordinator idle"),
        "{text}"
    );
    assert!(
        text.contains("expected: the coordinator starts the next item"),
        "{text}"
    );
    assert!(
        text.contains("event: pane pane_1 state change 41"),
        "{text}"
    );
    assert!(text.contains("agent_status: idle"), "{text}");
    // The focused tab's panes are not drawn under it, and get no input.
    assert!(!text.contains("LIVE"), "{text}");
    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty()),
    )]);
    assert!(outcome.requests.is_empty());
    assert!(methods(&outcome).is_empty());
}

#[test]
fn without_findings_the_view_says_progress_cannot_be_established() {
    let mut state = supervised(Vec::new());
    let row = state.hits.supervision_row;
    left_click(&mut state, (row.x + 3, row.y));
    let text = screen(&mut state).join("\n");
    assert!(
        text.contains("no findings (not a sign of progress)"),
        "{text}"
    );
    assert!(text.contains("cannot establish progress"), "{text}");
    assert!(text.contains("that does not mean progress"), "{text}");
}

#[test]
fn open_coordinator_pane_focuses_its_existing_tab() {
    let mut state = supervised(vec![finding("f-1", "warning")]);
    let row = state.hits.supervision_row;
    left_click(&mut state, (row.x + 3, row.y));
    screen(&mut state);
    let (link, pane_id) = state
        .hits
        .supervision_pane_links
        .first()
        .cloned()
        .expect("the pane link");
    assert_eq!(pane_id, "pane_1");
    let outcome = left_click(&mut state, (link.x + 1, link.y));
    assert!(methods(&outcome)
        .iter()
        .any(|method| matches!(method, Method::PaneFocus(target) if target.pane_id == "pane_1")));
    assert!(!state.supervision_view_shown());
}

#[test]
fn a_focus_change_leaves_the_view() {
    let mut state = supervised(Vec::new());
    let row = state.hits.supervision_row;
    left_click(&mut state, (row.x + 3, row.y));
    assert!(state.supervision_view_shown());
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.focused_tab_id = Some("tab_2".into());
    state.set_snapshot(Box::new(projected));
    assert!(!state.supervision_view_shown());
}

#[test]
fn a_findings_question_entry_opens_the_same_finding() {
    let mut state = supervised(vec![finding("f-1", "warning"), finding("f-2", "critical")]);
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.worker_questions = vec![crate::protocol::ClientShellWorkerQuestion {
        worker_id: "supervision-f-2".into(),
        request_id: "herdr-finding-f-2".into(),
        cwd: "/repo".into(),
        tool_name: "herdr supervise".into(),
        text: "f-2: idle".into(),
        choice: true,
        since_ms: 1_000,
        quiet: false,
        owner_seen_ms: None,
    }];
    state.set_snapshot(Box::new(projected));
    let mut outcome = ClientShellInput::default();
    state.open_worker_question(
        "supervision-f-2".into(),
        "herdr-finding-f-2".into(),
        &mut outcome,
    );
    let view = state.worker_question_view().expect("the dialog");
    assert!(view
        .buttons
        .iter()
        .any(|(button, _, enabled)| *button == WorkerQuestionButton::OpenSupervision && *enabled));
    let mut outcome = ClientShellInput::default();
    state.press_worker_question_button(WorkerQuestionButton::OpenSupervision, &mut outcome);
    assert!(state.overlay.is_none());
    assert!(state.supervision_view_shown());
    assert_eq!(
        state
            .supervision_view
            .as_ref()
            .and_then(|view| view.highlight.as_deref()),
        Some("f-2")
    );

    // A worker's own question has no such link.
    let mut outcome = ClientShellInput::default();
    state.open_worker_question("w1".into(), "r1".into(), &mut outcome);
    let view = state.worker_question_view().expect("the dialog");
    assert!(!view
        .buttons
        .iter()
        .any(|(button, _, _)| *button == WorkerQuestionButton::OpenSupervision));
}
