use super::*;
use crate::api::schema::TabStatus;

fn state_with_tabs(tabs: bool) -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.spaces.tabs = tabs;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    projected.tabs[0].label = "agent tab".into();
    projected.tabs[0].agent_status = AgentStatus::Working;
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 0,
        awaiting_reply: false,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    });
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state
}

/// Adds a tab nested under `tab_1`, as herdr-job opens its jobs.
fn with_job(state: &mut ClientShellState, tab_id: &str, status: TabStatus) {
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut job = projected.tabs[0].clone();
    job.tab_id = tab_id.into();
    job.label = format!("job {tab_id}");
    job.focused = false;
    job.agent_status = AgentStatus::Unknown;
    job.parent_tab_id = Some("tab_1".into());
    job.status = Some(status);
    projected.tabs.push(job);
    state.set_snapshot(Box::new(projected));
}

fn set_agent_status(state: &mut ClientShellState, status: AgentStatus, awaiting_reply: bool) {
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs[0].agent_status = status;
    projected.agents[0].agent_status = status;
    projected.agents[0].awaiting_reply = awaiting_reply;
    state.set_snapshot(Box::new(projected));
}

#[test]
fn spaces_list_their_tabs_without_nested_jobs_when_enabled() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    with_job(&mut state, "job_2", TabStatus::Failed);
    with_job(&mut state, "job_3", TabStatus::Succeeded);
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let sidebar = |row: &String| row.chars().take(28).collect::<String>();
    let line = state.hits.space_tabs[0].0.y as usize;
    // The label is cut before the triangle and counts.
    assert!(sidebar(&rows[line]).contains("agent t…"), "{}", rows[line]);
    assert!(sidebar(&rows[line]).contains("► ⧖ 1 !1"), "{}", rows[line]);
    assert!(
        rows.iter().all(|row| !sidebar(row).contains("job job_")),
        "nested job tabs get no line: {rows:?}"
    );
    let space = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == "ws_1")
        .expect("space hit");
    assert!(
        (space.rect.y..space.rect.bottom()).contains(&(line as u16)),
        "the space's block covers its tab lines"
    );
    assert!(
        rows[space.rect.y as usize..line]
            .iter()
            .all(|row| !sidebar(row).contains("!1")),
        "the counts are not repeated on the space row"
    );
}

#[test]
fn spaces_keep_their_rows_when_disabled() {
    let mut state = state_with_tabs(false);
    state.compose(106, 30).unwrap();
    assert!(state.hits.space_tabs.is_empty());
}

fn left_click(state: &mut ClientShellState, (column, row): (u16, u16)) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn focuses(outcome: &ClientShellInput, tab: &str) -> bool {
    outcome.actions.iter().any(|action| {
        matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabFocus(target)
                if target.tab_id == tab))
    })
}

fn focus_tab(state: &mut ClientShellState, tab_id: &str) {
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == tab_id;
    }
    projected.focused_tab_id = Some(tab_id.into());
    projected.workspaces[0].active_tab_id = tab_id.into();
    state.set_snapshot(Box::new(projected));
}

fn click_tab_line(state: &mut ClientShellState) -> ClientShellInput {
    state.compose(106, 30).unwrap();
    let (rect, _) = state.hits.space_tabs[0];
    left_click(state, (rect.x + 6, rect.y))
}

/// Clicks the first tab line's disclosure triangle.
fn click_fold(state: &mut ClientShellState) -> ClientShellInput {
    state.compose(106, 30).unwrap();
    let (rect, _) = state.hits.space_tab_folds[0];
    left_click(state, (rect.x, rect.y))
}

#[test]
fn clicking_a_tab_line_always_opens_the_tab_itself() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_2".into();
    other.focused = false;
    projected.tabs.insert(1, other);
    state.set_snapshot(Box::new(projected));
    focus_tab(&mut state, "job_1");
    state.remember_focused_group_tab();
    // From another tab, not the job its group had open.
    focus_tab(&mut state, "tab_2");
    assert!(focuses(&click_tab_line(&mut state), "tab_1"));
    // From the open job, back to the tab.
    focus_tab(&mut state, "job_1");
    assert!(focuses(&click_tab_line(&mut state), "tab_1"));
    // On the tab itself it stays there and folds nothing.
    focus_tab(&mut state, "tab_1");
    assert!(focuses(&click_tab_line(&mut state), "tab_1"));
    assert!(state.unfolded_squares.is_empty());
}

