//! A headless worker's tab, driven through the client state: it opens from
//! the worker's line, shows the pushed transcript in the main pane, follows
//! it only at the bottom, takes no input, and gives way to any other tab.

use super::*;
use crate::api::schema::{
    Method, PaneKind, WorkerState, WorkerTab, WorkerTranscript, WorkerTranscriptEntry,
    WorkerTranscriptEvent, WorkerTranscriptRole,
};
use crate::protocol::endpoint::EndpointWorkerTranscript;

const WIDTH: u16 = 106;
const HEIGHT: u16 = 30;

fn mouse(state: &mut ClientShellState, kind: MouseEventKind, (column, row): (u16, u16)) {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })]);
}

fn left_click(state: &mut ClientShellState, (column, row): (u16, u16)) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn worker(worker_id: &str) -> crate::protocol::ClientShellWorker {
    crate::protocol::ClientShellWorker {
        worker_id: worker_id.into(),
        workspace_id: Some("ws_1".into()),
        name: "fix login".into(),
        cwd: "/repo".into(),
        state: "working".into(),
        session_id: Some("session-1".into()),
        takeover: false,
        ..Default::default()
    }
}

/// A client with worker w1 under its space.
fn state_with_worker() -> ClientShellState {
    let mut config = config_with_sidebar_width(26);
    config.spaces.tabs = true;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    projected.workers = vec![worker("w1")];
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(WIDTH, HEIGHT).unwrap();
    state
}

fn worker_line(state: &ClientShellState, worker_id: &str) -> Rect {
    state
        .hits
        .space_tabs
        .iter()
        .find(|(_, id)| super::super::space_tabs::worker_line_worker(id) == Some(worker_id))
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("no line for {worker_id}: {:?}", state.hits.space_tabs))
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

fn transcript_set(outcome: &ClientShellInput) -> Vec<(Option<String>, u64)> {
    methods(outcome)
        .into_iter()
        .filter_map(|method| match method {
            Method::ClientShellWorkerTranscriptSet(params) => {
                Some((params.worker_id, params.after))
            }
            _ => None,
        })
        .collect()
}

fn open(state: &mut ClientShellState) -> ClientShellInput {
    let line = worker_line(state, "w1");
    let outcome = left_click(state, (line.x + 6, line.y));
    state.compose(WIDTH, HEIGHT).unwrap();
    outcome
}

fn event(line: u64, entry: WorkerTranscriptEntry) -> WorkerTranscriptEvent {
    WorkerTranscriptEvent {
        worker_id: "w1".into(),
        run_id: Some("r-abc".into()),
        line,
        seq: Some(line as i64),
        ts_ms: line,
        entry,
    }
}

fn status(line: u64, text: &str) -> WorkerTranscriptEvent {
    event(
        line,
        WorkerTranscriptEntry::Status {
            text: text.to_owned(),
        },
    )
}

fn push(state: &mut ClientShellState, events: Vec<WorkerTranscriptEvent>, cursor: u64) -> bool {
    state.receive_worker_transcript(
        &ClientEndpointId::Local,
        EndpointWorkerTranscript {
            boot_id: "boot-1".into(),
            transcript: WorkerTranscript {
                worker_id: "w1".into(),
                run_id: Some("r-abc".into()),
                tab: WorkerTab {
                    tab_id: "worker:w1".into(),
                    pane_id: "worker:w1".into(),
                    pane_kind: PaneKind::Worker,
                    workspace_id: Some("ws_1".into()),
                    read_only: true,
                    takeover_tab_id: None,
                },
                state: WorkerState::Working,
                events,
                cursor,
                more: false,
            },
        },
    )
}

