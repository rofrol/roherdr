use super::*;
use crate::api::schema::TabStatus;

fn state_with_tabs(tabs: bool) -> ClientShellState {
    let mut config = config_with_sidebar_width(26);
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
    // The label is cut first; the counts include the succeeded job.
    assert!(sidebar(&rows[line]).contains("agent…"), "{}", rows[line]);
    assert!(
        sidebar(&rows[line]).contains("► ◐ 1 !1 ✓1"),
        "{}",
        rows[line]
    );
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

fn mouse_at(
    state: &mut ClientShellState,
    kind: crossterm::event::MouseEventKind,
    (column, row): (u16, u16),
) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn left_release(state: &mut ClientShellState, at: (u16, u16)) -> ClientShellInput {
    mouse_at(
        state,
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        at,
    )
}

fn left_drag(state: &mut ClientShellState, at: (u16, u16)) -> ClientShellInput {
    mouse_at(
        state,
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        at,
    )
}

/// A click on the first tab line: the tab opens when the button is released.
fn click_tab_line(state: &mut ClientShellState) -> ClientShellInput {
    state.compose(106, 30).unwrap();
    let (rect, _) = state.hits.space_tabs[0];
    let at = (rect.x + 6, rect.y);
    assert!(
        left_click(state, at).actions.is_empty(),
        "waits for release"
    );
    left_release(state, at)
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
    // On the tab itself it stays there and folds nothing: the click leaves
    // the unfolded squares as focusing the job had set them.
    focus_tab(&mut state, "tab_1");
    let before = state.unfolded_squares.clone();
    assert!(focuses(&click_tab_line(&mut state), "tab_1"));
    assert_eq!(state.unfolded_squares, before);
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
        rows[line.y as usize].contains("◐ 1 !1"),
        "{}",
        rows[line.y as usize]
    );
    let square_row = rows[square.y as usize].chars().collect::<Vec<_>>();
    assert_eq!(square_row[square.x as usize + 1], '◐');
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
fn the_job_footers_ends_go_back_and_close() {
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
    let footer_y = pane.bottom() - 1;

    assert!(focuses(
        &left_click(&mut state, (pane.x + 1, footer_y)),
        "tab_1"
    ));
    let outcome = left_click(&mut state, (pane.right() - 2, footer_y));
    assert!(
        outcome.actions.iter().any(|action| matches!(action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(&request.method, crate::api::schema::Method::TabClose(target)
                    if target.tab_id == "job_1"))),
        "{:?}",
        outcome.actions
    );
    // The old header row and the footer's middle pass clicks to the pane.
    assert!(!focuses(
        &left_click(&mut state, (pane.x + 1, pane.y)),
        "tab_1"
    ));
    assert!(!focuses(
        &left_click(&mut state, (pane.x + 10, footer_y)),
        "tab_1"
    ));
    // A full-screen program owns the last row: its click is not the footer's.
    state.pane_surface.as_mut().expect("surface").panes[0].alternate_screen_active = true;
    state.compose(106, 30).unwrap();
    assert!(!focuses(
        &left_click(&mut state, (pane.x + 1, footer_y)),
        "tab_1"
    ));
}

#[test]
fn client_job_footer_reserves_chrome_across_resize_and_rejects_stale_metadata() {
    let mut state = state_with_tabs(true);
    state.config.confirm_close = false;
    with_job(&mut state, "job_1", TabStatus::Failed);
    focus_tab(&mut state, "job_1");
    let snapshot = state.snapshot.as_deref().unwrap();
    let projection = crate::protocol::endpoint::EndpointJobMetadata {
        boot_id: snapshot.boot_id.clone(),
        revision: snapshot.revision,
        tab_id: snapshot.focused_tab_id.clone(),
        job: Some(crate::api::schema::TabJobMetadata {
            id: "probe".into(),
            name: "Build probe".into(),
            why: Some("resize check".into()),
            origin: "agent".into(),
            owner_pane: None,
        }),
    };
    state.endpoints[0].job_metadata = Some((None, projection.clone()));
    for (cols, rows) in [(106, 30), (80, 20), (40, 12), (106, 30)] {
        let layout = state.layout(cols, rows);
        let base = state.config.layout(
            cols,
            rows,
            state.sidebar_collapsed,
            state.focused_tab_count(),
            state.sidebar_width,
            true,
        );
        assert_eq!(layout.pane_surface.height + 1, base.pane_surface.height);
        assert_eq!(layout.pane_surface.bottom(), layout.job_footer.y);
        assert_eq!(
            state.surface_size(cols, rows).rows,
            layout.pane_surface.height
        );
        let frame = state.compose(cols, rows).unwrap();
        let buffer = frame.to_ratatui_buffer().unwrap();
        assert_eq!(
            buffer[(layout.job_footer.x + 1, layout.job_footer.y)].symbol(),
            "←"
        );
        assert!(focuses(
            &left_click(&mut state, (layout.job_footer.x + 1, layout.job_footer.y)),
            "tab_1"
        ));
        let outcome = left_click(
            &mut state,
            (layout.job_footer.right() - 2, layout.job_footer.y),
        );
        assert!(outcome.actions.iter().any(|action| matches!(action,
            ClientShellAction::Endpoint { request, .. }
                if matches!(&request.method, crate::api::schema::Method::TabClose(target)
                    if target.tab_id == "job_1"))));
    }
    state.endpoints[0].job_metadata.as_mut().unwrap().1.revision += 1;
    assert!(state.active_job_metadata().is_none());
    assert!(state.layout(106, 30).job_footer.is_empty());
    state.endpoints[0].job_metadata = Some((None, projection));
    assert!(state.layout(4, 3).job_footer.is_empty());
}

#[test]
fn job_footer_uses_text_foreground_in_light_and_dark_themes() {
    for palette in [Palette::catppuccin_latte(), Palette::catppuccin()] {
        let mut state = state_with_tabs(true);
        state.config.palette = palette.clone();
        with_job(&mut state, "job_1", TabStatus::Running);
        focus_tab(&mut state, "job_1");
        let snapshot = state.snapshot.as_deref().unwrap();
        state.endpoints[0].job_metadata = Some((
            None,
            crate::protocol::endpoint::EndpointJobMetadata {
                boot_id: snapshot.boot_id.clone(),
                revision: snapshot.revision,
                tab_id: snapshot.focused_tab_id.clone(),
                job: Some(crate::api::schema::TabJobMetadata {
                    id: "contrast-probe".into(),
                    name: "Build".into(),
                    why: Some("verify contrast".into()),
                    origin: "agent".into(),
                    owner_pane: None,
                }),
            },
        ));
        for (cols, rows) in [(106, 30), (40, 12)] {
            let area = state.layout(cols, rows).job_footer;
            assert!(!area.is_empty());
            let buffer = state
                .compose(cols, rows)
                .unwrap()
                .to_ratatui_buffer()
                .unwrap();
            assert_eq!(buffer[(area.x + 1, area.y)].symbol(), "←");
            assert_eq!(buffer[(area.right() - 2, area.y)].symbol(), "×");
            for x in area.x..area.right() {
                let cell = &buffer[(x, area.y)];
                assert_eq!(cell.fg, palette.text, "footer column {x}");
                assert_eq!(cell.bg, palette.sidebar_bg, "footer column {x}");
                assert_ne!(cell.fg, palette.surface_dim);
            }
        }
    }
}

#[test]
fn job_footer_generation_is_applied_atomically_with_snapshot_for_resize() {
    let mut state = state_with_tabs(true);
    let endpoint = ClientEndpointId::Local;
    let mut snapshot = state.snapshot.as_deref().unwrap().clone();
    snapshot.revision = 10;
    state.set_endpoint_snapshot_for_generation(&endpoint, 4, Box::new(snapshot.clone()));
    let full = state.surface_size(106, 30);
    let mut projection = crate::protocol::endpoint::EndpointJobMetadata {
        boot_id: snapshot.boot_id.clone(),
        revision: 11,
        tab_id: snapshot.focused_tab_id.clone(),
        job: Some(crate::api::schema::TabJobMetadata {
            id: "probe".into(),
            name: "Build".into(),
            why: None,
            origin: "agent".into(),
            owner_pane: None,
        }),
    };
    state.set_endpoint_job_metadata(&endpoint, 4, projection.clone());
    assert_eq!(state.surface_size(106, 30), full);
    snapshot.revision = 11;
    state.set_endpoint_snapshot_for_generation(&endpoint, 4, Box::new(snapshot.clone()));
    assert!(state.active_job_metadata().is_some());
    let reduced = state.surface_size(106, 30);
    assert_eq!(reduced.rows + 1, full.rows);
    projection.revision = 12;
    state.set_endpoint_job_metadata(&endpoint, 4, projection.clone());
    // Receiving the next companion must not temporarily remove the current footer.
    assert_eq!(state.surface_size(106, 30), reduced);
    snapshot.revision = 12;
    state.set_endpoint_snapshot_for_generation(&endpoint, 4, Box::new(snapshot.clone()));
    assert_eq!(state.surface_size(106, 30), reduced);
    projection.revision = 13;
    projection.job = None;
    state.set_endpoint_job_metadata(&endpoint, 4, projection.clone());
    assert_eq!(state.surface_size(106, 30), reduced);
    snapshot.revision = 13;
    state.set_endpoint_snapshot_for_generation(&endpoint, 4, Box::new(snapshot.clone()));
    assert_eq!(state.surface_size(106, 30), full);
    // Stale generation cannot replace a new generation's same-boot projection.
    projection.revision = 1;
    projection.job = Some(crate::api::schema::TabJobMetadata {
        id: "new".into(),
        name: "Reconnected".into(),
        why: None,
        origin: "agent".into(),
        owner_pane: None,
    });
    state.set_endpoint_job_metadata(&endpoint, 5, projection.clone());
    snapshot.revision = 1;
    state.set_endpoint_snapshot_for_generation(&endpoint, 5, Box::new(snapshot));
    projection.job = None;
    state.set_endpoint_job_metadata(&endpoint, 4, projection);
    assert_eq!(state.active_job_metadata().unwrap().id, "new");
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
    let last = rect.width - 2;

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
    // The shapes style (the default) marks it with a clock.
    assert_eq!(tab_icon_color(&mut state), ("◐".to_owned(), mauve));

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
fn the_symbols_style_uses_a_half_circle_for_waiting() {
    let mut state = state_with_tabs(true);
    state.config.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
    set_agent_status(&mut state, AgentStatus::Done, false);
    with_job(&mut state, "job_1", TabStatus::Running);
    assert_eq!(tab_icon_color(&mut state).0, "◐");
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

#[test]
fn resting_on_a_square_names_its_job_right_of_it_after_dwell() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Failed);
    with_job(&mut state, "job_2", TabStatus::Running);
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    let (first, _) = state.hits.space_tab_squares[0];
    let (second, _) = state.hits.space_tab_squares[1];
    let (line, _) = state.hits.space_tabs[0];
    let hover = |state: &mut ClientShellState, (column, row): (u16, u16)| {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 30).unwrap();
        if let Some(deadline) = state.tooltip_deadline() {
            assert!(!state.tooltip_visible(), "new target starts a fresh dwell");
            state.tick_selection_autoscroll(deadline - std::time::Duration::from_millis(1));
            assert!(!state.tooltip_visible(), "not before 450 ms");
            state.tick_selection_autoscroll(deadline);
            assert!(state.tooltip_visible(), "shown at 450 ms");
        }
        let frame = state.compose(106, 30).unwrap();
        let rows = frame_rows(&frame);
        let from = |x: u16, y: u16| {
            rows[y as usize]
                .chars()
                .skip(x as usize)
                .collect::<String>()
        };
        (from(first.right(), first.y), from(line.x, line.y))
    };

    // After the dwell, right of the square; the tab line keeps its label.
    let (row, tab_line) = hover(&mut state, (first.x + 1, first.y));
    assert!(row.starts_with("job job_1 "), "{row:?}");
    assert!(tab_line.contains("agent tab"), "{tab_line:?}");
    // The tooltip covers the next square but takes no hover: moving there
    // names that job.
    let (row, _) = hover(&mut state, (second.x + 1, second.y));
    assert!(!row.contains("job_1"), "{row:?}");
    let frame = state.compose(106, 30).unwrap();
    let second_row = frame_rows(&frame)[second.y as usize]
        .chars()
        .skip(second.right() as usize)
        .collect::<String>();
    assert!(second_row.starts_with("job job_2 "), "{second_row:?}");
    // In the square's fill, so the two read as one.
    let buffer = frame.to_ratatui_buffer().expect("buffer");
    assert_eq!(
        buffer[(second.right(), second.y)].bg,
        buffer[(second.x, second.y)].bg
    );
    // Off the squares it goes.
    let (row, _) = hover(&mut state, (first.x + 1, first.y + 3));
    assert!(!row.contains("job job_"), "{row:?}");
}