#[test]
fn the_triangle_before_the_counts_folds_and_unfolds_the_squares() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    with_job(&mut state, "job_2", TabStatus::Failed);
    state.compose(106, 30).unwrap();
    assert!(state.hits.space_tab_squares.is_empty(), "folded by default");

    // Only a tab with nested tabs gets the triangle, right before its counts.
    let frame = state.compose(106, 30).unwrap();
    let (fold, _) = state.hits.space_tab_folds[0];
    let fold_line = frame_rows(&frame)[fold.y as usize]
        .chars()
        .collect::<Vec<_>>();
    assert_eq!(fold_line[fold.x as usize], '►');
    assert_eq!(state.hits.space_tab_folds.len(), 1);
    let outcome = click_fold(&mut state);
    assert!(!focuses(&outcome, "tab_1"));
    state.compose(106, 30).unwrap();
    let squares = state
        .hits
        .space_tab_squares
        .iter()
        .map(|(_, tab_id)| tab_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(squares, ["job_1", "job_2"]);
    // The squares sit on the row under the line, and the counts stay.
    let (line, _) = state.hits.space_tabs[0];
    let (square, _) = state.hits.space_tab_squares[0];
    assert_eq!(square.y, line.y + 1);
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    assert!(
        rows[line.y as usize].contains("⧖ 1 !1"),
        "{}",
        rows[line.y as usize]
    );
    let square_row = rows[square.y as usize].chars().collect::<Vec<_>>();
    assert_eq!(square_row[square.x as usize + 1], '⧖');
    // The first square starts under the tab's fill, past the state icon.
    assert_eq!(square.x, line.x + 5);
    let fold_line = rows[line.y as usize].chars().collect::<Vec<_>>();
    assert_eq!(fold_line[fold.x as usize], '▼');

    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    assert!(state.hits.space_tab_squares.is_empty());
}

#[test]
fn a_square_opens_its_job_and_the_open_square_goes_back() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    let (square, _) = state.hits.space_tab_squares[0];
    assert!(focuses(
        &left_click(&mut state, (square.x + 1, square.y)),
        "job_1"
    ));

    focus_tab(&mut state, "job_1");
    state.compose(106, 30).unwrap();
    let (square, _) = state.hits.space_tab_squares[0];
    assert!(focuses(
        &left_click(&mut state, (square.x + 1, square.y)),
        "tab_1"
    ));
}

#[test]
fn middle_click_on_a_square_closes_only_its_job() {
    let mut state = state_with_tabs(true);
    state.config.confirm_close = false;
    with_job(&mut state, "job_1", TabStatus::Succeeded);
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    let (square, _) = state.hits.space_tab_squares[0];
    let outcome =
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle),
            column: square.x + 1,
            row: square.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let closed = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::TabClose(target) => Some(target.tab_id.clone()),
                crate::api::schema::Method::WorkspaceClose(params) => {
                    Some(params.workspace_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(closed, ["job_1"]);
}

#[test]
fn vertical_tabs_hide_both_tab_rows() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    state.compose(106, 30).unwrap();
    assert!(state.hits.tab_bar.is_empty());
    assert!(state.hits.child_tabs.is_empty());

    let mut state = state_with_tabs(false);
    with_job(&mut state, "job_1", TabStatus::Running);
    state.compose(106, 30).unwrap();
    assert!(!state.hits.tab_bar.is_empty());
}

#[test]
fn the_job_headers_ends_go_back_and_close() {
    let mut state = state_with_tabs(true);
    state.config.confirm_close = false;
    // A failed job closes at once; a running one would ask first.
    with_job(&mut state, "job_1", TabStatus::Failed);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.panes[0].tab_id = "job_1".into();
    // The fixture's agent runs in this pane; a job pane runs none.
    projected.agents.clear();
    state.set_snapshot(Box::new(projected));
    focus_tab(&mut state, "job_1");
    // A pane wide enough for both buttons.
    let mut wide = surface();
    let lines = ["x".repeat(20), "y".repeat(20), "z".repeat(20)];
    wide.frame = crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
        &ratatui::buffer::Buffer::with_lines(lines),
        None,
        &[],
    );
    let pane = &mut wide.panes[0];
    for rect in [&mut pane.rect, &mut pane.inner_rect] {
        rect.width = 20;
        rect.height = 3;
    }
    state.set_pane_surface(wide);
    state.compose(106, 30).unwrap();
    let pane = state.hits.panes[0].inner_rect;

    assert!(focuses(
        &left_click(&mut state, (pane.x + 1, pane.y)),
        "tab_1"
    ));
    let outcome = left_click(&mut state, (pane.right() - 2, pane.y));
    assert!(
        outcome.actions.iter().any(|action| matches!(action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(&request.method, crate::api::schema::Method::TabClose(target)
                    if target.tab_id == "job_1"))),
        "{:?}",
        outcome.actions
    );
    // Below the header row, or in its middle, clicks reach the pane.
    assert!(!focuses(
        &left_click(&mut state, (pane.x + 1, pane.y + 1)),
        "tab_1"
    ));
    assert!(!focuses(
        &left_click(&mut state, (pane.x + 10, pane.y)),
        "tab_1"
    ));
}

