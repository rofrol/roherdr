use super::*;
use crate::api::schema::Method;
use crossterm::event::{MouseButton, MouseEventKind};

fn close_state(confirm: bool, tab_count: usize) -> ClientShellState {
    let mut projected = snapshot();
    for number in 2..=tab_count {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.focused = false;
        projected.tabs.push(tab);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.confirm_close = confirm;
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).unwrap();
    state
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn request_close(state: &mut ClientShellState, menu: bool) -> ClientShellInput {
    if menu {
        state.open_tab_context_menu("tab_1".into(), 30, 1);
        state.compose(106, 24).unwrap();
        // The Close row, wherever the menu's other items put it.
        let close_index = match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu
                .items()
                .iter()
                .position(|item| item.action == ClientContextMenuAction::Close)
                .expect("close item"),
            _ => panic!("menu open"),
        };
        let close_row = state.hits.context_menu_rows[close_index].0;
        click(state, close_row)
    } else {
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseTab),
            &mut outcome,
        );
        outcome
    }
}

fn assert_no_close(outcome: &ClientShellInput) {
    assert!(outcome.requests.is_empty());
    assert!(outcome.actions.iter().all(|action| {
        !matches!(action, ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, Method::TabClose(_) | Method::WorkspaceClose(_)))
    }));
}

fn assert_tab_close(outcome: &ClientShellInput) {
    let methods = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .filter(|method| !matches!(method, Method::TabFocus(_)))
        .collect::<Vec<_>>();
    assert!(matches!(methods.as_slice(), [Method::TabClose(target)] if target.tab_id == "tab_1"));
}

#[test]
fn last_tab_close_waits_for_keyboard_or_mouse_confirmation() {
    for menu in [false, true] {
        let mut state = close_state(true, 1);
        let requested = request_close(&mut state, menu);
        assert_no_close(&requested);
        assert!(requested.repaint);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmClose(_))
        ));
        let frame = state.compose(106, 24).unwrap();
        let text = frame_rows(&frame).join("\n");
        assert!(text.contains("Close workspace?"));
        assert!(text.contains("1 pane"));
        let accepted = if menu {
            let primary = state.hits.overlay_primary;
            click(&mut state, primary)
        } else {
            state.handle_input_bytes(b"\r")
        };
        assert_tab_close(&accepted);
        assert!(state.overlay.is_none());
    }
}

#[test]
fn last_tab_close_confirmation_can_be_cancelled() {
    for mouse in [false, true] {
        let mut state = close_state(true, 1);
        assert_no_close(&request_close(&mut state, false));
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmClose(_))
        ));
        state.compose(106, 24).unwrap();
        let cancelled = if mouse {
            let cancel = state.hits.overlay_cancel;
            click(&mut state, cancel)
        } else {
            state.handle_input_bytes(b"\x1b")
        };
        assert_no_close(&cancelled);
        assert!(state.overlay.is_none());
        // Cancelling must not leave the space selected as in navigate mode.
        assert_ne!(state.mode, ClientShellMode::Navigate, "mouse: {mouse}");
        assert!(state.navigate_workspace_id.is_none(), "mouse: {mouse}");
    }
}

#[test]
fn tab_close_stays_immediate_with_confirmation_disabled_or_other_tabs() {
    for (confirm, tabs) in [(false, 1), (false, 2), (true, 2)] {
        for menu in [false, true] {
            let mut state = close_state(confirm, tabs);
            assert_tab_close(&request_close(&mut state, menu));
            assert!(state.overlay.is_none());
        }
    }
}

#[test]
fn last_tab_confirmation_preserves_target_across_focus_changes_and_new_tabs() {
    let mut state = close_state(true, 1);
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let mut other_workspace = projected.workspaces[0].clone();
    other_workspace.workspace_id = "ws_2".into();
    other_workspace.active_tab_id = "other_tab".into();
    other_workspace.focused = false;
    projected.workspaces.push(other_workspace);
    let mut other_tab = projected.tabs[0].clone();
    other_tab.workspace_id = "ws_2".into();
    other_tab.tab_id = "other_tab".into();
    other_tab.focused = false;
    projected.tabs.push(other_tab);
    state.set_snapshot(Box::new(projected.clone()));

    assert_no_close(&request_close(&mut state, false));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(_))
    ));
    let mut new_tab = projected.tabs[0].clone();
    new_tab.tab_id = "new_tab".into();
    new_tab.focused = false;
    projected.tabs.push(new_tab);
    projected.focused_workspace_id = Some("ws_2".into());
    projected.focused_tab_id = Some("other_tab".into());
    for workspace in &mut projected.workspaces {
        workspace.focused = workspace.workspace_id == "ws_2";
    }
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "other_tab";
    }
    state.set_snapshot(Box::new(projected));
    assert_tab_close(&state.handle_input_bytes(b"\r"));
}