#[test]
fn pending_job_tooltips_are_cancelled_by_input_and_target_removal() {
    for dismiss in ["key", "click", "scroll", "removed"] {
        let mut state = state_with_tabs(true);
        with_job(&mut state, "job_1", TabStatus::Running);
        click_fold(&mut state);
        state.compose(106, 30).unwrap();
        let (square, _) = state.hits.space_tab_squares[0];
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: square.x + 1,
            row: square.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 30).unwrap();
        let deadline = state.tooltip_deadline().expect("pending dwell");
        assert!(!state.tooltip_visible());
        match dismiss {
            "key" => {
                state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
                    crate::input::TerminalKey::new(
                        crossterm::event::KeyCode::Char('x'),
                        KeyModifiers::empty(),
                    ),
                )]);
            }
            "click" | "scroll" => {
                state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
                    kind: if dismiss == "click" {
                        crossterm::event::MouseEventKind::Down(MouseButton::Left)
                    } else {
                        crossterm::event::MouseEventKind::ScrollDown
                    },
                    column: square.x + 1,
                    row: square.y,
                    modifiers: KeyModifiers::empty(),
                })]);
            }
            "removed" => state.hits.tooltips.clear(),
            _ => unreachable!(),
        }
        state.tick_selection_autoscroll(deadline);
        assert!(
            !state.tooltip_visible(),
            "{dismiss} cancels pending tooltip"
        );
        assert!(state.tooltip_deadline().is_none(), "{dismiss} clears timer");
    }
}

#[test]
fn a_closed_jobs_square_keeps_its_slot_while_the_pointer_is_over_the_list() {
    let mut state = state_with_tabs(true);
    state.config.confirm_close = false;
    with_job(&mut state, "job_1", TabStatus::Succeeded);
    with_job(&mut state, "job_2", TabStatus::Running);
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    let (first, _) = state.hits.space_tab_squares[0];
    let (second, _) = state.hits.space_tab_squares[1];
    let move_to = |state: &mut ClientShellState, (column, row): (u16, u16)| {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 30).unwrap();
    };
    move_to(&mut state, (first.x + 1, first.y));

    // job_1 closes itself under the pointer.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs.retain(|tab| tab.tab_id != "job_1");
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    assert_eq!(state.hits.space_tab_gone, [first]);
    assert_eq!(state.hits.space_tab_squares, [(second, "job_2".to_owned())]);

    // Its blank slot takes no click, not even the space's middle-click close.
    let outcome =
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle),
            column: first.x + 1,
            row: first.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.actions.is_empty(), "{:?}", outcome.actions);
    assert!(state.overlay.is_none());
    assert!(!focuses(
        &left_click(&mut state, (first.x + 1, first.y)),
        "tab_1"
    ));

    // Leaving the list lets the others close up.
    let pane = state.hits.panes[0].inner_rect;
    move_to(&mut state, (pane.x, pane.y));
    state.compose(106, 30).unwrap();
    assert!(state.hits.space_tab_gone.is_empty());
    assert_eq!(state.hits.space_tab_squares, [(first, "job_2".to_owned())]);
}

#[test]
fn the_spaces_list_scrolls_by_rows_to_the_last_square_of_a_tall_space() {
    let mut state = state_with_tabs(true);
    for index in 0..60 {
        with_job(&mut state, &format!("job_{index}"), TabStatus::Running);
    }
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    let body = state.hits.workspace_body;
    let max = state.hits.workspace_max_scroll;
    assert!(max > 0, "twelve square rows do not fit");

    // Wheel to the bottom: the space's top rows scroll away, its last
    // squares show and take clicks.
    for _ in 0..max {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: body.x + 2,
            row: body.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
    }
    assert_eq!(state.workspace_scroll, max);
    let frame = state.compose(106, 30).unwrap();
    let last = state
        .hits
        .space_tab_squares
        .iter()
        .find(|(_, tab_id)| tab_id == "job_59")
        .map(|(rect, _)| *rect)
        .expect("the last square is drawn");
    assert!(last.bottom() <= body.bottom());
    let row = frame_rows(&frame)[last.y as usize]
        .chars()
        .collect::<Vec<_>>();
    assert_eq!(row[last.x as usize + 1], '◐');
    assert!(focuses(
        &left_click(&mut state, (last.x + 1, last.y)),
        "job_59"
    ));
    // The space's name row is above the list, so it has no hit in view,
    // but the space still knows where it is.
    let space = state.hits.workspaces[0].rect;
    assert!(space.y >= body.y, "{space:?}");
    assert!(state.hits.workspace_layout[0].top < i32::from(body.y));
}

#[test]
fn revealing_the_focused_space_brings_its_open_job_square_in() {
    let mut state = state_with_tabs(true);
    for index in 0..60 {
        with_job(&mut state, &format!("job_{index}"), TabStatus::Running);
    }
    click_fold(&mut state);
    focus_tab(&mut state, "job_59");
    state.workspace_scroll = 0;
    state.reveal_focused_workspace = true;
    state.compose(106, 30).unwrap();
    let body = state.hits.workspace_body;
    let open = state
        .hits
        .space_tab_squares
        .iter()
        .find(|(_, tab_id)| tab_id == "job_59")
        .map(|(rect, _)| *rect)
        .expect("the open square is revealed");
    assert!(open.y >= body.y && open.bottom() <= body.bottom());
}

#[test]
fn the_plus_on_a_spaces_name_line_opens_a_tab_there() {
    let mut state = state_with_tabs(true);
    let frame = state.compose(106, 30).unwrap();
    let space = state.hits.workspaces[0].rect;
    let (plus, workspace_id) = state.hits.space_new_tab[0].clone();
    assert_eq!(workspace_id, "ws_1");
    assert_eq!(plus.y, space.y);
    let row = frame_rows(&frame)[plus.y as usize]
        .chars()
        .collect::<Vec<_>>();
    assert_eq!(row[plus.x as usize + 1], '+');

    let outcome = left_click(&mut state, (plus.x + 1, plus.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabCreate(params)
                if params.workspace_id.as_deref() == Some("ws_1") && params.focus))));
    // Not a press on the space, which would start a drag or select it.
    assert!(state.workspace_press.is_none());
}

#[test]
fn resting_on_a_tab_state_glyph_says_what_it_means() {
    let mut state = state_with_tabs(true);
    state.compose(106, 30).unwrap();
    let (line, _) = state.hits.space_tabs[0];
    let target = state
        .hits
        .tooltips
        .iter()
        .find(|target| target.id.starts_with("tab-state:") && target.rect.y == line.y)
        .expect("state glyph tooltip")
        .clone();
    assert_eq!(target.text, "working · menu › status legend");
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: target.rect.x,
        row: target.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 30).unwrap();
    let deadline = state.tooltip_deadline().expect("pending dwell");
    state.tick_selection_autoscroll(deadline);
    let frame = state.compose(106, 30).unwrap();
    let row = frame_rows(&frame)[target.rect.y as usize].clone();
    assert!(row.contains("working · menu › status legend"), "{row}");
}

#[test]
fn resting_on_a_cut_tab_label_shows_it_whole_until_a_key() {
    let mut state = state_with_tabs(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs[0].label = "a tab label much longer than the sidebar".into();
    projected.tabs[0].custom_label = true;
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    let target = state
        .hits
        .tooltips
        .iter()
        .find(|target| target.id.starts_with("tab:"))
        .expect("cut label tooltip")
        .clone();
    assert_eq!(target.text, "a tab label much longer than the sidebar");
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column: target.rect.x + 1,
        row: target.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let shown = |state: &mut ClientShellState| {
        let frame = state.compose(106, 30).unwrap();
        frame_rows(&frame)[target.rect.y as usize].contains("longer than the sidebar")
    };
    assert!(!shown(&mut state), "not before the dwell");
    let now = std::time::Instant::now();
    assert!(
        state
            .tick_selection_autoscroll(now + std::time::Duration::from_millis(500))
            .repaint
    );
    assert!(shown(&mut state));

    // A key hides it.
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
        crate::input::TerminalKey::new(crossterm::event::KeyCode::Char('x'), KeyModifiers::empty()),
    )]);
    assert!(!shown(&mut state));
}

#[test]
fn a_cut_tab_labels_tooltip_keeps_the_accent_bar_and_icon() {
    // One label fits the screen, the other is longer than it: neither
    // tooltip may cover the active tab's bar or its state icon.
    for label in [
        "a tab label much longer than the sidebar".to_owned(),
        "x".repeat(150),
    ] {
        let mut state = state_with_tabs(true);
        let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
        projected.tabs[0].label = label.clone();
        projected.tabs[0].custom_label = true;
        state.set_snapshot(Box::new(projected));
        let frame = state.compose(106, 30).unwrap();
        let target = state
            .hits
            .tooltips
            .iter()
            .find(|target| target.id.starts_with("tab:"))
            .expect("cut label tooltip")
            .clone();
        let left = |frame: &FrameData| {
            frame_rows(frame)[target.rect.y as usize]
                .chars()
                .take(target.rect.x as usize)
                .collect::<String>()
        };
        let before = left(&frame);
        assert!(before.ends_with('▌'), "{before}");
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: target.rect.x + 1,
            row: target.rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.compose(106, 30).unwrap();
        let deadline = state.tooltip_deadline().expect("pending dwell");
        state.tick_selection_autoscroll(deadline);
        let frame = state.compose(106, 30).unwrap();
        assert!(state.tooltip_visible());
        assert_eq!(left(&frame), before, "{label}");
        let row = frame_rows(&frame)[target.rect.y as usize].clone();
        let shown = row.chars().skip(target.rect.x as usize).collect::<String>();
        if label.len() < 100 {
            assert!(shown.starts_with(&label), "{row}");
        } else {
            // Cut at the screen edge instead of moving left.
            assert!(shown.trim_end().ends_with("x…"), "{row}");
        }
    }
}

#[test]
fn the_spaces_list_keeps_its_top_space_when_squares_above_fold() {
    let mut state = state_with_tabs(true);
    for index in 0..40 {
        with_job(&mut state, &format!("job_{index}"), TabStatus::Running);
    }
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for index in 2..=12 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.label = format!("space-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    state.set_snapshot(Box::new(projected));
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    // Scroll ws_2's name row to the top.
    let body = state.hits.workspace_body;
    let ws_2 = state
        .hits
        .workspace_layout
        .iter()
        .find(|layout| layout.workspace_id == "ws_2")
        .expect("ws_2")
        .top;
    state.workspace_scroll += (ws_2 - i32::from(body.y)) as usize;
    state.compose(106, 30).unwrap();
    let top_space = |state: &ClientShellState| {
        state
            .hits
            .workspace_layout
            .iter()
            .find(|layout| layout.top == i32::from(body.y))
            .map(|layout| layout.workspace_id.clone())
    };
    assert_eq!(top_space(&state).as_deref(), Some("ws_2"));

    // ws_1's squares fold (all its jobs close): ws_2 stays at the top.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs.retain(|tab| tab.parent_tab_id.is_none());
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    assert_eq!(top_space(&state).as_deref(), Some("ws_2"));
}

#[test]
fn the_notification_history_lists_and_opens_past_notifications() {
    let mut state = state_with_tabs(true);
    let entry = |id: u64, title: &str, tab: &str| crate::api::schema::NotificationRecord {
        id,
        unix_ms: 1_790_633_100_000,
        kind: "finished".into(),
        title: title.into(),
        body: None,
        agent: None,
        workspace_id: Some("ws_1".into()),
        tab_id: Some(tab.into()),
        pane_id: None,
        task: None,
        request: None,
        repeats: None,
    };
    // The shown tab's notification is read at once, another tab's counts.
    state.notification_log_received(Some("tab_1"));
    state.notification_log_received(Some("tab_9"));
    let frame = state.compose(106, 30).unwrap();
    let button = state.hits.notification_log_button;
    let header = frame_rows(&frame)[button.y as usize].clone();
    assert!(header.contains("✉1"), "{header:?}");

    // Opening it fetches the list; nothing is fetched before.
    let outcome = left_click(&mut state, (button.x + 1, button.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::NotificationList(_)))));
    state.complete_notification_list(
        ClientEndpointId::Local,
        Ok(crate::api::schema::ResponseResult::NotificationList {
            notifications: vec![
                entry(1, "on this tab", "tab_1"),
                entry(2, "elsewhere", "tab_9"),
            ],
        }),
    );
    let frame = state.compose(106, 30).unwrap();
    let rows = state.hits.notification_log_rows.clone();
    assert_eq!(rows.len(), 2);
    // Newest first; no row is highlighted until one is picked; the unread
    // row is marked, the read row is not.
    // Only the list's own columns: a day separator moves the rows down, onto
    // lines where the sidebar draws its own `▌`.
    let in_row = |rect: Rect| {
        frame_rows(&frame)[rect.y as usize]
            .chars()
            .skip(rect.x as usize)
            .take(rect.width as usize)
            .collect::<String>()
    };
    let first = in_row(rows[0].0);
    assert!(
        !first.contains('▌') && first.contains('•') && first.contains("elsewhere"),
        "{first:?}"
    );
    let second = in_row(rows[1].0);
    assert!(
        !second.contains('•') && second.contains("on this tab"),
        "{second:?}"
    );

    // Its tab is gone, so the space opens; the entry is read.
    let outcome = left_click(&mut state, (rows[0].0.x + 3, rows[0].0.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(target)
                if target.workspace_id == "ws_1"))));
    assert!(state.overlay.is_none());
    assert_eq!(state.notification_log_button(), Some(0));
}