/// The state icon of the first tab line and its colour.
fn tab_icon_color(state: &mut ClientShellState) -> (String, ratatui::style::Color) {
    let (symbol, fg, _) = tab_cell(state, 3);
    (symbol, fg)
}

/// The cell `column` columns into the first tab line: its symbol and colours.
fn tab_cell(
    state: &mut ClientShellState,
    column: u16,
) -> (String, ratatui::style::Color, ratatui::style::Color) {
    let frame = state.compose(106, 30).unwrap();
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let (rect, _) = state.hits.space_tabs[0];
    let cell = &buffer[(rect.x + column, rect.y)];
    (cell.symbol().to_owned(), cell.fg, cell.bg)
}

#[test]
fn only_the_focused_spaces_active_tab_line_is_blue() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_2".into();
    other.focused = false;
    other.agent_status = AgentStatus::Unknown;
    projected.tabs.push(other);
    state.set_snapshot(Box::new(projected));
    let palette = state.config.palette.clone();
    let frame = state.compose(106, 30).unwrap();
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let cell = |line: usize, column: u16| {
        let (rect, _) = state.hits.space_tabs[line];
        buffer[(rect.x + column, rect.y)].clone()
    };
    let (rect, _) = state.hits.space_tabs[0];
    let last = rect.width - 3;

    // The focused space's active tab is accent-tinted, the other tab a
    // lighter grey, and neither fill reaches the state icon, which keeps
    // its colour.
    let tint = cell(0, 5).bg;
    let inactive = cell(1, 5).bg;
    assert_ne!(tint, palette.accent);
    assert_ne!(tint, inactive);
    assert_ne!(cell(0, 4).bg, tint);
    assert_ne!(cell(1, 4).bg, inactive);
    assert_eq!(cell(0, 3).fg, palette.yellow);
    // On the tint the job count keeps its colour.
    assert_eq!(cell(0, last).fg, palette.yellow);
    assert_eq!(cell(0, last).bg, tint);

    // A tab without an agent gets the program mark.
    assert_eq!(cell(1, 3).symbol(), "❏");

    // In a space that is not focused the active tab is grey, not blue.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.workspaces[0].focused = false;
    state.set_snapshot(Box::new(projected));
    let (_, _, active_elsewhere) = tab_cell(&mut state, 5);
    assert_ne!(active_elsewhere, tint);
    assert_ne!(active_elsewhere, inactive);
}

#[test]
fn an_idle_tab_with_a_running_job_shows_the_waiting_mark() {
    let mut state = state_with_tabs(true);
    set_agent_status(&mut state, AgentStatus::Idle, false);
    with_job(&mut state, "job_1", TabStatus::Running);
    let mauve = state.config.palette.mauve;
    assert_eq!(tab_icon_color(&mut state), ("●".to_owned(), mauve));

    // A finished job, or a working agent, keeps the usual status.
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Succeeded);
    assert_ne!(tab_icon_color(&mut state).1, mauve);
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    assert_eq!(tab_icon_color(&mut state).1, state.config.palette.yellow);
}

#[test]
fn a_tab_awaiting_a_reply_shows_a_question_mark_over_a_running_job() {
    let mut state = state_with_tabs(true);
    set_agent_status(&mut state, AgentStatus::Done, true);
    with_job(&mut state, "job_1", TabStatus::Running);
    let done = super::super::status_color(AgentStatus::Done, &state.config.palette);
    assert_eq!(tab_icon_color(&mut state), ("?".to_owned(), done));
}