#[test]
fn last_tab_confirmation_rejects_missing_moved_or_reconnected_targets() {
    for change in ["missing", "moved", "reconnected"] {
        let mut state = close_state(true, 1);
        assert_no_close(&request_close(&mut state, false));
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::ConfirmClose(_))
        ));
        let mut projected = state.snapshot.as_deref().unwrap().clone();
        match change {
            "missing" => projected.tabs.clear(),
            "moved" => projected.tabs[0].workspace_id = "different_workspace".into(),
            "reconnected" => state.endpoints[0].snapshot_generation = Some(2),
            _ => unreachable!(),
        }
        if change != "reconnected" {
            state.set_snapshot(Box::new(projected));
        }
        assert_no_close(&state.handle_input_bytes(b"\r"));
        assert!(state.overlay.is_none());
    }
}

#[test]
fn last_tab_close_preserves_parent_group_and_linked_workspace_scope() {
    for linked in [false, true] {
        let mut state = close_state(true, 1);
        let mut projected = state.snapshot.as_deref().unwrap().clone();
        projected.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: linked,
        });
        let mut sibling = projected.workspaces[0].clone();
        sibling.workspace_id = "ws_2".into();
        sibling.active_tab_id = "tab_2".into();
        sibling.focused = false;
        sibling.worktree.as_mut().unwrap().is_linked_worktree = !linked;
        projected.workspaces.push(sibling);
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = "tab_2".into();
        tab.workspace_id = "ws_2".into();
        tab.focused = false;
        projected.tabs.push(tab);
        state.set_snapshot(Box::new(projected));

        let close = request_close(&mut state, false);
        if linked {
            assert_no_close(&close);
            assert!(matches!(
                state.overlay,
                Some(ClientShellOverlay::ConfirmClose(_))
            ));
            assert_tab_close(&state.handle_input_bytes(b"\r"));
        } else {
            assert_tab_close(&close);
            assert!(state.overlay.is_none());
            let [ClientShellAction::Endpoint { request, .. }] = close.actions.as_slice() else {
                panic!("tab close request");
            };
            state.handle_endpoint_result(
                "boot-1",
                &request.id,
                Err(ClientShellEndpointError {
                    code: Some("confirmation_required".into()),
                    message: "closing this tab would close a worktree group".into(),
                }),
            );
            assert!(matches!(state.overlay.as_ref(),
                Some(ClientShellOverlay::ConfirmClose(confirm)) if confirm.title == "Close worktree group?"));
            let accepted = state.handle_input_bytes(b"\r");
            assert!(matches!(accepted.actions.as_slice(),
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, Method::WorkspaceClose(params)
                        if params.workspace_id == "ws_1" && params.close_group)));
        }
    }
}

fn parent_with_jobs_state(confirm: bool) -> ClientShellState {
    let mut state = close_state(confirm, 3);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for (tab, (label, status)) in projected.tabs[1..].iter_mut().zip([
        ("build", crate::api::schema::TabStatus::Failed),
        ("tests", crate::api::schema::TabStatus::Running),
    ]) {
        tab.parent_tab_id = Some("tab_1".into());
        tab.status = Some(status);
        tab.label = label.into();
        tab.custom_label = true;
    }
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24).unwrap();
    state
}