#[test]
fn the_history_counts_unread_tabs_and_shows_what_happened_then() {
    let mut state = state_with_tabs(true);
    let entry = |id: u64, kind: &str, tab: &str| crate::api::schema::NotificationRecord {
        id,
        unix_ms: 1_790_633_100_000,
        kind: kind.into(),
        title: format!("entry {id}"),
        body: None,
        agent: None,
        workspace_id: Some("ws_1".into()),
        tab_id: Some(tab.into()),
        pane_id: None,
        task: None,
        request: None,
        repeats: None,
    };
    // Two arrivals for one unvisited tab are one unread tab.
    state.notification_log_received(Some("tab_9"));
    state.notification_log_received(Some("tab_9"));
    assert_eq!(state.notification_log_button(), Some(1));

    // Newest first: only the newest row of the unread tab is marked.
    let rows = vec![
        entry(3, "asking", "tab_9"),
        entry(2, "finished", "tab_9"),
        entry(1, "finished", "tab_1"),
    ];
    assert_eq!(
        state.notification_unread_rows(&rows),
        vec![true, false, false]
    );

    // `tab_1` works now, but its row says it finished then.
    let style = state.config.status_indicators;
    let icons = state.notification_row_icons(&rows);
    assert_eq!(icons[0].map(|(icon, _)| icon), Some("?"));
    let done = super::super::AgentMark::None;
    assert_eq!(
        icons[2],
        Some((
            super::super::agent_icon(AgentStatus::Done, done, style),
            super::super::agent_color(AgentStatus::Done, done, &state.config.palette),
        ))
    );
}

#[test]
fn a_new_focused_tab_low_in_a_tall_space_scrolls_into_view() {
    let mut state = state_with_tabs(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for index in 2..=30 {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = format!("tab_{index}");
        tab.label = format!("tab {index}");
        tab.focused = false;
        projected.tabs.push(tab);
    }
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    assert_eq!(state.workspace_scroll, 0);
    // A tab is created at the end and focused.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    let mut new_tab = projected.tabs[0].clone();
    new_tab.tab_id = "tab_new".into();
    new_tab.label = "new tab".into();
    projected.tabs.push(new_tab);
    for tab in &mut projected.tabs {
        tab.focused = tab.tab_id == "tab_new";
    }
    projected.focused_tab_id = Some("tab_new".into());
    projected.workspaces[0].active_tab_id = "tab_new".into();
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    let body = state.hits.workspace_body;
    let (line, _) = state
        .hits
        .space_tabs
        .iter()
        .find(|(_, tab_id)| tab_id == "tab_new")
        .cloned()
        .expect("the new tab's line is drawn");
    assert!(line.y >= body.y && line.bottom() <= body.bottom());
}

#[test]
fn the_tab_menu_closes_its_jobs_by_state_from_chips() {
    let mut state = state_with_tabs(true);
    state.config.confirm_close = false;
    with_job(&mut state, "job_run", TabStatus::Running);
    with_job(&mut state, "job_fail", TabStatus::Failed);
    with_job(&mut state, "job_ok", TabStatus::Succeeded);
    let closes = |outcome: &ClientShellInput| {
        outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => match &request.method {
                    crate::api::schema::Method::TabClose(target) => Some(target.tab_id.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let open_menu = |state: &mut ClientShellState| {
        state.compose(106, 30).unwrap();
        let (line, _) = state.hits.space_tabs[0];
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
            column: line.x + 6,
            row: line.y,
            modifiers: KeyModifiers::empty(),
        })]);
        let frame = state.compose(106, 30).unwrap();
        (frame_rows(&frame), state.hits.context_menu_rows.clone())
    };
    let chip = |rows: &[String], hits: &[(Rect, usize)], text: &str| {
        hits.iter()
            .find(|(rect, _)| {
                rows[rect.y as usize]
                    .chars()
                    .skip(rect.x as usize)
                    .take(rect.width as usize)
                    .collect::<String>()
                    .contains(text)
            })
            .map(|(rect, _)| *rect)
            .expect(text)
    };

    let (rows, hits) = open_menu(&mut state);
    let chips_row = chip(&rows, &hits, "!1").y as usize;
    assert!(
        rows[chips_row].contains("Close jobs:  ◐ 1   !1   ✓1 "),
        "{}",
        rows[chips_row]
    );
    // A finished state closes at once, only its own jobs.
    let failed = chip(&rows, &hits, "!1");
    assert_eq!(
        closes(&left_click(&mut state, (failed.x + 1, failed.y))),
        ["job_fail"]
    );

    // Running jobs ask first, then close only them, not the tab.
    let (rows, hits) = open_menu(&mut state);
    let running = chip(&rows, &hits, "◐ 1");
    assert!(closes(&left_click(&mut state, (running.x + 1, running.y))).is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(ref confirm)) if confirm.title == "Stop 1 running job?"
    ));
    let mut outcome = ClientShellInput::default();
    state.accept_close_confirmation(&mut outcome);
    assert_eq!(closes(&outcome), ["job_run"]);
}

/// Three top-level tabs in `ws_1`: `tab_1`, `tab_2`, `tab_3`.
fn state_with_three_tabs() -> ClientShellState {
    let mut state = state_with_tabs(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for id in ["tab_2", "tab_3"] {
        let mut tab = projected.tabs[0].clone();
        tab.tab_id = id.into();
        tab.focused = false;
        projected.tabs.push(tab);
    }
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    state
}

fn line_row(state: &ClientShellState, tab_id: &str) -> (u16, u16) {
    let (rect, _) = state
        .hits
        .space_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .expect("tab line");
    (rect.x + 6, rect.y)
}

/// The tab lines' ids from the top row down, as drawn.
fn drawn_order(state: &ClientShellState) -> Vec<String> {
    let mut lines = state.hits.space_tabs.clone();
    lines.sort_by_key(|(rect, _)| rect.y);
    lines.into_iter().map(|(_, id)| id).collect()
}

fn tab_moves(outcome: &ClientShellInput) -> Vec<(String, usize)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::TabMove(params) => {
                    Some((params.tab_id.clone(), params.insert_index))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn a_tab_line_dragged_down_past_the_others_moves_to_the_end() {
    let mut state = state_with_three_tabs();
    let (first, third) = (line_row(&state, "tab_1"), line_row(&state, "tab_3"));
    assert!(left_click(&mut state, first).actions.is_empty());
    left_drag(&mut state, third);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::TabLine {
            insert_index: Some(3),
            ..
        })
    ));
    // The list already shows the tab where it would land, and the header
    // says which tab moves and where.
    let frame = state.compose(106, 30).unwrap();
    assert_eq!(drawn_order(&state), ["tab_2", "tab_3", "tab_1"]);
    let rows = frame_rows(&frame);
    assert!(
        rows.iter()
            .any(|row| row.contains("1 → 3 · agent tab · at")),
        "{rows:?}"
    );
    let released = left_release(&mut state, third);
    assert_eq!(tab_moves(&released), [("tab_1".to_string(), 3)]);
    assert!(!focuses(&released, "tab_1"), "a drop does not also click");
}

#[test]
fn a_tab_line_dragged_up_lands_before_the_line_under_the_pointer() {
    let mut state = state_with_three_tabs();
    let (second, third) = (line_row(&state, "tab_2"), line_row(&state, "tab_3"));
    left_click(&mut state, third);
    left_drag(&mut state, second);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::TabLine {
            insert_index: Some(1),
            ..
        })
    ));
    let frame = state.compose(106, 30).unwrap();
    assert_eq!(drawn_order(&state), ["tab_1", "tab_3", "tab_2"]);
    assert!(
        frame_rows(&frame)
            .iter()
            .any(|row| row.contains("3 → 2 · agent tab")),
        "{:?}",
        frame_rows(&frame)
    );
    assert_eq!(
        tab_moves(&left_release(&mut state, second)),
        [("tab_3".to_string(), 1)]
    );
}

#[test]
fn a_tab_line_dropped_at_its_own_place_sends_nothing() {
    let mut state = state_with_three_tabs();
    let (second, third) = (line_row(&state, "tab_2"), line_row(&state, "tab_3"));
    left_click(&mut state, second);
    left_drag(&mut state, third);
    left_drag(&mut state, second);
    let released = left_release(&mut state, second);
    assert!(tab_moves(&released).is_empty());
    assert!(state.chrome_drag.is_none());
}

#[test]
fn sideways_movement_alone_does_not_start_a_tab_line_drag() {
    let mut state = state_with_three_tabs();
    let first = line_row(&state, "tab_1");
    left_click(&mut state, first);
    left_drag(&mut state, (first.0 + 4, first.1));
    assert!(state.chrome_drag.is_none());
    // Still a click: the tab opens on release.
    assert!(focuses(
        &left_release(&mut state, (first.0 + 4, first.1)),
        "tab_1"
    ));
}

#[test]
fn escape_cancels_a_tab_line_drag_and_the_pointer_outside_the_space_says_so() {
    let mut state = state_with_three_tabs();
    let (first, third) = (line_row(&state, "tab_1"), line_row(&state, "tab_3"));
    left_click(&mut state, first);
    left_drag(&mut state, third);
    // Above the space's first line: nothing to land on, nothing clamped.
    left_drag(&mut state, (first.0, 0));
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::TabLine {
            insert_index: None,
            ..
        })
    ));
    state.handle_input_bytes(b"\x1b");
    assert!(state.chrome_drag.is_none());
    assert!(tab_moves(&left_release(&mut state, third)).is_empty());
    // A drag released outside the space cancels too.
    left_click(&mut state, first);
    left_drag(&mut state, third);
    left_drag(&mut state, (first.0, 0));
    assert!(tab_moves(&left_release(&mut state, (first.0, 0))).is_empty());
}

#[test]
fn the_drop_slot_is_measured_against_the_rows_frozen_at_the_drag_start() {
    let mut state = state_with_three_tabs();
    let first = line_row(&state, "tab_1");
    left_click(&mut state, first);
    // Walk down past every line and back up: the slot depends on the pointer
    // alone, so it never flickers although the drawn list reorders.
    let mut down = Vec::new();
    for step in 1..=2u16 {
        left_drag(&mut state, (first.0, first.1 + step));
        state.compose(106, 30).unwrap();
        down.push(slot(&state));
    }
    let mut up = Vec::new();
    for step in (0..=1u16).rev() {
        left_drag(&mut state, (first.0, first.1 + step));
        state.compose(106, 30).unwrap();
        up.push(slot(&state));
    }
    assert_eq!(down, [Some(2), Some(3)]);
    assert_eq!(up, [Some(2), Some(0)]);
}

