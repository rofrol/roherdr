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
    assert!(header.contains("fix login · w1 · working"), "{header}");
    assert!(header.contains("read-only"), "{header}");
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
    let header = &frame_rows(&frame)[usize::from(state.hits.worker_view.y)];
    assert!(header.contains("r-abc"), "the run id: {header}");

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