fn tab_closes(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::TabClose(target) => Some(target.tab_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn child_tabs_get_their_own_row_and_a_summary_on_the_parent() {
    let mut state = parent_with_jobs_state(true);
    let frame = state.compose(106, 24).unwrap();
    let rows = frame_rows(&frame);

    assert_eq!(state.hits.tabs.len(), 1, "children leave the main row");
    assert!(rows[0].contains("1 ◐ 1 !1"), "{}", rows[0]);
    assert!(
        rows[1].contains("◆ ") && rows[1].contains("│ ! build") && rows[1].contains("│ ◐ tests"),
        "{}",
        rows[1]
    );
    assert_eq!(
        state
            .hits
            .child_tabs
            .iter()
            .map(|(_, id)| id.as_str())
            .collect::<Vec<_>>(),
        ["tab_1", "tab_2", "tab_3"],
        "the parent's own entry comes first"
    );
    assert_eq!(state.layout(106, 24).pane_surface.y, 2);
}

#[test]
fn job_counts_and_icons_use_the_sidebar_status_colors() {
    let mut state = parent_with_jobs_state(true);
    let frame = state.compose(106, 24).unwrap();
    let buffer = frame.to_ratatui_buffer().expect("tab bar buffer");
    let palette = &state.config.palette;
    let fg = |rect: Rect, symbol: &str| {
        (rect.x..rect.right())
            .map(|x| &buffer[(x, rect.y)])
            .find(|cell| cell.symbol() == symbol)
            .map(|cell| cell.fg)
    };

    let parent = state.hits.tabs[0].0;
    assert_eq!(fg(parent, "◐"), Some(palette.yellow));
    assert_eq!(fg(parent, "!"), Some(palette.red));
    let children = &state.hits.child_tabs;
    assert_eq!(fg(children[1].0, "!"), Some(palette.red));
    assert_eq!(fg(children[2].0, "◐"), Some(palette.yellow));
}

#[test]
fn only_the_entry_on_screen_gets_the_full_accent() {
    let mut state = parent_with_jobs_state(true);
    let accent = state.config.palette.accent;
    let backgrounds = |state: &mut ClientShellState| {
        let frame = state.compose(106, 24).unwrap();
        let buffer = frame.to_ratatui_buffer().expect("tab bar buffer");
        let bg = |rect: Rect| buffer[(rect.x + 1, rect.y)].bg;
        let main = bg(state.hits.tabs[0].0);
        let children = state
            .hits
            .child_tabs
            .iter()
            .map(|(rect, _)| bg(*rect))
            .collect::<Vec<_>>();
        (main, children)
    };

    let (main, children) = backgrounds(&mut state);
    assert_ne!(main, accent, "a parent with children is only tinted");
    assert_eq!(children, [accent, children[1], children[1]]);
    assert_ne!(children[1], accent);

    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "tab_3";
    }
    projected.focused_tab_id = Some("tab_3".into());
    state.set_snapshot(Box::new(projected));
    let (main_on_child, children) = backgrounds(&mut state);
    assert_eq!(main_on_child, main);
    assert_eq!(children, [children[0], children[0], accent]);
    assert_ne!(children[0], accent);
}

#[test]
fn a_tab_without_children_keeps_the_full_accent() {
    let mut state = close_state(true, 2);
    let frame = state.compose(106, 24).unwrap();
    let buffer = frame.to_ratatui_buffer().expect("tab bar buffer");
    let rect = state.hits.tabs[0].0;
    assert_eq!(buffer[(rect.x + 1, rect.y)].bg, state.config.palette.accent);
}

#[test]
fn the_child_row_stays_while_the_workspace_has_child_tabs() {
    let mut state = parent_with_jobs_state(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_4".into();
    other.label = "lazygit".into();
    projected.tabs.push(other);
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "tab_4";
    }
    projected.focused_tab_id = Some("tab_4".into());
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24).unwrap();

    assert_eq!(
        state.layout(106, 24).pane_surface.y,
        2,
        "no resize on switching"
    );
    assert_eq!(
        state
            .hits
            .child_tabs
            .iter()
            .map(|(_, id)| id.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new(),
        "a tab without children leaves the row empty"
    );
}

#[test]
fn the_wheel_steps_through_its_own_row_and_stops_at_the_ends() {
    let mut state = parent_with_jobs_state(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_4".into();
    projected.tabs.push(other);
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "tab_3";
    }
    projected.focused_tab_id = Some("tab_3".into());
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24).unwrap();
    let child_row = state.hits.child_tabs[0].0;
    let main_row = state.hits.tabs[0].0;
    let child_bar = state.hits.child_tab_bar;
    let child_gap = Rect::new(child_bar.right() - 1, child_bar.y, 1, 1);
    assert!(state
        .hits
        .child_tabs
        .iter()
        .all(|(rect, _)| rect.right() <= child_gap.x));
    let mut wheel = |rect: Rect, kind| {
        let outcome =
            state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                kind,
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => match &request.method {
                    Method::TabFocus(target) => Some(target.tab_id.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    assert!(
        wheel(child_row, MouseEventKind::ScrollDown).is_empty(),
        "the last child does not step on to the next main tab"
    );
    assert_eq!(wheel(child_row, MouseEventKind::ScrollUp), ["tab_2"]);
    assert_eq!(
        wheel(main_row, MouseEventKind::ScrollDown),
        ["tab_4"],
        "the main row skips children"
    );
    assert!(
        wheel(main_row, MouseEventKind::ScrollUp).is_empty(),
        "the parent is the first main tab"
    );
    assert_eq!(
        wheel(child_gap, MouseEventKind::ScrollUp),
        ["tab_2"],
        "the empty end of the child row steps its own row"
    );
}

#[test]
fn closing_a_parent_asks_then_closes_its_children_first() {
    let mut state = parent_with_jobs_state(true);
    assert_no_close(&request_close(&mut state, false));
    let frame = state.compose(106, 24).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("Close tab and its child tabs?"), "{text}");
    assert!(text.contains("2 child tabs: ◑ 1 !1"), "{text}");

    let accepted = state.handle_input_bytes(b"\r");

    assert_eq!(tab_closes(&accepted), ["tab_2", "tab_3", "tab_1"]);
}

#[test]
fn closing_a_parent_without_confirmations_still_closes_children_first() {
    let mut state = parent_with_jobs_state(false);
    state.config.confirm_close_running = false;

    let requested = request_close(&mut state, false);

    assert!(state.overlay.is_none());
    assert_eq!(tab_closes(&requested), ["tab_2", "tab_3", "tab_1"]);
}

fn running_job_state(confirm_running: bool) -> ClientShellState {
    let mut state = close_state(false, 2);
    state.config.confirm_close_running = confirm_running;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs[0].label = "build".into();
    projected.tabs[0].status = Some(crate::api::schema::TabStatus::Running);
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24).unwrap();
    state
}

fn pane_closes(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::PaneClose(target) => Some(target.pane_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn closing_a_running_tab_asks_even_with_close_confirmation_off() {
    for menu in [false, true] {
        let mut state = running_job_state(true);
        let requested = request_close(&mut state, menu);
        assert_no_close(&requested);
        let frame = state.compose(106, 24).unwrap();
        let text = frame_rows(&frame).join("\n");
        assert!(text.contains("Close tab?"), "{text}");
        assert!(text.contains("stops: build marked running"), "{text}");
        assert_tab_close(&state.handle_input_bytes(b"\r"));
        assert!(state.overlay.is_none());
    }
}

#[test]
fn closing_a_running_tab_is_immediate_when_the_setting_is_off() {
    let mut state = running_job_state(false);
    assert_tab_close(&request_close(&mut state, false));
    assert!(state.overlay.is_none());
}

#[test]
fn closing_a_parent_names_its_running_children() {
    let mut state = parent_with_jobs_state(false);
    let requested = request_close(&mut state, false);
    assert!(tab_closes(&requested).is_empty());
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.running.as_deref() == Some("tests marked running")));
    assert_eq!(
        tab_closes(&state.handle_input_bytes(b"\r")),
        ["tab_2", "tab_3", "tab_1"]
    );
}

fn busy_agent(pane_id: &str, status: AgentStatus) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 0,
        awaiting_reply: false,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    }
}