fn slot(state: &ClientShellState) -> Option<usize> {
    match &state.chrome_drag {
        Some(ClientChromeDrag::TabLine { insert_index, .. }) => *insert_index,
        _ => None,
    }
}

#[test]
fn the_drag_hint_names_the_tab_and_where_it_lands() {
    let mut state = state_with_three_tabs();
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for (tab, label) in projected.tabs.iter_mut().zip(["build", "tests", "review"]) {
        tab.label = label.into();
    }
    state.set_snapshot(Box::new(projected));
    let snapshot = state.snapshot.as_deref().expect("snapshot");
    let hint = |slot| crate::client::shell::space_tabs::tab_drag_hint(snapshot, "tab_2", slot);
    assert_eq!(hint(None), "release cancels · Esc");
    assert_eq!(hint(Some(1)), "no change · tests · Esc");
    assert_eq!(hint(Some(2)), "no change · tests · Esc");
    assert_eq!(hint(Some(0)), "2 → 1 · tests · before build");
    assert_eq!(hint(Some(3)), "2 → 3 · tests · at the end");
    let hint = |slot| crate::client::shell::space_tabs::tab_drag_hint(snapshot, "tab_1", slot);
    assert_eq!(hint(Some(2)), "1 → 2 · build · before review");
}

/// `state_with_three_tabs` with distinct tab labels: build, tests, review.
fn state_with_named_tabs() -> ClientShellState {
    let mut state = state_with_three_tabs();
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for (tab, label) in projected.tabs.iter_mut().zip(["build", "tests", "review"]) {
        tab.label = label.into();
        tab.custom_label = true;
    }
    state.set_snapshot(Box::new(projected));
    state
}

fn type_text(state: &mut ClientShellState, text: &str) {
    state.handle_input_bytes(text.as_bytes());
}

fn shown_tabs(state: &mut ClientShellState) -> Vec<String> {
    state.compose(106, 30).unwrap();
    drawn_order(state)
}

#[test]
fn the_filter_bar_opens_from_its_button_and_narrows_the_list_as_you_type() {
    let mut state = state_with_named_tabs();
    state.compose(106, 30).unwrap();
    assert!(
        !state.hits.space_filter_button.is_empty(),
        "the header has a button"
    );
    let button = state.hits.space_filter_button;
    left_click(&mut state, (button.x + 1, button.y));
    assert!(state.space_filter.open && state.space_filter.focused);
    // Nothing typed yet: every tab is listed.
    assert_eq!(shown_tabs(&mut state), ["tab_1", "tab_2", "tab_3"]);
    // `rev` fits only the review tab (the space and its branch do not match).
    type_text(&mut state, "rev");
    assert_eq!(state.space_filter.query, "rev");
    assert_eq!(shown_tabs(&mut state), ["tab_3"]);
    let rows = frame_rows(&state.compose(106, 30).unwrap());
    assert!(rows.iter().any(|row| row.contains("/ rev")), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.contains("1/1")),
        "the count: {rows:?}"
    );
    // The matched characters of the label are underlined, the others not.
    let frame = state.compose(106, 30).unwrap();
    let line = state.hits.space_tabs[0].0;
    let row =
        &frame.cells[usize::from(line.y) * usize::from(frame.width)..][..usize::from(frame.width)];
    let underlined = |symbol: &str| {
        row.iter()
            .find(|cell| cell.symbol == symbol)
            .map(|cell| cell.modifier & ratatui::style::Modifier::UNDERLINED.bits() != 0)
    };
    assert_eq!(underlined("r"), Some(true));
    assert_eq!(underlined("i"), Some(false));
    // The space's own name shows all its tabs.
    state.space_filter.query = "client".into();
    assert_eq!(shown_tabs(&mut state), ["tab_1", "tab_2", "tab_3"]);
    // No match says so and lists nothing.
    state.space_filter.query = "zzz".into();
    let rows = frame_rows(&state.compose(106, 30).unwrap());
    assert!(state.hits.space_tabs.is_empty());
    assert!(rows.iter().any(|row| row.contains("no match")), "{rows:?}");
}

#[test]
fn escape_clears_the_text_then_closes_and_enter_opens_the_first_match() {
    let mut state = state_with_named_tabs();
    state.space_filter.open = true;
    state.space_filter.focused = true;
    type_text(&mut state, "tes");
    state.handle_input_bytes(b"\x1b");
    assert!(state.space_filter.open && state.space_filter.query.is_empty());
    state.handle_input_bytes(b"\x1b");
    assert!(!state.space_filter.open);

    state.space_filter.open = true;
    state.space_filter.focused = true;
    type_text(&mut state, "tes");
    let outcome = state.handle_input_bytes(b"\r");
    assert!(focuses(&outcome, "tab_2"), "the matching tab opens");
    assert!(!state.space_filter.open, "choosing closes the bar");

    // A query that matches the space opens the space.
    state.space_filter.open = true;
    state.space_filter.focused = true;
    type_text(&mut state, "client");
    let outcome = state.handle_input_bytes(b"\r");
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(target)
                if target.workspace_id == "ws_1"))));
}

#[test]
fn a_blurred_filter_leaves_the_keys_to_the_pane_and_a_press_blurs_it() {
    let mut state = state_with_named_tabs();
    state.space_filter.open = true;
    state.space_filter.focused = true;
    state.compose(106, 30).unwrap();
    // A click on a pane gives the keys back and leaves the filter on.
    left_click(&mut state, (60, 10));
    assert!(state.space_filter.open && !state.space_filter.focused);
    type_text(&mut state, "x");
    assert!(state.space_filter.query.is_empty(), "the pane got the key");
    // The bar takes them again, and its cross closes it.
    state.compose(106, 30).unwrap();
    let bar = state.hits.space_filter_bar;
    left_click(&mut state, (bar.x + 4, bar.y));
    assert!(state.space_filter.focused);
    state.compose(106, 30).unwrap();
    let close = state.hits.space_filter_close;
    left_click(&mut state, (close.x + 1, close.y));
    assert!(!state.space_filter.open);
}

#[test]
fn tabs_are_not_dragged_while_the_list_is_filtered() {
    let mut state = state_with_named_tabs();
    state.space_filter.open = true;
    state.space_filter.query = "e".into();
    state.compose(106, 30).unwrap();
    let first = state.hits.space_tabs[0].0;
    left_click(&mut state, (first.x + 6, first.y));
    left_drag(&mut state, (first.x + 6, first.y + 1));
    assert!(state.chrome_drag.is_none());
}

#[test]
fn working_and_job_glyphs_turn_with_the_clock_and_stand_still_when_animations_are_off() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    // The agent works and a job runs: something turns, so the timer runs.
    state.compose(106, 30).unwrap();
    assert!(state.motion_active);
    let delay = state.timer_delay(state.motion_epoch);
    assert!(delay <= std::time::Duration::from_millis(100));
    let row = |state: &mut ClientShellState| {
        let frame = state.compose(106, 30).unwrap();
        frame_rows(&frame)[state.hits.space_tabs[0].0.y as usize].clone()
    };
    let first = row(&mut state);
    assert!(first.contains("◐ ▌agent tab"), "{first}");
    // 160 ms later both circles have turned once, in opposite directions.
    assert!(state.tick_motion(state.motion_epoch + std::time::Duration::from_millis(170)));
    let second = row(&mut state);
    assert!(second.contains("◓ ▌agent tab"), "{second}");
    assert!(second.contains("◒ 1"), "the job loop turned too: {second}");
    // A tick inside the same frame changes nothing, so nothing repaints.
    assert!(!state.tick_motion(state.motion_epoch + std::time::Duration::from_millis(180)));
    // Animations off: static glyphs, no timer.
    state.config.animations = false;
    let still = row(&mut state);
    assert!(
        still.contains("◐ ▌agent tab") && still.contains("◑ 1"),
        "{still}"
    );
    assert!(!state.motion_active);
}

#[test]
fn nothing_turns_without_a_working_agent_or_a_running_job() {
    let mut state = state_with_tabs(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for agent in &mut projected.agents {
        agent.agent_status = AgentStatus::Idle;
    }
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    assert!(!state.motion_active);
    assert!(!state.tick_motion(state.motion_epoch + std::time::Duration::from_secs(5)));
    assert_eq!(
        state.timer_delay(state.motion_epoch),
        std::time::Duration::from_millis(100)
    );
}

#[test]
fn the_waiting_icon_and_the_job_count_turn_in_step() {
    let mut state = state_with_tabs(true);
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for agent in &mut projected.agents {
        agent.agent_status = AgentStatus::Idle;
    }
    projected.tabs[0].agent_status = AgentStatus::Idle;
    state.set_snapshot(Box::new(projected));
    with_job(&mut state, "job_1", TabStatus::Running);
    state.compose(106, 30).unwrap();
    assert!(state.motion_active);
    let mut seen = Vec::new();
    for ms in [0u64, 160, 320, 480, 640] {
        state.tick_motion(state.motion_epoch + std::time::Duration::from_millis(ms + 5));
        let frame = state.compose(106, 30).unwrap();
        let row = frame_rows(&frame)[state.hits.space_tabs[0].0.y as usize].clone();
        let glyphs = row
            .chars()
            .filter(|c| "◐◓◑◒".contains(*c))
            .collect::<String>();
        seen.push(glyphs);
    }
    // Icon first, count last; the same glyph in both at every instant.
    for glyphs in &seen {
        let chars = glyphs.chars().collect::<Vec<_>>();
        assert_eq!(chars.first(), chars.last(), "{seen:?}");
    }
    // Counter-clockwise, one frame every 160 ms.
    assert_eq!(seen[0].chars().next(), Some('◐'));
    assert_eq!(seen[1].chars().next(), Some('◒'));
    assert_eq!(seen[2].chars().next(), Some('◑'));
}

#[test]
fn arrows_move_the_filter_selection_and_enter_opens_the_selected_space() {
    let mut state = state_with_named_tabs();
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for (id, label) in [("ws_2", "notes"), ("ws_3", "client-docs")] {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = id.into();
        workspace.label = label.into();
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    state.set_snapshot(Box::new(projected));
    state.space_filter.open = true;
    state.space_filter.focused = true;
    state.compose(106, 30).unwrap();
    // Down from the first space selects the second; Up goes back, and the
    // ends do not wrap.
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(state.space_filter.selected.as_deref(), Some("ws_2"));
    state.handle_input_bytes(b"\x1b[B");
    state.handle_input_bytes(b"\x1b[B");
    assert_eq!(state.space_filter.selected.as_deref(), Some("ws_3"));
    state.handle_input_bytes(b"\x1b[A");
    assert_eq!(state.space_filter.selected.as_deref(), Some("ws_2"));
    state.compose(106, 30).unwrap();
    // Typing narrows the list and selects its first space again.
    type_text(&mut state, "docs");
    assert_eq!(state.space_filter.selected.as_deref(), Some("ws_3"));
    // Enter opens the selected space.
    let outcome = state.handle_input_bytes(b"\r");
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(target)
                if target.workspace_id == "ws_3"))));
    assert!(!state.space_filter.open);
}

#[test]
fn the_filter_spaces_key_opens_the_bar_even_with_the_sidebar_collapsed() {
    let mut state = state_with_named_tabs();
    state.sidebar_collapsed = true;
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FilterSpaces),
        &mut outcome,
    );
    assert!(state.space_filter.open && state.space_filter.focused);
    assert!(!state.sidebar_collapsed);
    assert!(outcome.repaint && outcome.resize);
    // The resize dropped the pane surface; the next one brings it back.
    state.set_pane_surface(surface());
    type_text(&mut state, "rev");
    assert_eq!(state.space_filter.query, "rev");
    assert_eq!(shown_tabs(&mut state), ["tab_3"]);
}

fn endpoint_requests(outcome: &ClientShellInput) -> Vec<(String, crate::api::schema::Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

fn accept(state: &mut ClientShellState, id: &str) -> Vec<ClientShellAction> {
    let boot = state.snapshot.as_deref().expect("snapshot").boot_id.clone();
    state
        .handle_endpoint_result(&boot, id, Ok(crate::api::schema::ResponseResult::Ok {}))
        .1
}

/// Closes `tab_id` as the user does and has the server accept it.
fn close_tab_accepted(state: &mut ClientShellState, tab_id: &str) {
    let mut outcome = ClientShellInput::default();
    state.config.confirm_close = false;
    state.request_tab_close(tab_id.into(), &mut outcome);
    let [(id, crate::api::schema::Method::TabClose(target))] = &endpoint_requests(&outcome)[..]
    else {
        panic!("expected one tab close: {:?}", outcome.actions);
    };
    assert_eq!(target.tab_id, tab_id);
    accept(state, id);
}

fn reopen(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::ReopenTab),
        &mut outcome,
    );
    outcome
}