fn shown_rows(state: &mut ClientShellState) -> Vec<String> {
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let body = state.hits.worker_view_body;
    frame_rows(&frame)
        .into_iter()
        .skip(usize::from(body.y))
        .take(usize::from(body.height))
        .map(|row| {
            row.chars()
                .skip(usize::from(body.x))
                .take(usize::from(body.width))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn many(from: u64, count: u64) -> Vec<WorkerTranscriptEvent> {
    (from..from + count)
        .map(|line| status(line, &format!("■ line {line}")))
        .collect()
}

#[test]
fn a_worker_line_opens_its_tab_in_the_main_pane_not_a_modal() {
    let mut state = state_with_worker();
    let outcome = open(&mut state);
    assert!(state.overlay.is_none(), "no modal");
    assert_eq!(transcript_set(&outcome), vec![(Some("w1".into()), 0)]);
    assert!(
        !methods(&outcome)
            .iter()
            .any(|method| matches!(method, Method::WorkerOpenLog(_))),
        "no log popup"
    );
    let layout = state.layout(WIDTH, HEIGHT);
    assert_eq!(state.hits.worker_view, layout.pane_surface);
    // The focused tab's panes are neither drawn nor hit under it.
    assert!(state.hits.panes.is_empty());
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let rows = frame_rows(&frame);
    let header = &rows[usize::from(layout.pane_surface.y)];
    assert!(header.contains("⚒ working · fix login"), "{header}");
    assert!(header.contains("read-only"), "{header}");
    let details = &rows[usize::from(layout.pane_surface.y) + 1];
    assert!(details.contains("worker w1 · 0 turns"), "{details}");
    assert!(!rows.iter().any(|row| row.contains("LIVE")), "{rows:#?}");
    // Its line is the active one.
    let snapshot = state.snapshot.as_deref().unwrap();
    let mut lines = super::super::space_tabs::worker_lines(snapshot, &snapshot.workspaces[0]);
    super::super::space_tabs::mark_shown_worker(&mut lines, state.shown_worker_id());
    assert!(lines.iter().all(|line| line.active));
}

#[test]
fn the_pushed_transcript_shows_the_lines_worker_log_prints() {
    let mut state = state_with_worker();
    open(&mut state);
    let events = vec![
        event(
            1,
            WorkerTranscriptEntry::Message {
                role: WorkerTranscriptRole::User,
                text: "fix it".into(),
            },
        ),
        event(
            2,
            WorkerTranscriptEntry::ToolCall {
                name: "Bash".into(),
                input: "git status".into(),
            },
        ),
        event(
            3,
            WorkerTranscriptEntry::ToolResult {
                text: "clean".into(),
                is_error: false,
            },
        ),
    ];
    let expected: Vec<String> = events
        .iter()
        .flat_map(|event| crate::workers::entry_lines(&event.entry))
        .collect();
    assert!(push(&mut state, events.clone(), 3));
    let rows = shown_rows(&mut state);
    let shown: Vec<&String> = rows.iter().filter(|row| !row.is_empty()).collect();
    assert_eq!(shown, expected.iter().collect::<Vec<_>>());
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let details = &frame_rows(&frame)[usize::from(state.hits.worker_view.y) + 1];
    assert!(details.contains("r-abc"), "the run id: {details}");

    // A resend after a reconnect repeats nothing.
    assert!(!push(&mut state, events, 3));
    assert_eq!(shown_rows(&mut state), rows);
}

#[test]
fn the_tail_follows_only_at_the_bottom_with_a_new_output_mark() {
    let mut state = state_with_worker();
    open(&mut state);
    push(&mut state, many(1, 60), 60);
    let rows = shown_rows(&mut state);
    assert!(rows.iter().any(|row| row == "■ line 60"), "at the tail");

    // Scrolled up, new lines keep the view where it is and show a mark.
    let body = state.hits.worker_view_body;
    mouse(
        &mut state,
        MouseEventKind::ScrollUp,
        (body.x + 2, body.y + 2),
    );
    let before = shown_rows(&mut state);
    assert!(!before.iter().any(|row| row == "■ line 60"));
    push(&mut state, many(61, 5), 65);
    let after = shown_rows(&mut state);
    assert_eq!(after[..after.len() - 1], before[..before.len() - 1]);
    assert!(
        after.last().is_some_and(|row| row.contains("new output")),
        "{after:#?}"
    );

    // Back at the bottom it follows again, and the mark is gone.
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::End,
        KeyModifiers::empty(),
    ))]);
    let rows = shown_rows(&mut state);
    assert!(rows.iter().any(|row| row == "■ line 65"));
    assert!(!rows.iter().any(|row| row.contains("new output")));
    push(&mut state, many(66, 1), 66);
    assert!(shown_rows(&mut state).iter().any(|row| row == "■ line 66"));
}