fn close_focused_pane(state: &mut ClientShellState) -> ClientShellInput {
    let mut requested = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::ClosePane),
        &mut requested,
    );
    requested
}

#[test]
fn closing_a_pane_with_a_waiting_agent_asks_first() {
    let mut state = close_state(false, 2);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected
        .agents
        .push(busy_agent("pane_1", AgentStatus::Blocked));
    state.set_snapshot(Box::new(projected));

    assert!(pane_closes(&close_focused_pane(&mut state)).is_empty());
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.title == "Close pane?"
                && confirm.running.as_deref() == Some("claude waiting")));
    assert!(pane_closes(&state.handle_input_bytes(b"\x1b")).is_empty());
    assert!(state.overlay.is_none());

    // An idle interactive agent session asks too: closing ends the live
    // process and loses its unsent input and scrollback.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents[0].agent_status = AgentStatus::Idle;
    state.set_snapshot(Box::new(projected));
    assert!(pane_closes(&close_focused_pane(&mut state)).is_empty());
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.running.as_deref() == Some("claude idle")));
    assert!(pane_closes(&state.handle_input_bytes(b"\x1b")).is_empty());

    // With background tasks it names them: they die with the pane.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents[0]
        .tokens
        .push(("bg".to_owned(), "2 bg".to_owned()));
    state.set_snapshot(Box::new(projected));
    assert!(pane_closes(&close_focused_pane(&mut state)).is_empty());
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.running.as_deref() == Some("claude idle · 2 bg")));
    assert_eq!(pane_closes(&state.handle_input_bytes(b"\r")), ["pane_1"]);
}