#[test]
fn a_closed_tab_reopens_with_its_directory_and_own_name_in_its_place() {
    let mut state = state_with_named_tabs();
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        tab_id: "tab_2".into(),
        cwd: Some("/work/tests".into()),
        foreground_cwd: None,
        ..projected.panes[0].clone()
    });
    state.set_snapshot(Box::new(projected));
    close_tab_accepted(&mut state, "tab_2");
    assert_eq!(state.closed_tabs.len(), 1);
    // Gone from the snapshot, as the server would project it.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs.retain(|tab| tab.tab_id != "tab_2");
    state.set_snapshot(Box::new(projected));

    let outcome = reopen(&mut state);
    let requests = endpoint_requests(&outcome);
    let [(create_id, crate::api::schema::Method::TabCreate(params))] = &requests[..] else {
        panic!("expected one tab create: {:?}", outcome.actions);
    };
    assert_eq!(params.workspace_id.as_deref(), Some("ws_1"));
    assert_eq!(params.cwd.as_deref(), Some("/work/tests"));
    assert_eq!(params.label.as_deref(), Some("tests"));
    assert!(params.focus);
    assert!(state.closed_tabs.is_empty(), "used up when sent");
    // A second press has nothing to reopen.
    assert!(endpoint_requests(&reopen(&mut state)).is_empty());

    // The server created the tab; it goes back after `build`, the tab that
    // stood before it, ahead of `review`.
    let boot = state.snapshot.as_deref().expect("snapshot").boot_id.clone();
    let tab = crate::api::schema::TabInfo {
        activity: None,
        bookmarked: false,
        job: None,
        tab_id: "tab_9".into(),
        workspace_id: "ws_1".into(),
        number: 9,
        label: "tests".into(),
        focused: true,
        pane_count: 1,
        agent_status: AgentStatus::Unknown,
        parent_tab_id: None,
        status: None,
    };
    let root_pane = crate::api::schema::PaneInfo {
        pane_id: "pane_9".into(),
        terminal_id: "term_9".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_9".into(),
        focused: true,
        cwd: None,
        foreground_cwd: None,
        restore_error: None,
        label: None,
        agent: None,
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        display_agent: None,
        agent_status: AgentStatus::Unknown,
        state_labels: Default::default(),
        tokens: Default::default(),
        agent_session: None,
        scroll: None,
        revision: 0,
    };
    let (_, actions) = state.handle_endpoint_result(
        &boot,
        create_id,
        Ok(crate::api::schema::ResponseResult::TabCreated { tab, root_pane }),
    );
    let moves = actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::TabMove(params) => {
                    Some((params.tab_id.clone(), params.insert_index))
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(moves, [("tab_9".to_string(), 1)]);
}

#[test]
fn a_refused_close_is_not_remembered_and_a_parent_counts_its_jobs() {
    let mut state = state_with_three_tabs();
    with_job(&mut state, "job_1", TabStatus::Failed);
    state.config.confirm_close = false;
    // A tab with a job under it comes back without the job, and knows it.
    let parent = state.closed_tab_record("tab_1").expect("parent recorded");
    assert_eq!((parent.jobs, parent.job), (1, false));
    // A job closed on its own leaves an entry that only says it cannot.
    let job = state.closed_tab_record("job_1").expect("job recorded");
    assert!(job.job);
    // A refused close leaves no entry.
    let mut outcome = ClientShellInput::default();
    state.request_tab_close("tab_3".into(), &mut outcome);
    let [(id, _)] = &endpoint_requests(&outcome)[..] else {
        panic!("one request");
    };
    let boot = state.snapshot.as_deref().expect("snapshot").boot_id.clone();
    state.handle_endpoint_result(
        &boot,
        id,
        Err(ClientShellEndpointError {
            code: Some("tab_close_failed".into()),
            message: "no".into(),
        }),
    );
    assert!(state.closed_tabs.is_empty());
}

#[test]
fn reopening_skips_a_vanished_space_keeps_ten_and_reuses_nothing_twice() {
    let mut state = state_with_three_tabs();
    for n in 0..12 {
        state.remember_closed_tab(crate::client::shell::closed_tabs::ClosedTab {
            endpoint_id: ClientEndpointId::Local,
            workspace_id: if n == 11 { "ws_gone" } else { "ws_1" }.into(),
            label: Some(format!("t{n}")),
            cwd: None,
            after_tab_id: None,
            jobs: 0,
            job: false,
        });
    }
    assert_eq!(state.closed_tabs.len(), 10);
    // The newest entry's space is gone: it is skipped, the next one reopens.
    let outcome = reopen(&mut state);
    let requests = endpoint_requests(&outcome);
    let [(_, crate::api::schema::Method::TabCreate(params))] = &requests[..] else {
        panic!("expected a tab create");
    };
    assert_eq!(params.label.as_deref(), Some("t10"));
}

#[test]
fn focusing_a_job_leaves_its_parents_squares_folded_and_shows_the_job_instead() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Running);
    state.compose(106, 30).unwrap();
    assert!(state.hits.space_tab_squares.is_empty(), "folded by default");
    // Focus moves to the job and back, as clicks between its row and the
    // parent do: the squares stay folded, the job keeps one row of its own.
    for tab in ["job_1", "tab_1", "job_1", "tab_1"] {
        focus_tab(&mut state, tab);
        state.compose(106, 30).unwrap();
        assert!(
            state
                .unfolded_squares
                .get(&ClientEndpointId::Local)
                .is_none_or(|set| set.is_empty()),
            "{tab}"
        );
        assert_eq!(state.hits.space_tab_squares.len(), 1, "{tab}");
        assert!(state.hits.space_tab_squares[0].0.width > 3, "{tab}");
    }
}

#[test]
fn reopening_a_closed_job_says_so_instead_of_an_older_tab() {
    // The user closed a tab, then a job, and pressed reopen: an older,
    // unrelated tab came back. Now the first press explains, the second
    // reopens the older tab.
    let mut state = state_with_three_tabs();
    let entry = |label: &str, job: bool| crate::client::shell::closed_tabs::ClosedTab {
        endpoint_id: ClientEndpointId::Local,
        workspace_id: "ws_1".into(),
        label: Some(label.into()),
        cwd: None,
        after_tab_id: None,
        jobs: 0,
        job,
    };
    state.remember_closed_tab(entry("older", false));
    state.remember_closed_tab(entry("build", true));

    assert!(endpoint_requests(&reopen(&mut state)).is_empty());
    let notice = state.visible_endpoint_notice.as_ref().expect("a notice");
    assert!(
        notice.body.contains("job build cannot be reopened"),
        "{}",
        notice.body
    );

    let requests = endpoint_requests(&reopen(&mut state));
    let [(_, crate::api::schema::Method::TabCreate(params))] = &requests[..] else {
        panic!("expected a tab create");
    };
    assert_eq!(params.label.as_deref(), Some("older"));
}

#[test]
fn every_press_with_nothing_to_reopen_answers() {
    let mut state = state_with_three_tabs();
    for _ in 0..2 {
        state.visible_endpoint_notice = None;
        assert!(endpoint_requests(&reopen(&mut state)).is_empty());
        assert!(
            state.visible_endpoint_notice.is_some(),
            "a notice every time"
        );
    }
}

#[test]
fn reopening_keeps_entries_it_cannot_use_yet() {
    let mut state = state_with_three_tabs();
    let entry = |endpoint: ClientEndpointId, workspace: &str, label: &str| {
        crate::client::shell::closed_tabs::ClosedTab {
            endpoint_id: endpoint,
            workspace_id: workspace.into(),
            label: Some(label.into()),
            cwd: None,
            after_tab_id: None,
            jobs: 0,
            job: false,
        }
    };
    let remote = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("00000000000000000000000000000002")
            .expect("profile id"),
    );
    state.remember_closed_tab(entry(remote.clone(), "ws_9", "elsewhere"));
    state.remember_closed_tab(entry(ClientEndpointId::Local, "ws_gone", "gone"));
    state.remember_closed_tab(entry(ClientEndpointId::Local, "ws_1", "usable"));
    // The usable entry opens; the one of this machine's vanished space is dropped;
    // the other machine's entry waits for that machine.
    let outcome = reopen(&mut state);
    let requests = endpoint_requests(&outcome);
    let [(_, crate::api::schema::Method::TabCreate(params))] = &requests[..] else {
        panic!("expected a tab create");
    };
    assert_eq!(params.label.as_deref(), Some("usable"));
    let kept = state
        .closed_tabs
        .iter()
        .map(|closed| closed.label.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(kept, ["elsewhere"]);
    // Nothing left for this machine: a notice, and the other entry stays.
    assert!(endpoint_requests(&reopen(&mut state)).is_empty());
    assert_eq!(state.closed_tabs.len(), 1);
}

#[test]
fn pasted_text_goes_into_the_focused_filter_and_not_into_the_pane() {
    let mut state = state_with_named_tabs();
    state.space_filter.open = true;
    state.space_filter.focused = true;
    let outcome = state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Paste(
        "rev\niew ".into(),
    )]);
    assert_eq!(state.space_filter.query, "rev iew");
    assert!(outcome.repaint);
    assert!(
        outcome.requests.is_empty() && outcome.actions.is_empty(),
        "nothing reaches the pane"
    );
    // Blurred, the paste is the pane's again.
    state.space_filter.focused = false;
    state.space_filter.query.clear();
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Paste("x".into())]);
    assert!(state.space_filter.query.is_empty());
}

#[test]
fn a_pending_prefix_keeps_its_key_from_the_filter_bar() {
    let mut state = state_with_named_tabs();
    state.space_filter.open = true;
    state.space_filter.focused = true;
    state.mode = ClientShellMode::Prefix;
    type_text(&mut state, "u");
    assert!(state.space_filter.query.is_empty());
}

#[test]
fn a_tab_line_drag_cancels_when_the_spaces_tabs_change_under_it() {
    let mut state = state_with_three_tabs();
    let (first, third) = (line_row(&state, "tab_1"), line_row(&state, "tab_3"));
    left_click(&mut state, first);
    left_drag(&mut state, third);
    assert_eq!(slot(&state), Some(3));
    // Another client closes `tab_2` while the drag is on.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs.retain(|tab| tab.tab_id != "tab_2");
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    left_drag(&mut state, third);
    assert_eq!(slot(&state), None, "rows no longer match the tabs");
    assert!(tab_moves(&left_release(&mut state, third)).is_empty());
}

#[test]
fn history_rows_say_what_finished_and_in_which_space() {
    let state = state_with_tabs(true);
    let record = |kind: &str, task: Option<&str>, repeats: Option<u32>| {
        crate::api::schema::NotificationRecord {
            id: 1,
            unix_ms: 0,
            kind: kind.into(),
            title: format!(
                "claude {}",
                if kind == "finished" {
                    "finished"
                } else {
                    "needs attention"
                }
            ),
            body: Some("client-shell · 1 · 3".into()),
            agent: Some("claude".into()),
            workspace_id: Some("ws_1".into()),
            tab_id: Some("tab_1".into()),
            pane_id: Some("pane_1".into()),
            task: task.map(str::to_owned),
            request: None,
            repeats,
        }
    };
    // The task first, then the agent and the space; no position, no tab number.
    assert_eq!(
        state.notification_row_text(&record("finished", Some("Fix the login test"), Some(3))),
        "✓ Fix the login test · claude · client-shell ×3"
    );
    assert_eq!(
        state.notification_row_text(&record("needs_attention", Some("Fix the login test"), None)),
        "? Fix the login test · claude · client-shell"
    );
    // Without a task the row names the event.
    assert_eq!(
        state.notification_row_text(&record("finished", None, None)),
        "✓ claude finished · client-shell"
    );
    // Other kinds keep their title and body.
    let mut update = record("update_installed", None, None);
    update.title = "herdr updated".into();
    update.body = Some("0.9.4".into());
    assert_eq!(
        state.notification_row_text(&update),
        "herdr updated · 0.9.4"
    );
}