#[test]
fn the_tab_takes_no_input() {
    let mut state = state_with_worker();
    open(&mut state);
    push(&mut state, many(1, 60), 60);
    let typed = state.handle_raw_events(vec![
        RawInputEvent::Text(crate::input::TextCommit::new("ls\r")),
        RawInputEvent::Paste("rm -rf".into()),
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        )),
        RawInputEvent::Key(crate::input::TerminalKey::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )),
    ]);
    assert!(
        !typed
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })),
        "{:?}",
        typed.requests
    );
    // Its keys scroll it instead.
    let before = shown_rows(&mut state);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::PageUp,
        KeyModifiers::empty(),
    ))]);
    assert_ne!(shown_rows(&mut state), before);
}

#[test]
fn a_drag_selects_and_its_release_copies() {
    let mut state = state_with_worker();
    open(&mut state);
    push(
        &mut state,
        vec![status(1, "■ first line"), status(2, "■ second line")],
        2,
    );
    shown_rows(&mut state);
    let body = state.hits.worker_view_body;
    let rows = shown_rows(&mut state);
    let first = rows.iter().position(|row| row == "■ first line").unwrap() as u16;
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        (body.x + 2, body.y + first),
    );
    mouse(
        &mut state,
        MouseEventKind::Drag(MouseButton::Left),
        (body.x + 7, body.y + first + 1),
    );
    let released = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: body.x + 7,
        row: body.y + first + 1,
        modifiers: KeyModifiers::empty(),
    })]);
    let copied: Vec<String> = released
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::ClipboardWrite(bytes) => {
                Some(String::from_utf8(bytes.clone()).unwrap())
            }
            _ => None,
        })
        .collect();
    assert_eq!(copied, vec!["first line\n■ second".to_owned()]);
}

#[test]
fn another_tab_or_a_focus_change_leaves_the_worker_tab() {
    let mut state = state_with_worker();
    open(&mut state);
    // Clicking a tab focuses it and stops the transcript.
    let tab = state
        .hits
        .space_tabs
        .iter()
        .find(|(_, id)| id.as_str() == "tab_1")
        .map(|(rect, _)| *rect)
        .expect("the tab's line");
    left_click(&mut state, (tab.x + 6, tab.y));
    let released = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: tab.x + 6,
        row: tab.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.worker_view.is_none());
    assert_eq!(transcript_set(&released), vec![(None, 0)]);
    assert!(methods(&released)
        .iter()
        .any(|method| matches!(method, Method::TabFocus(target) if target.tab_id == "tab_1")));
    state.compose(WIDTH, HEIGHT).unwrap();
    assert!(!state.hits.panes.is_empty(), "the tab's panes are back");

    // The server focusing another tab (a takeover's) leaves it too.
    open(&mut state);
    assert!(state.worker_view.is_some());
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.revision += 1;
    projected.focused_tab_id = Some("tab_2".into());
    state.set_snapshot(Box::new(projected));
    assert!(state.worker_view.is_none());
}