#[test]
fn closing_a_pane_running_a_program_asks_first() {
    let mut state = close_state(false, 2);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.panes[0].running_program = Some("lazygit".into());
    state.set_snapshot(Box::new(projected));

    assert!(pane_closes(&close_focused_pane(&mut state)).is_empty());
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.running.as_deref() == Some("lazygit")));
    assert_eq!(pane_closes(&state.handle_input_bytes(b"\r")), ["pane_1"]);
}

#[test]
fn closing_a_workspace_names_its_busy_agent() {
    let mut state = close_state(false, 1);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected
        .agents
        .push(busy_agent("pane_1", AgentStatus::Working));
    state.set_snapshot(Box::new(projected));
    let mut requested = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseWorkspace),
        &mut requested,
    );
    assert_no_close(&requested);
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.title == "Close workspace?"
                && confirm.running.as_deref() == Some("claude working in 1")));
}

fn focus_tab(state: &mut ClientShellState, tab_id: &str) {
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == tab_id;
    }
    projected.focused_tab_id = Some(tab_id.into());
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24).unwrap();
}

fn click_up(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    click(state, rect);
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn focused_by(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::TabFocus(target) => Some(target.tab_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// `tab_1` with children `tab_2` and `tab_3`, and a plain `tab_4`.
fn group_memory_state() -> ClientShellState {
    let mut state = parent_with_jobs_state(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_4".into();
    other.number = 4;
    other.label = "lazygit".into();
    other.focused = false;
    projected.tabs.push(other);
    state.set_snapshot(Box::new(projected));
    state
}

fn main_row_rect(state: &ClientShellState, tab_id: &str) -> Rect {
    state
        .hits
        .tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .map(|(rect, _)| *rect)
        .expect("main-row tab")
}

#[test]
fn a_main_row_tab_returns_to_its_groups_last_tab() {
    let mut state = group_memory_state();
    focus_tab(&mut state, "tab_3");
    focus_tab(&mut state, "tab_4");

    let rect = main_row_rect(&state, "tab_1");
    assert_eq!(focused_by(&click_up(&mut state, rect)), ["tab_3"]);

    let mut switched = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::SwitchTab(0)),
        &mut switched,
    );
    assert_eq!(focused_by(&switched), ["tab_3"]);
}

#[test]
fn a_group_without_history_or_with_its_last_tab_closed_opens_the_parent() {
    let mut state = group_memory_state();
    focus_tab(&mut state, "tab_4");
    let rect = main_row_rect(&state, "tab_4");
    assert_eq!(focused_by(&click_up(&mut state, rect)), ["tab_4"]);

    focus_tab(&mut state, "tab_3");
    focus_tab(&mut state, "tab_4");
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs.retain(|tab| tab.tab_id != "tab_3");
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24).unwrap();
    let rect = main_row_rect(&state, "tab_1");
    assert_eq!(focused_by(&click_up(&mut state, rect)), ["tab_1"]);
}

#[test]
fn the_parents_own_entry_in_the_second_row_selects_the_parent() {
    let mut state = group_memory_state();
    focus_tab(&mut state, "tab_3");
    let parent_entry = state
        .hits
        .child_tabs
        .iter()
        .find(|(_, id)| id == "tab_1")
        .map(|(rect, _)| *rect)
        .expect("parent entry");
    assert_eq!(focused_by(&click_up(&mut state, parent_entry)), ["tab_1"]);
}

#[test]
fn the_close_dialog_names_the_tab_by_its_task_and_does_not_repeat_it() {
    let mut state = close_state(false, 2);
    state.config.tab_label = crate::config::TabLabelConfig::Title;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut agent = busy_agent("pane_1", AgentStatus::Blocked);
    agent.terminal_title_stripped = Some("Unpack the 7z archives".into());
    projected.agents.push(agent);
    state.set_snapshot(Box::new(projected));

    assert!(pane_closes(&close_focused_pane(&mut state)).is_empty());
    assert!(matches!(state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(confirm))
            if confirm.title == "Close pane?"
                && confirm.detail == "Unpack the 7z archives"
                && confirm.running.as_deref() == Some("claude waiting")));
}