#[test]
fn the_notification_list_grows_to_the_right_to_show_a_long_row_whole() {
    let mut state = state_with_tabs(true);
    state.notification_log_received(Some("tab_9"));
    state.compose(106, 30).unwrap();
    let button = state.hits.notification_log_button;
    left_click(&mut state, (button.x + 1, button.y));
    state.complete_notification_list(
        ClientEndpointId::Local,
        Ok(crate::api::schema::ResponseResult::NotificationList {
            notifications: vec![crate::api::schema::NotificationRecord {
                id: 1,
                unix_ms: 1_790_633_100_000,
                kind: "finished".into(),
                title: "claude finished".into(),
                body: None,
                agent: Some("claude".into()),
                workspace_id: Some("ws_1".into()),
                tab_id: Some("tab_9".into()),
                pane_id: None,
                task: Some("Rewrite the history so each row names its task and more".into()),
                request: None,
                repeats: Some(2),
            }],
        }),
    );
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    // The box grew to fit the row: whole, with no ellipsis and no footer.
    assert!(text.contains("task and more"), "{text}");
    assert!(text.contains("×2"), "{text}");
    let row = text
        .lines()
        .find(|line| line.contains("task and more"))
        .unwrap_or_default();
    assert!(!row.contains('…'), "{row}");
    // No detail footer: a rule between the rows and a repeat of the row.
    assert_eq!(text.matches("task and more").count(), 1, "{text}");
}

fn header_agent(
    pane: &str,
    status: crate::api::schema::AgentStatus,
    awaiting_reply: bool,
    title: &str,
) -> crate::protocol::ClientShellAgent {
    crate::protocol::ClientShellAgent {
        pane_id: pane.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: Some(title.into()),
        terminal_title_stripped: Some(title.into()),
        agent_status: status,
        state_change_seq: 1,
        awaiting_reply,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    }
}

#[test]
fn the_header_counts_agents_working_and_asking_and_lists_them_on_click() {
    use crate::api::schema::AgentStatus::{Blocked, Idle, Working};
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents = vec![
        header_agent("p1", Working, false, "Fix the login test"),
        header_agent("p2", Working, false, "Write the docs"),
        // Asking wins over working; a blocked agent with a reply wish counts once.
        header_agent("pane_1", Working, true, "Which database?"),
        header_agent("p4", Blocked, true, "Delete the build dir?"),
        header_agent("p5", Idle, false, "idle one"),
    ];
    state.set_snapshot(Box::new(projected));
    assert_eq!(state.agent_indicator_counts(), (2, 2));

    for width in [106, 140] {
        let frame = state.compose(width, 30).unwrap();
        let header = frame_rows(&frame)[state.hits.notification_log_button.y as usize].clone();
        assert!(header.contains("?2"), "{width}: {header:?}");
        assert!(header.contains('2'), "{header:?}");
    }
    let asking = state.hits.asking_list_button;
    let working = state.hits.working_list_button;
    assert!(asking.width > 0 && working.width > 0 && asking.x > working.x);

    // Clicking opens the list of those agents: no request is sent.
    let outcome = left_click(&mut state, (asking.x + 1, asking.y));
    assert!(outcome.actions.is_empty());
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("Which database?"), "{text}");
    assert!(text.contains("Delete the build dir?"), "{text}");
    assert!(text.contains("reply"), "{text}");
    assert!(!text.contains("Fix the login test"), "{text}");
    assert_eq!(state.hits.notification_log_rows.len(), 2);

    // A row jumps to the agent's pane.
    let row = state.hits.notification_log_rows[0].0;
    let outcome = left_click(&mut state, (row.x + 3, row.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::PaneFocus(target)
                if target.pane_id == "pane_1"))));
    assert!(state.overlay.is_none());

    // The working button lists the other two.
    let outcome = left_click(&mut state, (working.x + 1, working.y));
    assert!(outcome.actions.is_empty());
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(
        text.contains("Fix the login test") && text.contains("Write the docs"),
        "{text}"
    );
    assert!(!text.contains("Which database?"), "{text}");
}

#[test]
fn the_header_hides_indicators_for_zero_and_keeps_clear_of_the_sort_buttons() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut empty = state.snapshot.as_deref().expect("snapshot").clone();
    empty.agents.clear();
    state.set_snapshot(Box::new(empty));
    state.compose(106, 30).unwrap();
    assert_eq!(state.hits.working_list_button.width, 0);
    assert_eq!(state.hits.asking_list_button.width, 0);

    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents = vec![
        header_agent("p1", crate::api::schema::AgentStatus::Working, false, "a"),
        header_agent("p2", crate::api::schema::AgentStatus::Blocked, false, "b"),
    ];
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    let sort_right = state
        .hits
        .space_sort_buttons
        .iter()
        .map(|(rect, _)| rect.right())
        .max()
        .unwrap_or(0);
    for rect in [
        state.hits.working_list_button,
        state.hits.asking_list_button,
    ] {
        assert!(
            rect.width > 0 && rect.x >= sort_right,
            "{rect:?} vs {sort_right}"
        );
    }
}

#[test]
fn one_wheel_event_scrolls_the_spaces_list_by_one_row() {
    let mut state = state_with_tabs(true);
    for index in 0..60 {
        with_job(&mut state, &format!("job_{index}"), TabStatus::Running);
    }
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    // Unfolding scrolled to the squares; start the wheel from the top.
    state.workspace_scroll = 0;
    state.compose(106, 30).unwrap();
    let body = state.hits.workspace_body;
    assert!(state.hits.workspace_max_scroll >= 3);
    let wheel = |state: &mut ClientShellState, kind| {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind,
            column: body.x + 2,
            row: body.y + 1,
            modifiers: KeyModifiers::empty(),
        })]);
    };
    wheel(&mut state, crossterm::event::MouseEventKind::ScrollDown);
    assert_eq!(state.workspace_scroll, 1);
    wheel(&mut state, crossterm::event::MouseEventKind::ScrollDown);
    wheel(&mut state, crossterm::event::MouseEventKind::ScrollDown);
    assert_eq!(state.workspace_scroll, 3);
    wheel(&mut state, crossterm::event::MouseEventKind::ScrollUp);
    assert_eq!(state.workspace_scroll, 2);
}

#[test]
fn a_space_name_row_starts_with_a_bar_and_the_focused_active_tab_has_one() {
    let mut state = state_with_tabs(true);
    let frame = state.compose(106, 30).unwrap();
    let palette = state.config.palette.clone();
    let name_row = state.hits.workspaces[0].rect;
    let cell = |x: u16, y: u16| &frame.cells[y as usize * frame.width as usize + x as usize];
    // The focused space's name row has the accent bar in its first column
    // and no fill.
    let bar = cell(name_row.x, name_row.y);
    assert_eq!(bar.symbol, "▍");
    assert_eq!(bar.fg, crate::protocol::color_to_u32(palette.accent));
    // No fill: the name row's background is the panel's, like the cell next
    // to the bar.
    assert_eq!(
        cell(name_row.x + 12, name_row.y).bg,
        cell(name_row.x + 1, name_row.y).bg
    );
    // The active tab's line starts with the accent bar in its fill.
    let line = state.hits.space_tabs[0].0;
    let bar = frame_rows(&frame)[line.y as usize].contains('▌');
    assert!(bar, "{:?}", frame_rows(&frame)[line.y as usize]);
}

#[test]
fn the_working_list_includes_an_agent_that_waits_on_a_running_job() {
    use crate::api::schema::AgentStatus::{Done, Idle, Working};
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    // The purple icon in the sidebar: an idle agent whose child tab runs.
    let mut job = projected.tabs[0].clone();
    job.tab_id = "job_9".into();
    job.parent_tab_id = Some("tab_1".into());
    job.status = Some(TabStatus::Running);
    projected.tabs.push(job);
    projected.agents = vec![
        header_agent("pane_1", Idle, false, "Session import"),
        header_agent("p2", Working, false, "Claude Code"),
        header_agent("p3", Done, false, "finished, no job"),
    ];
    projected.agents[1].tab_id = "tab_2".into();
    projected.agents[2].tab_id = "tab_3".into();
    state.set_snapshot(Box::new(projected));
    assert_eq!(state.agent_indicator_counts(), (2, 0));
    state.compose(106, 30).unwrap();
    let working = state.hits.working_list_button;
    left_click(&mut state, (working.x + 1, working.y));
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("Session import"), "{text}");
    assert!(text.contains("Claude Code"), "{text}");
    assert!(!text.contains("finished, no job"), "{text}");
}

#[test]
fn a_renamed_tab_shows_its_name_in_the_agent_lists_and_old_history_rows() {
    use crate::api::schema::AgentStatus::Working;
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs[0].label = "try-guix".into();
    projected.tabs[0].custom_label = true;
    projected.agents = vec![header_agent("pane_1", Working, false, "Session import")];
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    let working = state.hits.working_list_button;
    left_click(&mut state, (working.x + 1, working.y));
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("try-guix · Session import"), "{text}");

    // A history row stored with the old task gets the new name too.
    let record = crate::api::schema::NotificationRecord {
        id: 1,
        unix_ms: 1_790_633_100_000,
        kind: "finished".into(),
        title: "claude finished".into(),
        body: None,
        agent: Some("claude".into()),
        workspace_id: Some("ws_1".into()),
        tab_id: Some("tab_1".into()),
        pane_id: None,
        task: Some("Session import".into()),
        request: None,
        repeats: None,
    };
    assert!(state
        .notification_row_text(&record)
        .starts_with("✓ try-guix · Session import · claude"));
    // An unnamed tab shows the task alone, as before.
    let mut plain = state.snapshot.as_deref().expect("snapshot").clone();
    plain.tabs[0].custom_label = false;
    state.set_snapshot(Box::new(plain));
    assert!(state
        .notification_row_text(&record)
        .starts_with("✓ Session import · claude"));
}

#[test]
fn bookmarked_tabs_are_listed_in_the_order_of_the_spaces_and_removed_from_the_list() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents.clear();
    let mut second_space = projected.workspaces[0].clone();
    second_space.workspace_id = "ws_2".into();
    second_space.label = "second".into();
    second_space.number = 2;
    second_space.focused = false;
    projected.workspaces.push(second_space);
    // The second space's tab comes first in the snapshot's tab list: the
    // list still follows the spaces.
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_other".into();
    other.workspace_id = "ws_2".into();
    other.label = "other tab".into();
    other.focused = false;
    other.bookmarked = true;
    projected.tabs.insert(0, other);
    let own = projected
        .tabs
        .iter_mut()
        .find(|tab| tab.tab_id == "tab_1")
        .expect("tab_1");
    own.label = "first tab".into();
    own.bookmarked = true;
    state.set_snapshot(Box::new(projected));
    assert_eq!(state.bookmark_count(), 2);

    state.compose(106, 30).unwrap();
    let button = state.hits.bookmarks_list_button;
    assert!(button.width > 0);
    let outcome = left_click(&mut state, (button.x + 1, button.y));
    assert!(outcome.actions.is_empty(), "the list needs no request");
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let first = rows
        .iter()
        .position(|row| row.contains("first tab"))
        .expect("first tab listed");
    let second = rows
        .iter()
        .position(|row| row.contains("other tab"))
        .expect("other tab listed");
    assert!(first < second, "spaces order: {first} {second}");
    // The tab's state icon replaces the star in front of the name.
    assert!(!rows[first].contains('★'), "{:?}", rows[first]);
    assert!(rows[first].contains('◐'), "{:?}", rows[first]);

    // A row jumps to its tab.
    let row = state.hits.notification_log_rows[1].0;
    let outcome = left_click(&mut state, (row.x + 3, row.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabFocus(target)
                if target.tab_id == "tab_other"))));

    // A middle click removes the bookmark through the server.
    left_click(&mut state, (button.x + 1, button.y));
    state.compose(106, 30).unwrap();
    let row = state.hits.notification_log_rows[0].0;
    let outcome =
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle),
            column: row.x + 3,
            row: row.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabBookmark(params)
                if params.tab_id == "tab_1" && !params.bookmarked))));
}