#[test]
fn a_reconnect_asks_the_new_connection_from_the_view_s_cursor() {
    let mut state = state_with_worker();
    open(&mut state);
    push(&mut state, many(1, 3), 3);
    assert!(
        state.worker_view_resubscription().is_empty(),
        "asked already"
    );
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.boot_id = "boot-2".into();
    state.set_snapshot(Box::new(projected));
    let actions = state.worker_view_resubscription();
    let asked: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::ClientShellWorkerTranscriptSet(params) => {
                    Some((params.worker_id.clone(), params.after))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(asked, vec![(Some("w1".into()), 3)]);
}

#[test]
fn a_server_without_worker_tabs_opens_the_log_popup() {
    let mut state = state_with_worker();
    state.set_endpoint_methods(Some(vec!["worker.open_log".into()]));
    let outcome = open(&mut state);
    assert!(state.worker_view.is_none());
    assert!(methods(&outcome)
        .iter()
        .any(|method| matches!(method, Method::WorkerOpenLog(target) if target.worker_id == "w1")));
}

#[test]
fn worktree_dialogs_and_release_notes_close_on_a_click_outside() {
    let mut state = state_with_worker();
    state.overlay = Some(ClientShellOverlay::WorktreeCreate(
        ClientWorktreeCreateOverlay {
            source_workspace_id: "ws_1".into(),
            repo_name: "repo".into(),
            branch: "branch".into(),
            checkout_path: "path".into(),
            error: None,
            creating: false,
        },
    ));
    state.compose(WIDTH, HEIGHT).unwrap();
    let area = state.hits.overlay_area;
    assert!(!area.is_empty());
    let outcome = left_click(&mut state, (0, 0));
    assert!(state.overlay.is_none());
    assert!(
        outcome.actions.is_empty(),
        "the click acts on nothing under it"
    );

    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.release_notes = Some(crate::protocol::ClientShellReleaseNotes {
        version: "0.8.3".into(),
        body: "- a change".into(),
        preview: false,
    });
    state.set_snapshot(Box::new(projected));
    state.overlay = Some(ClientShellOverlay::ReleaseNotes(
        crate::app::state::ReleaseNotesState {
            version: "0.8.3".into(),
            body: "- a change".into(),
            scroll: 0,
            preview: false,
        },
    ));
    state.compose(WIDTH, HEIGHT).unwrap();
    let outcome = left_click(&mut state, (0, 0));
    assert!(state.overlay.is_none());
    assert!(matches!(
        methods(&outcome).as_slice(),
        [Method::ReleaseNotesDismiss(_)]
    ));
}

/// Worker w1 working on an item for 12 minutes, its turn 3 of them, its
/// last event a minute ago, as of `now`.
fn timed_worker(now: u64) -> crate::protocol::ClientShellWorker {
    crate::protocol::ClientShellWorker {
        item: Some("t-abcd2345".into()),
        item_title: Some("A worker's line shows its age and its todo item".into()),
        slot: Some("worker".into()),
        branch: Some("todo/t-abcd2345-1".into()),
        turns: 1,
        started_ms: Some(now - 12 * 60_000 - 30_000),
        turn_started_ms: Some(now - 3 * 60_000 - 30_000),
        last_event_ms: Some(now - 60_000 - 30_000),
        ..worker("w1")
    }
}

fn state_with_timed_worker() -> ClientShellState {
    let mut state = state_with_worker();
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.workers = vec![timed_worker(super::super::workers::unix_now_ms())];
    state.set_snapshot(Box::new(projected));
    state.compose(WIDTH, HEIGHT).unwrap();
    state
}

#[test]
fn a_worker_line_shows_its_items_title_and_its_age_at_the_right() {
    let now = 100 * 60 * 60_000;
    let mut projected = snapshot();
    projected.workers = vec![timed_worker(now)];
    let lines =
        super::super::space_tabs::worker_lines_at(&projected, &projected.workspaces[0], now);
    let [line] = lines.as_slice() else {
        panic!("one line: {lines:?}");
    };
    assert_eq!(
        line.label,
        "A worker's line shows its age and its todo item"
    );
    assert_eq!(line.plan.as_deref(), Some("12m"));
    let tooltip = &line.worker.as_ref().expect("a worker line").tooltip;
    assert!(
        tooltip.starts_with(
            "A worker's line shows its age and its todo item · working · t-abcd2345 \
             · worker w1 (fix login) · 1 turn · this turn 3m · last activity 1m ago \
             · age 12m · branch todo/t-abcd2345-1 · slot worker"
        ),
        "{tooltip}"
    );

    // An ended worker keeps its word before its age, which stopped.
    projected.workers[0].state = "exited".into();
    let lines = super::super::space_tabs::worker_lines_at(
        &projected,
        &projected.workspaces[0],
        now + 60 * 60_000,
    );
    assert_eq!(lines[0].plan.as_deref(), Some("exited 11m"));

    // Drawn: the title cut with an ellipsis, the age at the line's end.
    let mut state = state_with_timed_worker();
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let line = worker_line(&state, "w1");
    let row = &frame_rows(&frame)[usize::from(line.y)];
    let sidebar: String = row.chars().take(usize::from(line.right())).collect();
    assert!(sidebar.contains("A worker's"), "{sidebar}");
    assert!(sidebar.contains('…'), "{sidebar}");
    assert!(sidebar.trim_end().ends_with("12m"), "{sidebar}");
}

#[test]
fn a_frame_is_drawn_again_when_a_shown_worker_time_changes() {
    let mut state = state_with_timed_worker();
    let change_at = state
        .worker_times_change_at
        .expect("a moment a shown time changes");
    let now = super::super::workers::unix_now_ms();
    // The first change is the last activity's, under a minute away.
    assert!(
        change_at > now && change_at <= now + 60_000,
        "{change_at} {now}"
    );
    assert!(!state.tick_worker_times_at(change_at - 1));
    assert!(state.tick_worker_times_at(change_at));
    // Once: the next frame sets the next moment.
    assert!(!state.tick_worker_times_at(change_at));

    // No worker time shown: no redraw.
    let mut state = state_with_worker();
    assert_eq!(state.worker_times_change_at, None);
    assert!(!state.tick_worker_times_at(u64::MAX));
}

#[test]
fn the_tabs_header_names_the_item_and_details_and_offers_a_takeover() {
    let mut state = state_with_timed_worker();
    open(&mut state);
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let rows = frame_rows(&frame);
    let top = usize::from(state.hits.worker_view.y);
    let header = &rows[top];
    assert!(
        header.contains("⚒ working · A worker's line shows its age"),
        "{header}"
    );
    assert!(header.contains("take over"), "{header}");
    // Wide enough for every detail.
    let frame = state.compose(220, HEIGHT).unwrap();
    let details = &frame_rows(&frame)[top + 1];
    for part in [
        "t-abcd2345",
        "worker w1 (fix login)",
        "branch todo/t-abcd2345-1",
        "slot worker",
        "1 turn",
        "this turn 3m",
        "last activity 1m ago",
        "age 12m",
    ] {
        assert!(details.contains(part), "{part}: {details}");
    }

    // A click on it takes the worker over.
    let button = state.hits.worker_view_take_over;
    assert!(button.width > 0);
    let outcome = left_click(&mut state, (button.x + 1, button.y));
    assert!(
        methods(&outcome).iter().any(|method| matches!(method,
            Method::WorkerTakeOver(target) if target.worker_id == "w1")),
        "{:?}",
        methods(&outcome)
    );

    // Not offered while the worker asks.
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.workers[0].state = "waiting_approval".into();
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    assert_eq!(state.hits.worker_view_take_over, Rect::default());
    assert!(!frame_rows(&frame)[top].contains("take over"));
}

#[test]
fn a_worker_lost_between_turns_reads_finished_ended_by_the_restart() {
    let now = 100 * 60 * 60_000;
    let mut projected = snapshot();
    projected.workers = vec![crate::protocol::ClientShellWorker {
        state: "finished".into(),
        gone: true,
        end_note: Some("ended by a server restart".into()),
        ..timed_worker(now)
    }];
    let lines = super::super::space_tabs::worker_lines_at(
        &projected,
        &projected.workspaces[0],
        now + 60 * 60_000,
    );
    let [line] = lines.as_slice() else {
        panic!("one line: {lines:?}");
    };
    // Its age stopped at its last event, and it says it ended.
    assert_eq!(line.plan.as_deref(), Some("ended 11m"));
    let worker = line.worker.as_ref().expect("a worker line");
    assert!(
        worker
            .tooltip
            .contains("· finished, ended by a server restart ·"),
        "{}",
        worker.tooltip
    );
    assert!(!worker.tooltip.contains("lost"), "{}", worker.tooltip);
    assert!(!worker.tooltip.contains("this turn"), "{}", worker.tooltip);
    assert_eq!(worker.state, "finished");
}

#[test]
fn each_worker_state_draws_its_own_glyph() {
    use crate::config::StatusIndicatorStyle;
    for style in [
        StatusIndicatorStyle::Dots,
        StatusIndicatorStyle::Symbols,
        StatusIndicatorStyle::Shapes,
    ] {
        let glyphs = super::super::status_legend::WORKER_STATES
            .iter()
            // Starting is working that has not begun yet: the same glyph.
            .filter(|state| **state != "starting")
            .map(|state| super::super::space_tabs::worker_icon(state, style))
            .collect::<Vec<_>>();
        let distinct = glyphs.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(distinct.len(), glyphs.len(), "{style:?}: {glyphs:?}");
    }

    // Drawn on the line: a finished worker's check mark.
    let mut state = state_with_worker();
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.workers[0].state = "finished".into();
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let line = worker_line(&state, "w1");
    let row = &frame_rows(&frame)[usize::from(line.y)];
    let sidebar: String = row.chars().take(usize::from(line.right())).collect();
    assert!(sidebar.contains('✓'), "{sidebar}");
}

/// The ids of the fold hits drawn now.
fn fold_hits(state: &ClientShellState) -> Vec<String> {
    state
        .hits
        .space_tab_folds
        .iter()
        .map(|(_, id)| id.clone())
        .collect()
}

fn has_worker_line(state: &ClientShellState, worker_id: &str) -> bool {
    state
        .hits
        .space_tabs
        .iter()
        .any(|(_, id)| super::super::space_tabs::worker_line_worker(id) == Some(worker_id))
}

#[test]
fn an_owners_ended_workers_collapse_into_one_line_that_a_click_unfolds() {
    let mut state = state_with_worker();
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let ended = |worker_id: &str| crate::protocol::ClientShellWorker {
        state: "exited".into(),
        gone: true,
        owner_pane_id: Some("pane_1".into()),
        ..worker(worker_id)
    };
    projected.workers = vec![ended("w1"), worker("w2"), ended("w3")];
    state.set_snapshot(Box::new(projected));
    state.compose(WIDTH, HEIGHT).unwrap();
    let fold = super::super::space_tabs::ended_fold_id("ws_1", Some("tab_1"));
    assert!(fold_hits(&state).contains(&fold), "{:?}", fold_hits(&state));
    assert!(!has_worker_line(&state, "w1"));
    assert!(!has_worker_line(&state, "w3"));
    assert!(has_worker_line(&state, "w2"));
    let rect = state
        .hits
        .space_tab_folds
        .iter()
        .find(|(_, id)| *id == fold)
        .map(|(rect, _)| *rect)
        .unwrap();
    let frame = state.compose(WIDTH, HEIGHT).unwrap();
    let row = &frame_rows(&frame)[usize::from(rect.y)];
    assert!(row.contains("2 ended workers"), "{row}");

    // A click unfolds them under the line, which stays to fold them again.
    left_click(&mut state, (rect.x + 4, rect.y));
    state.compose(WIDTH, HEIGHT).unwrap();
    assert!(has_worker_line(&state, "w1"));
    assert!(has_worker_line(&state, "w3"));
    assert!(fold_hits(&state).contains(&fold));
    left_click(&mut state, (rect.x + 4, rect.y));
    state.compose(WIDTH, HEIGHT).unwrap();
    assert!(!has_worker_line(&state, "w1"));
    // Unfolded or not, it is not saved with the unfolded tab lines.
    assert!(!state.saved_job_folds().0.contains(&fold));

    // A single ended worker keeps its own line.
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.workers.retain(|worker| worker.worker_id != "w3");
    state.set_snapshot(Box::new(projected));
    state.compose(WIDTH, HEIGHT).unwrap();
    assert!(has_worker_line(&state, "w1"));
    assert!(!fold_hits(&state).contains(&fold));
}

#[test]
fn the_fold_button_puts_quiet_worker_lines_away_but_not_busy_ones() {
    let mut state = state_with_worker();
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.workers = vec![
        crate::protocol::ClientShellWorker {
            state: "finished".into(),
            quiet: true,
            ..worker("w1")
        },
        worker("w2"),
    ];
    state.set_snapshot(Box::new(projected));
    let quiet = super::super::space_tabs::quiet_tab_ids(state.snapshot.as_deref().unwrap());
    assert!(quiet.contains(&super::super::space_tabs::worker_line_id("w1")));
    assert!(!quiet.contains(&super::super::space_tabs::worker_line_id("w2")));

    state.toggle_quiet_folds();
    state.compose(WIDTH, HEIGHT).unwrap();
    assert!(!has_worker_line(&state, "w1"));
    assert!(has_worker_line(&state, "w2"));
    assert!(fold_hits(&state).contains(&super::super::space_tabs::quiet_fold_id("ws_1")));

    // Once it has something to say again, its line is back.
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.workers[0].state = "working".into();
    projected.workers[0].quiet = false;
    state.set_snapshot(Box::new(projected));
    state.compose(WIDTH, HEIGHT).unwrap();
    assert!(has_worker_line(&state, "w1"));
}
