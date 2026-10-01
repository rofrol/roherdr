use super::*;

fn state_with_agent(agents: bool) -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.spaces.agents = agents;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    let mut tokens = Vec::new();
    tokens.push(("jobs".to_owned(), "1⧖ 2✓".to_owned()));
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: Some("Fold agents into spaces".into()),
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 0,
        state_labels: Vec::new(),
        tokens,
        focused: true,
    });
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state
}

#[test]
fn spaces_list_their_agents_and_jobs_under_them_when_enabled() {
    let mut state = state_with_agent(true);
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let task = rows
        .iter()
        .position(|row| row.contains("Fold agents into"))
        .expect("agent line under the space");
    assert!(rows[task + 1].contains("1⧖ 2✓"), "{}", rows[task + 1]);
    let space = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == "ws_1")
        .expect("space hit");
    assert!(
        (space.rect.y..space.rect.bottom()).contains(&(task as u16 + 1)),
        "the space's block covers its agent and job lines"
    );
}

#[test]
fn spaces_keep_their_rows_when_disabled() {
    let mut state = state_with_agent(false);
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    let space = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == "ws_1")
        .expect("space hit");
    let space_rows = &rows[space.rect.y as usize..space.rect.bottom() as usize];
    assert!(
        space_rows.iter().all(|row| !row.contains("1⧖ 2✓")),
        "{space_rows:?}"
    );
}

#[test]
fn clicking_an_agent_line_focuses_its_pane() {
    let mut state = state_with_agent(true);
    state.compose(106, 30).unwrap();
    let (rect, pane_id) = state.hits.space_agents[0].clone();
    assert_eq!(pane_id, "pane_1");
    let outcome =
        state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x + 4,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::PaneFocus(target)
                if target.pane_id == "pane_1"))));
}