#[test]
fn a_header_list_highlights_no_row_until_one_is_picked_and_keeps_it_on_its_tab() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents.clear();
    let mut second_space = projected.workspaces[0].clone();
    second_space.workspace_id = "ws_2".into();
    second_space.label = "second".into();
    second_space.number = 2;
    second_space.focused = false;
    projected.workspaces.push(second_space);
    let mut other = projected.tabs[0].clone();
    other.tab_id = "tab_other".into();
    other.workspace_id = "ws_2".into();
    other.label = "other tab".into();
    other.focused = false;
    other.bookmarked = true;
    projected.tabs.push(other);
    projected.tabs[0].label = "first tab".into();
    projected.tabs[0].bookmarked = true;
    state.set_snapshot(Box::new(projected));
    let key = |state: &mut ClientShellState, code: KeyCode| {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(code, KeyModifiers::empty()),
        )])
    };
    // The text of the highlighted row, if one has the accent bar.
    let lit = |state: &mut ClientShellState| {
        let frame = state.compose(106, 30).unwrap();
        let lines = frame_rows(&frame);
        state
            .hits
            .notification_log_rows
            .iter()
            .map(|(rect, _)| {
                lines[rect.y as usize]
                    .chars()
                    .skip(rect.x as usize)
                    .take(rect.width as usize)
                    .collect::<String>()
            })
            .find(|line| line.contains('▌'))
    };

    state.compose(106, 30).unwrap();
    let button = state.hits.bookmarks_list_button;
    left_click(&mut state, (button.x + 1, button.y));
    assert_eq!(lit(&mut state), None);
    // With nothing picked, `x` removes nothing and Enter jumps nowhere.
    assert!(key(&mut state, KeyCode::Char('x')).actions.is_empty());
    assert!(key(&mut state, KeyCode::Enter).actions.is_empty());
    assert!(state.overlay.is_some());

    // Up from nothing picks the last row.
    key(&mut state, KeyCode::Up);
    let line = lit(&mut state).expect("a row is lit");
    assert!(line.contains("other tab"), "{line:?}");

    // The rows reorder under it: the highlight stays on its tab.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.workspaces.reverse();
    state.set_snapshot(Box::new(projected));
    let line = lit(&mut state).expect("a row is lit");
    assert!(line.contains("other tab"), "{line:?}");
    let frame = state.compose(106, 30).unwrap();
    let first = frame_rows(&frame)[state.hits.notification_log_rows[0].0.y as usize].clone();
    assert!(first.contains("other tab"), "now first: {first:?}");

    // `x` removes that bookmark once: the row waits for the snapshot, and a
    // second `x` must not remove it again.
    let outcome = key(&mut state, KeyCode::Char('x'));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabBookmark(params)
                if params.tab_id == "tab_other" && !params.bookmarked))));
    assert_eq!(lit(&mut state), None);
    assert!(key(&mut state, KeyCode::Char('x')).actions.is_empty());

    // Down from nothing picks the first row.
    key(&mut state, KeyCode::Down);
    let line = lit(&mut state).expect("a row is lit");
    assert!(line.contains("other tab"), "{line:?}");
    key(&mut state, KeyCode::Down);
    let line = lit(&mut state).expect("a row is lit");
    assert!(line.contains("first tab"), "{line:?}");
}

#[test]
fn the_tab_context_menu_adds_and_removes_a_bookmark() {
    let mut state = state_with_tabs(true);
    state.compose(106, 30).unwrap();
    let line = state.hits.space_tabs[0].0;
    // Right-click the tab line.
    let menu_labels = |state: &ClientShellState| match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
        column: line.x + 6,
        row: line.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let labels = menu_labels(&state);
    assert!(
        labels.iter().any(|label| label == "Add to bookmarks"),
        "{labels:?}"
    );

    // Choosing it asks the server and does not move the focus.
    let index = labels
        .iter()
        .position(|label| label == "Add to bookmarks")
        .unwrap();
    if let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_mut() {
        menu.highlighted = index;
    }
    let outcome = state.handle_input_bytes(b"\r");
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabBookmark(params)
                if params.bookmarked))));
    assert!(!outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabFocus(_)))));
}

#[test]
fn at_32_columns_every_indicator_fits_beside_the_one_sort_button() {
    use crate::api::schema::AgentStatus::{Blocked, Working};
    let mut state = state_with_tabs(true);
    state.sidebar_width = 32;
    state.notification_log_received(Some("tab_9"));
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents = vec![
        header_agent("p1", Working, false, "a"),
        header_agent("p2", Blocked, false, "b"),
    ];
    projected.tabs[0].bookmarked = true;
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(106, 30).unwrap();
    let header = frame_rows(&frame)[0].clone();
    assert!(header.starts_with(" ⇅ manual"), "{header:?}");
    for (name, rect) in [
        ("working", state.hits.working_list_button),
        ("asking", state.hits.asking_list_button),
        ("bookmarks", state.hits.bookmarks_list_button),
        ("notifications", state.hits.notification_log_button),
    ] {
        assert!(rect.width > 0, "{name} hidden: {header:?}");
    }
    // The working and asking indicators sit side by side.
    assert!(
        state.hits.working_list_button.right() <= state.hits.asking_list_button.x + 1,
        "{header:?}"
    );

    // The sort button opens the choice; picking name sorts by name, and
    // picking it again flips the direction.
    let button = state.hits.space_sort_buttons[0].0;
    left_click(&mut state, (button.x + 1, button.y));
    state.compose(106, 30).unwrap();
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::ContextMenu(_))
    ));
    let name_row = state.hits.context_menu_rows[1].0;
    left_click(&mut state, (name_row.x + 2, name_row.y));
    assert_eq!(
        state.space_sort.key,
        super::super::space_sort::SpaceSortKey::Name
    );
    assert!(!state.space_sort.name_descending);
    left_click(&mut state, (button.x + 1, button.y));
    state.compose(106, 30).unwrap();
    let name_row = state.hits.context_menu_rows[1].0;
    left_click(&mut state, (name_row.x + 2, name_row.y));
    assert!(state.space_sort.name_descending);
}

#[test]
fn the_button_of_an_open_list_is_tinted_and_keeps_its_own_colour() {
    use crate::api::schema::AgentStatus::{Blocked, Working};
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents = vec![
        header_agent("p1", Working, false, "a"),
        header_agent("p2", Blocked, false, "b"),
    ];
    projected.tabs[0].bookmarked = true;
    state.set_snapshot(Box::new(projected));
    let palette = state.config.palette.clone();
    let tints = [6, 4].map(|total| {
        crate::protocol::color_to_u32(
            crate::client::shell::render::tabs::blend(palette.accent, palette.panel_bg, 1, total)
                .unwrap_or(palette.accent),
        )
    });
    let cell = |state: &mut ClientShellState, rect: Rect| {
        let frame = state.compose(106, 30).unwrap();
        frame.cells[rect.y as usize * frame.width as usize + rect.x as usize + 1].clone()
    };
    state.compose(106, 30).unwrap();
    let buttons = [
        state.hits.working_list_button,
        state.hits.asking_list_button,
        state.hits.bookmarks_list_button,
        state.hits.notification_log_button,
    ];
    for (index, button) in buttons.into_iter().enumerate() {
        let closed = cell(&mut state, button);
        assert!(!tints.contains(&closed.bg), "closed {index}");
        left_click(&mut state, (button.x + 1, button.y));
        let open = cell(&mut state, button);
        // Tinted, and the glyph keeps the colour it had when closed.
        assert!(tints.contains(&open.bg), "open {index}: {}", open.bg);
        assert_eq!(open.fg, closed.fg, "colour kept {index}");
        // Only the open one is tinted.
        for (other, rect) in buttons.into_iter().enumerate() {
            if other != index {
                assert!(
                    !tints.contains(&cell(&mut state, rect).bg),
                    "{other} while {index}"
                );
            }
        }
        // The same button closes it.
        left_click(&mut state, (button.x + 1, button.y));
        assert!(
            !tints.contains(&cell(&mut state, button).bg),
            "closed again {index}"
        );
    }
}

#[test]
fn unfolding_a_tab_line_near_the_bottom_scrolls_its_squares_into_view() {
    let mut state = state_with_tabs(true);
    for index in 0..24 {
        with_job(&mut state, &format!("job_{index}"), TabStatus::Running);
    }
    // A short list: the unfolded line's squares do not fit below it.
    state.compose(106, 14).unwrap();
    state.workspace_scroll = 0;
    state.compose(106, 14).unwrap();
    assert!(state.hits.space_tab_squares.is_empty());
    let (rect, _) = state.hits.space_tab_folds[0];
    left_click(&mut state, (rect.x, rect.y));
    state.compose(106, 14).unwrap();
    let body = state.hits.workspace_body;
    assert!(
        state
            .hits
            .space_tab_squares
            .iter()
            .any(|(rect, _)| rect.y >= body.y && rect.bottom() <= body.bottom()),
        "squares in view after the unfold: {:?} in {body:?}",
        state.hits.space_tab_squares
    );
}

#[test]
fn a_bookmark_row_shows_the_tabs_task_and_its_menu_opens_over_the_list() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    state.config.tab_label = crate::config::TabLabelConfig::Title;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.tabs[0].bookmarked = true;
    projected.tabs[0].custom_label = false;
    projected.tabs[0].label = "2".into();
    projected.agents = vec![header_agent(
        "pane_1",
        crate::api::schema::AgentStatus::Idle,
        false,
        "Job search automation",
    )];
    projected.agents[0].tab_id = "tab_1".into();
    state.set_snapshot(Box::new(projected));
    state.compose(106, 30).unwrap();
    let button = state.hits.bookmarks_list_button;
    left_click(&mut state, (button.x + 1, button.y));
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("Job search automation"), "{text}");

    let row = state.hits.notification_log_rows[0].0;
    let right_click = |state: &mut ClientShellState| {
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
            column: row.x + 3,
            row: row.y,
            modifiers: KeyModifiers::empty(),
        })])
    };
    right_click(&mut state);
    // The menu opens over the list; the list stays.
    let frame = state.compose(106, 30).unwrap();
    let text = frame_rows(&frame).join("\n");
    assert!(
        text.contains("Job search automation"),
        "the list stays: {text}"
    );
    assert!(text.contains("│ Remove from bookmarks "), "{text}");
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::NotificationLog(log)) if log.menu.is_some()
    ));
    let item = state.hits.bookmark_menu_row;
    assert_eq!(
        item.y,
        row.y + 1,
        "the popup's border is on the clicked row"
    );

    // Esc closes only the menu.
    state.handle_input_bytes(b"\x1b");
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::NotificationLog(log)) if log.menu.is_none()
    ));

    // Choosing the item removes the bookmark and leaves the list open.
    right_click(&mut state);
    state.compose(106, 30).unwrap();
    let item = state.hits.bookmark_menu_row;
    let outcome = left_click(&mut state, (item.x + 2, item.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabBookmark(params)
                if params.tab_id == "tab_1" && !params.bookmarked))));
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::NotificationLog(log)) if log.menu.is_none()
    ));

    // A click elsewhere closes only the menu as well.
    right_click(&mut state);
    state.compose(106, 30).unwrap();
    left_click(&mut state, (row.x + 3, row.y + 2));
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::NotificationLog(log)) if log.menu.is_none()
    ));
}

#[test]
fn the_filter_lives_in_the_header_row_and_its_cursor_is_a_bar_not_a_fill() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    // Closed: a magnifier beside the sort button; nothing at the bottom.
    assert!(
        rows[0].contains("⇅ manual") && rows[0].contains('⌕'),
        "{:?}",
        rows[0]
    );
    assert!(!rows.iter().any(|row| row.contains("/ filter")));
    let body_y = state.hits.workspace_body.y;

    // Open: the bar replaces the header row; the list does not move down.
    let button = state.hits.space_filter_button;
    left_click(&mut state, (button.x + 1, button.y));
    state.handle_input_bytes(b"c");
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    assert!(rows[0].starts_with(" / c"), "{:?}", rows[0]);
    assert!(!rows[0].contains('⇅'), "{:?}", rows[0]);
    assert_eq!(state.hits.workspace_body.y, body_y);

    // The space the arrows chose has a bar down its block and no fill.
    let name_row = state.hits.workspaces[0].rect;
    let cell = |x: u16, y: u16| &frame.cells[y as usize * frame.width as usize + x as usize];
    let selection = crate::protocol::color_to_u32(state.config.palette.selection_bg);
    for y in name_row.y..name_row.bottom() {
        assert_ne!(
            cell(name_row.x + 12, y).bg,
            selection,
            "row {y} is not filled"
        );
    }
}