#[test]
fn a_tab_with_an_agent_awaiting_a_reply_shows_a_question_mark_in_the_tab_bar() {
    // Vertical tabs hide the tab bar.
    let mut state = state_with_tabs(false);
    set_agent_status(&mut state, AgentStatus::Done, true);
    let frame = state.compose(106, 30).unwrap();
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    let bar = state.hits.tab_bar;
    let row: String = (bar.x..bar.right())
        .map(|x| buffer[(x, bar.y)].symbol().to_owned())
        .collect();
    assert!(row.contains("? "), "{row:?}");
}

#[test]
fn the_symbols_style_uses_a_clock_for_waiting() {
    let mut state = state_with_tabs(true);
    state.config.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
    set_agent_status(&mut state, AgentStatus::Done, false);
    with_job(&mut state, "job_1", TabStatus::Running);
    assert_eq!(tab_icon_color(&mut state).0, "◷");
}

#[test]
fn the_spaces_chevron_hides_and_shows_its_tab_lines() {
    let mut state = state_with_tabs(true);
    state.compose(106, 30).unwrap();
    assert_eq!(state.hits.space_tabs.len(), 1);
    let click_chevron = |state: &mut ClientShellState| {
        let (rect, _) = state
            .hits
            .workspaces
            .iter()
            .find(|hit| hit.workspace_id == "ws_1")
            .and_then(|hit| hit.group_toggle.clone())
            .expect("chevron");
        let events = vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })];
        state.handle_raw_events(events);
        state.compose(106, 30).unwrap();
    };

    // In front of the name, like tree-style tab lists.
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let space = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == "ws_1")
        .expect("space hit");
    let (toggle, _) = space.group_toggle.clone().expect("chevron");
    let name_line = rows[toggle.y as usize].chars().collect::<Vec<_>>();
    assert_eq!(name_line[toggle.x as usize], '▼', "{name_line:?}");
    assert_eq!(toggle.x, space.rect.x + 1);

    click_chevron(&mut state);
    assert!(state.hits.space_tabs.is_empty());
    let frame = state.compose(106, 30).unwrap();
    assert_eq!(
        frame_rows(&frame)[toggle.y as usize]
            .chars()
            .nth(toggle.x as usize),
        Some('►')
    );
    click_chevron(&mut state);
    assert_eq!(state.hits.space_tabs.len(), 1);
}

#[test]
fn middle_and_right_click_on_a_tab_line_target_the_tab_not_its_space() {
    let mut state = state_with_tabs(true);
    state.config.confirm_close = false;
    with_job(&mut state, "job_1", TabStatus::Succeeded);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_2".into();
    other.focused = false;
    projected.tabs.push(other);
    // The job tab had focus last; the line still stands for its group.
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "job_1";
    }
    projected.focused_tab_id = Some("job_1".into());
    projected.workspaces[0].active_tab_id = "job_1".into();
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    let (line, _) = state.hits.space_tabs[0];
    let space = state.hits.workspaces[0].rect;
    assert!(
        space.y < line.y,
        "the space's name line sits above its tabs"
    );
    let press = |state: &mut ClientShellState, button, (column, row): (u16, u16)| {
        state.overlay = None;
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(button),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })])
    };
    let closes = |outcome: &ClientShellInput| {
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => match &request.method {
                    crate::api::schema::Method::TabClose(target) => {
                        Some(format!("tab {}", target.tab_id))
                    }
                    crate::api::schema::Method::WorkspaceClose(params) => {
                        Some(format!("space {}", params.workspace_id))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let middle = crossterm::event::MouseButton::Middle;
    let right = crossterm::event::MouseButton::Right;

    let outcome = press(&mut state, middle, (line.x + 4, line.y));
    // The agent makes both closes ask first; the tab line asks for its whole
    // group, the name line for the space.
    assert!(closes(&outcome).is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(ref confirm))
            if confirm.tab_target.as_ref().is_some_and(|target| {
                target.tab_id == "tab_1" && target.children == ["job_1"]
            })
    ));
    let outcome = press(&mut state, middle, (space.x + 2, space.y));
    assert!(closes(&outcome).is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(ref confirm))
            if confirm.workspace_id == "ws_1" && confirm.tab_target.is_none()
    ));

    press(&mut state, right, (line.x + 4, line.y));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { ref tab_id, .. },
            ..
        })) if tab_id == "tab_1"
    ));
    press(&mut state, right, (space.x + 2, space.y));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
            ..
        })) if workspace_id == "ws_1"
    ));
}
