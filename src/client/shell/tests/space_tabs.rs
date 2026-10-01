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
    let line = rows
        .iter()
        .position(|row| sidebar(row).contains("agent tab"))
        .expect("tab line under the space");
    assert!(sidebar(&rows[line]).contains("⧖ 1 !1"), "{}", rows[line]);
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

#[test]
fn clicking_a_tab_line_enters_its_groups_last_focused_tab() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    let click = |state: &mut ClientShellState| {
        state.compose(106, 30).unwrap();
        let (rect, _) = state.hits.space_tabs[0].clone();
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x + 4,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })])
    };
    let focuses = |outcome: &ClientShellInput, tab: &str| {
        outcome.actions.iter().any(|action| {
            matches!(action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(&request.method, crate::api::schema::Method::TabFocus(target)
                    if target.tab_id == tab))
        })
    };

    assert!(focuses(&click(&mut state), "tab_1"));

    // After the job tab had focus, the line returns to it.
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "job_1";
    }
    projected.focused_tab_id = Some("job_1".into());
    projected.workspaces[0].active_tab_id = "job_1".into();
    state.set_snapshot(Box::new(projected));
    assert!(focuses(&click(&mut state), "job_1"));
}

/// The state icon of the first tab line and its colour, drawn in a space that
/// is not focused: in the focused one the active tab's accent fill turns the
/// icon to the text colour.
fn tab_icon_color(state: &mut ClientShellState) -> (String, ratatui::style::Color) {
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.workspaces[0].focused = false;
    state.set_snapshot(Box::new(projected));
    let (symbol, fg, _) = tab_cell(state, 4);
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
fn tab_lines_take_the_tab_bars_colours_from_their_indent() {
    let mut state = state_with_tabs(true);
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
    let bg = |line: usize, column: u16| {
        let (rect, _) = state.hits.space_tabs[line];
        buffer[(rect.x + column, rect.y)].bg
    };

    // The active tab of the focused space is accent-filled, the other grey,
    // and neither fill reaches left of the tab indent.
    assert_eq!(bg(0, 3), palette.accent);
    assert_eq!(bg(1, 3), palette.surface0);
    assert_ne!(bg(0, 2), palette.accent);
    assert_ne!(bg(1, 2), palette.surface0);

    // A tab without an agent gets the program mark.
    let (rect, _) = state.hits.space_tabs[1];
    assert_eq!(buffer[(rect.x + 4, rect.y)].symbol(), "❏");

    // In a space that is not focused the active tab is only tinted.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.workspaces[0].focused = false;
    state.set_snapshot(Box::new(projected));
    let (_, _, bg) = tab_cell(&mut state, 3);
    assert_eq!(bg, super::super::render::tabs::accent_tint(&palette));
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
    set_agent_status(&mut state, AgentStatus::Idle, false);
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
    let mut state = state_with_tabs(true);
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