#[test]
fn the_job_summary_on_a_space_row_is_right_aligned() {
    let mut state = state_with_tabs(false);
    state.sidebar_width = 40;
    with_job(&mut state, "job_1", TabStatus::Running);
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let row = rows
        .iter()
        .find(|row| row.contains("client-shell"))
        .expect("the space's name row");
    let chars = row.chars().collect::<Vec<_>>();
    let name_end = row.find("client-shell").unwrap_or(0);
    let glyph = chars[..40]
        .iter()
        .rposition(|c| matches!(*c, '◐' | '◓' | '◑' | '◒'))
        .expect("the running jobs count");
    // Far right of the name, close to the sidebar's edge (column 40).
    assert!(glyph > name_end + 20 && glyph >= 30, "{glyph} in {row:?}");
}

#[test]
fn a_jump_from_a_list_opens_the_collapsed_space_it_lands_in() {
    use crate::api::schema::AgentStatus::Blocked;
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents = vec![header_agent("pane_1", Blocked, false, "Which database?")];
    state.set_snapshot(Box::new(projected));
    state
        .collapsed_groups
        .insert(super::super::space_tabs::tabs_collapse_key("ws_1"));
    let frame = state.compose(106, 30).unwrap();
    assert!(state.hits.space_tabs.is_empty(), "the space is collapsed");
    assert!(frame_rows(&frame).iter().any(|row| row.contains('►')));

    let button = state.hits.asking_list_button;
    left_click(&mut state, (button.x + 1, button.y));
    state.compose(106, 30).unwrap();
    let row = state.hits.notification_log_rows[0].0;
    let outcome = left_click(&mut state, (row.x + 3, row.y));
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::PaneFocus(_)))));
    // The space opened; its tab line shows.
    assert!(state.collapsed_groups.is_empty());
    state.compose(106, 30).unwrap();
    assert!(!state.hits.space_tabs.is_empty(), "the tab line is listed");
}

#[test]
fn a_new_tab_opens_the_collapsed_space_it_is_created_in() {
    let mut state = state_with_tabs(true);
    state.config.prompt_new_tab_name = false;
    state
        .collapsed_groups
        .insert(super::super::space_tabs::tabs_collapse_key("ws_1"));
    state.compose(106, 30).unwrap();
    // The keybinding path.
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
        &mut outcome,
    );
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::TabCreate(_)))));
    assert!(state.collapsed_groups.is_empty(), "the space opened");
}

#[test]
fn history_times_and_icons_form_columns_whatever_their_format() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let now_ms = crate::usage::now_unix() * 1000;
    let record =
        |id: u64, unix_ms: u64, title: &str, tab: &str| crate::api::schema::NotificationRecord {
            id,
            unix_ms,
            kind: "finished".into(),
            title: title.into(),
            body: None,
            agent: None,
            workspace_id: Some("ws_1".into()),
            tab_id: Some(tab.into()),
            pane_id: None,
            task: None,
            request: None,
            repeats: None,
        };
    state.notification_log_received(Some("tab_9"));
    state.compose(106, 30).unwrap();
    let button = state.hits.notification_log_button;
    left_click(&mut state, (button.x + 1, button.y));
    state.complete_notification_list(
        ClientEndpointId::Local,
        Ok(crate::api::schema::ResponseResult::NotificationList {
            notifications: vec![
                // Three days old: a date and a time; today: a time only; one
                // row has a live tab (an icon), the others do not.
                record(1, now_ms - 3 * 86_400_000, "old entry", "tab_gone"),
                record(2, now_ms, "today entry", "tab_gone2"),
                record(3, now_ms, "icon entry", "tab_1"),
            ],
        }),
    );
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let column = |needle: &str| -> usize {
        let row = rows
            .iter()
            .find(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle} listed: {rows:?}"));
        row[..row.find(needle).unwrap()].chars().count()
    };
    // The words start in one column (compared by the first letter of each).
    let old = column("old entry");
    assert_eq!(column("today entry"), old, "{rows:?}");
    assert_eq!(column("icon entry"), old, "{rows:?}");
    let today = rows.iter().find(|row| row.contains("today entry")).unwrap();
    assert!(today.contains(':') && today.find("today").unwrap() > today.find(':').unwrap() + 3);
}

#[test]
fn a_tab_line_shows_the_progress_of_the_agents_todo_list() {
    let mut state = state_with_tabs(true);
    state.sidebar_width = 40;
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents[0]
        .tokens
        .push(("plan".to_owned(), "3/7".to_owned()));
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let line = state.hits.space_tabs[0].0;
    let row = &rows[line.y as usize];
    assert!(row.contains("agent tab") && row.contains("3/7"), "{row:?}");
    assert!(row.find("agent tab").unwrap() < row.find("3/7").unwrap());

    // Without the token, or with a stray value, nothing is drawn.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    projected.agents[0].tokens.clear();
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(106, 30).unwrap();
    assert!(!frame_rows(&frame)[line.y as usize].contains("3/7"));
}

#[test]
fn an_agent_waiting_on_an_idle_job_gets_a_ring_that_does_not_turn() {
    use crate::api::schema::TabActivity;
    let mut state = state_with_tabs(true);
    set_agent_status(&mut state, AgentStatus::Idle, false);
    with_job(&mut state, "job_1", TabStatus::Running);
    let icon = |state: &mut ClientShellState| {
        let frame = state.compose(106, 30).unwrap();
        let line = state.hits.space_tabs[0].0;
        frame_rows(&frame)[line.y as usize].clone()
    };
    // A running job: the half circle of a job.
    let row = icon(&mut state);
    assert!(!row.contains('◌'), "{row:?}");
    // The job reports it is idle: the agent's icon is the still ring.
    let mut projected = state.snapshot.as_deref().expect("snapshot").clone();
    for tab in &mut projected.tabs {
        if tab.tab_id == "job_1" {
            tab.activity = Some(TabActivity::Idle);
        }
    }
    state.set_snapshot(Box::new(projected));
    let row = icon(&mut state);
    assert!(row.contains('◌'), "{row:?}");
}

#[test]
fn a_folded_line_with_the_open_job_under_it_shows_that_job_on_its_own_row() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Failed);
    focus_tab(&mut state, "job_1");
    // The jump unfolded the squares; the user folds them by hand.
    state
        .unfolded_squares
        .entry(ClientEndpointId::Local)
        .or_default()
        .remove("tab_1");
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let line = state.hits.space_tabs[0].0;
    assert!(rows[line.y as usize].contains("agent tab"), "{rows:?}");
    let job_row = &rows[line.y as usize + 1];
    assert!(job_row.contains("job job_1"), "{job_row:?}");
    // The row opens the job like its square does.
    assert!(
        state
            .hits
            .space_tab_squares
            .iter()
            .any(|(rect, tab)| rect.y == line.y + 1 && tab == "job_1"),
        "{:?}",
        state.hits.space_tab_squares
    );

    // Focus back on the parent: the job's row stays (see the test below for
    // how long); the parent is the selection again.
    focus_tab(&mut state, "tab_1");
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame)[line.y as usize + 1].contains("job job_1"));
}

#[test]
fn the_last_open_job_stays_under_the_folded_line_after_the_focus_returns_to_the_parent() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Failed);
    focus_tab(&mut state, "job_1");
    state
        .unfolded_squares
        .entry(ClientEndpointId::Local)
        .or_default()
        .remove("tab_1");
    focus_tab(&mut state, "tab_1");
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let line = state.hits.space_tabs[0].0;
    // The parent is selected again; the job's row is still there.
    assert!(rows[line.y as usize + 1].contains("job job_1"), "{rows:?}");
    assert!(state
        .hits
        .space_tab_squares
        .iter()
        .any(|(rect, tab)| rect.y == line.y + 1 && tab == "job_1"));

    // Unfolding by hand shows the squares in its place.
    state
        .unfolded_squares
        .entry(ClientEndpointId::Local)
        .or_default()
        .insert("tab_1".into());
    let frame = state.compose(106, 30).unwrap();
    assert!(!frame_rows(&frame)[line.y as usize + 1].contains("job job_1"));

    // A restarted server keeps its tab ids (a live handoff, a restore): the
    // pin survives.
    state
        .unfolded_squares
        .entry(ClientEndpointId::Local)
        .or_default()
        .remove("tab_1");
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.boot_id = "another boot".into();
    state.set_snapshot(Box::new(projected));
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame)[line.y as usize + 1].contains("job job_1"));
}

#[test]
fn unfolding_by_hand_unpins_the_kept_job_and_shows_all_jobs() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Failed);
    with_job(&mut state, "job_2", TabStatus::Succeeded);
    let job_row = |state: &mut ClientShellState| {
        let frame = state.compose(106, 30).unwrap();
        let line = state.hits.space_tabs[0].0;
        frame_rows(&frame)[line.y as usize + 1].contains("job job_1")
    };
    let squares = |state: &ClientShellState| state.hits.space_tab_squares.len();
    // Unfold, open job_1, fold: job_1 is pinned under the line.
    click_fold(&mut state);
    focus_tab(&mut state, "job_1");
    click_fold(&mut state);
    assert!(job_row(&mut state));
    // Back on the parent, the pin stays.
    focus_tab(&mut state, "tab_1");
    assert!(job_row(&mut state));
    // The summary unpins it and shows every job.
    click_fold(&mut state);
    state.compose(106, 30).unwrap();
    assert_eq!(squares(&state), 2);
    // Folding with the focus on the parent pins nothing.
    click_fold(&mut state);
    assert!(!job_row(&mut state));
    assert_eq!(squares(&state), 0);
}

#[test]
fn a_kept_job_that_closes_is_forgotten() {
    let mut state = state_with_tabs(true);
    with_job(&mut state, "job_1", TabStatus::Failed);
    focus_tab(&mut state, "job_1");
    focus_tab(&mut state, "tab_1");
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.tabs.retain(|tab| tab.tab_id != "job_1");
    state.set_snapshot(Box::new(projected));
    assert!(state
        .kept_jobs
        .get(&ClientEndpointId::Local)
        .is_none_or(|kept| kept.is_empty()));
    // A new job that reuses the id is not shown as kept.
    with_job(&mut state, "job_1", TabStatus::Running);
    let frame = state.compose(106, 30).unwrap();
    let line = state.hits.space_tabs[0].0;
    assert!(!frame_rows(&frame)[line.y as usize + 1].contains("job job_1"));
}

#[test]
fn a_new_client_restores_the_unfolded_lines_and_the_pinned_jobs() {
    let path =
        std::env::temp_dir().join(format!("herdr-shell-job-folds-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let client = || {
        let mut config = config_with_sidebar_width(26).with_preferences_path(path.clone());
        config.spaces.tabs = true;
        let mut state = ClientShellState::new(config);
        // The first snapshot has every tab, the job too.
        let mut projected = snapshot();
        projected.tabs[0].label = "agent tab".into();
        let mut job = projected.tabs[0].clone();
        job.tab_id = "job_1".into();
        job.label = "job job_1".into();
        job.focused = false;
        job.parent_tab_id = Some("tab_1".into());
        job.status = Some(TabStatus::Failed);
        projected.tabs.push(job);
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        state
    };
    let mut state = client();
    // Unfold, open the job, fold: the job is pinned and saved.
    click_fold(&mut state);
    focus_tab(&mut state, "job_1");
    click_fold(&mut state);
    focus_tab(&mut state, "tab_1");
    drop(state);

    let mut state = client();
    let frame = state.compose(106, 30).unwrap();
    let line = state.hits.space_tabs[0].0;
    assert!(
        frame_rows(&frame)[line.y as usize + 1].contains("job job_1"),
        "the pin comes back"
    );
    // Unfolded lines come back unfolded.
    click_fold(&mut state);
    drop(state);
    let mut state = client();
    state.compose(106, 30).unwrap();
    assert_eq!(state.hits.space_tab_squares.len(), 1);
    assert!(state
        .unfolded_squares
        .get(&ClientEndpointId::Local)
        .is_some_and(|tabs| tabs.contains("tab_1")));
    let _ = std::fs::remove_file(&path);
}
