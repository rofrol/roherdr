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
        let close_row = state.hits.context_menu_rows[2].0;
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
    // `⏳` is two cells wide; `frame_rows` shows its second cell as a space.
    let rows = frame_rows(&frame)
        .into_iter()
        .map(|row| row.replace("⏳ ", "⏳"))
        .collect::<Vec<_>>();

    assert_eq!(state.hits.tabs.len(), 1, "children leave the main row");
    assert!(rows[0].contains("1 ⏳1 !1"), "{}", rows[0]);
    assert!(
        rows[1].contains("! build") && rows[1].contains("⏳ tests"),
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
        ["tab_4"],
        "a tab without children shows only itself"
    );
}

#[test]
fn closing_a_parent_asks_then_closes_its_children_first() {
    let mut state = parent_with_jobs_state(true);
    assert_no_close(&request_close(&mut state, false));
    let frame = state.compose(106, 24).unwrap();
    let text = frame_rows(&frame).join("\n").replace("⏳ ", "⏳");
    assert!(text.contains("Close tab and its child tabs?"), "{text}");
    assert!(text.contains("2 child tabs: ⏳1 !1"), "{text}");

    let accepted = state.handle_input_bytes(b"\r");

    assert_eq!(tab_closes(&accepted), ["tab_2", "tab_3", "tab_1"]);
}

#[test]
fn closing_a_parent_without_confirm_close_still_closes_children_first() {
    let mut state = parent_with_jobs_state(false);

    let requested = request_close(&mut state, false);

    assert!(state.overlay.is_none());
    assert_eq!(tab_closes(&requested), ["tab_2", "tab_3", "tab_1"]);
}
