//! A headless worker's question dialog, driven through the client state.

use super::*;
use crate::api::schema::{
    Method, ResponseResult, WorkerChoiceQuestion, WorkerDecision, WorkerQuestion,
    WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionState, WorkerState,
};
use crate::client::shell::worker_question::{
    WorkerQuestionButton, WorkerQuestionLine, WorkerQuestionStatus,
};

const COMMAND: &str = "git add -A\ngit commit -m 'fix'\ngit push origin master";

fn left_click(state: &mut ClientShellState, (column, row): (u16, u16)) -> ClientShellInput {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn question(
    worker_id: &str,
    request_id: &str,
    since_ms: u64,
) -> crate::protocol::ClientShellWorkerQuestion {
    crate::protocol::ClientShellWorkerQuestion {
        worker_id: worker_id.into(),
        request_id: request_id.into(),
        cwd: "/tmp/repo".into(),
        tool_name: "Bash".into(),
        text: "git add -A git commit -m 'fix' git push origin master".into(),
        choice: false,
        since_ms,
        quiet: false,
        owner_seen_ms: None,
    }
}

fn worker(worker_id: &str, state: &str) -> crate::protocol::ClientShellWorker {
    crate::protocol::ClientShellWorker {
        worker_id: worker_id.into(),
        workspace_id: Some("ws_1".into()),
        name: "fix login".into(),
        cwd: "/tmp/repo".into(),
        state: state.into(),
        session_id: Some("session-1".into()),
        takeover: false,
    }
}

/// A client whose snapshot has worker w1 waiting on `requests`, oldest
/// first.
fn state_with_questions(requests: &[&str]) -> ClientShellState {
    let mut state = ClientShellState::new(config_with_sidebar_width(26));
    let mut projected = snapshot();
    projected.workers = vec![worker("w1", "waiting_approval")];
    projected.worker_questions = requests
        .iter()
        .enumerate()
        .map(|(index, request)| question("w1", request, 1_000 + index as u64))
        .collect();
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state
}

fn detail(request_id: &str, owner_pane_id: Option<&str>) -> WorkerQuestionDetail {
    WorkerQuestionDetail {
        worker_id: "w1".into(),
        name: "fix login".into(),
        cwd: "/tmp/repo".into(),
        state: WorkerState::WaitingApproval,
        question: WorkerQuestion {
            request_id: request_id.into(),
            kind: WorkerQuestionKind::Approval,
            tool_name: "Bash".into(),
            text: "git add -A git commit".into(),
            reason: None,
            questions: Vec::new(),
            since_ms: 1_000,
            state: WorkerQuestionState::Pending,
            escalated: owner_pane_id.map(|_| "the coordinator's pane closed".to_owned()),
        },
        input_text: COMMAND.into(),
        owner_pane_id: owner_pane_id.map(str::to_owned),
        owner_coordinator_id: owner_pane_id.map(|_| "c-7".to_owned()),
        quiet: false,
    }
}

fn requests(actions: &[ClientShellAction]) -> Vec<(String, Method)> {
    actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Opens the dialog of `request_id` and answers its fetch with `detail`.
fn open_with_detail(state: &mut ClientShellState, request_id: &str, detail: WorkerQuestionDetail) {
    let mut outcome = ClientShellInput::default();
    state.open_worker_question("w1".into(), request_id.into(), &mut outcome);
    let sent = requests(&outcome.actions);
    let [(id, Method::WorkerQuestion(target))] = sent.as_slice() else {
        panic!("one worker.question fetch: {sent:?}");
    };
    assert_eq!(target.request_id, request_id);
    state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(ResponseResult::WorkerQuestionDetail { detail }),
    );
}

fn button_enabled(state: &ClientShellState, button: WorkerQuestionButton) -> Option<bool> {
    state
        .worker_question_view()
        .expect("the dialog")
        .buttons
        .iter()
        .find(|(candidate, _, _)| *candidate == button)
        .map(|(_, _, enabled)| *enabled)
}

fn press(state: &mut ClientShellState, button: WorkerQuestionButton) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.press_worker_question_button(button, &mut outcome);
    outcome
}

fn key(state: &mut ClientShellState, code: KeyCode, modifiers: KeyModifiers) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.worker_question_key(
        &crate::input::TerminalKey::new(code, modifiers),
        &mut outcome,
    );
    outcome
}

fn open_request(state: &ClientShellState) -> Option<&str> {
    state
        .worker_question_dialog()
        .map(|dialog| dialog.request_id.as_str())
}

#[test]
fn a_click_on_a_worker_question_in_the_asking_list_opens_its_dialog() {
    let mut state = state_with_questions(&["r1"]);
    let mut outcome = ClientShellInput::default();
    state.toggle_notification_view(
        super::super::notification_log::NotificationLogView::Asking,
        &mut outcome,
    );
    assert!(outcome.actions.is_empty(), "the list asks for nothing");
    state.compose(106, 30).unwrap();
    let (row, _) = state.hits.notification_log_rows[0];
    let outcome = left_click(&mut state, (row.x + 2, row.y));
    assert_eq!(open_request(&state), Some("r1"));
    let sent = requests(&outcome.actions);
    assert!(
        matches!(sent.as_slice(), [(_, Method::WorkerQuestion(target))]
            if target.worker_id == "w1" && target.request_id == "r1"),
        "one fetch on the click: {sent:?}"
    );
    // Until the whole input is here nothing can be allowed.
    assert_eq!(
        state.worker_question_status(),
        Some(WorkerQuestionStatus::Loading)
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Allow),
        Some(false)
    );
    // Drawing it again asks for nothing more.
    let pending = state.pending_requests.len();
    state.compose(106, 30).unwrap();
    state.compose(106, 30).unwrap();
    assert_eq!(state.pending_requests.len(), pending);
}

fn mouse_moved(state: &mut ClientShellState, (column, row): (u16, u16)) {
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })]);
}

/// Opens the `?` list and draws it; the drawn rows, top first.
fn open_asking_list(state: &mut ClientShellState) -> Vec<Rect> {
    let mut outcome = ClientShellInput::default();
    state.toggle_notification_view(
        super::super::notification_log::NotificationLogView::Asking,
        &mut outcome,
    );
    state.compose(106, 30).unwrap();
    state
        .hits
        .notification_log_rows
        .iter()
        .map(|(rect, _)| *rect)
        .collect()
}

fn set_questions(
    state: &mut ClientShellState,
    questions: Vec<crate::protocol::ClientShellWorkerQuestion>,
) {
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.worker_questions = questions;
    state.set_snapshot(Box::new(projected));
}

#[test]
fn a_click_opens_the_question_its_row_showed_when_the_list_changed_since() {
    // r1 is drawn first, r2 second.
    let mut state = state_with_questions(&["r1", "r2"]);
    let rows = open_asking_list(&mut state);
    assert_eq!(rows.len(), 2);
    // Before the next frame an older question arrives: r0 now takes the
    // first place, r1 the second.
    set_questions(
        &mut state,
        vec![
            question("w1", "r0", 500),
            question("w1", "r1", 1_000),
            question("w1", "r2", 1_001),
        ],
    );
    let outcome = left_click(&mut state, (rows[1].x + 2, rows[1].y));
    assert_eq!(open_request(&state), Some("r2"), "the row that showed r2");
    assert!(matches!(requests(&outcome.actions).as_slice(),
        [(_, Method::WorkerQuestion(target))] if target.request_id == "r2"));
    assert_eq!(
        state.worker_question_status(),
        Some(WorkerQuestionStatus::Loading)
    );
}

#[test]
fn a_click_on_a_question_answered_since_the_frame_says_it_is_gone() {
    let mut state = state_with_questions(&["r1", "r2"]);
    let rows = open_asking_list(&mut state);
    // r1 is answered elsewhere: r2 moves up into r1's place.
    set_questions(&mut state, vec![question("w1", "r2", 1_001)]);
    left_click(&mut state, (rows[0].x + 2, rows[0].y));
    assert_eq!(open_request(&state), Some("r1"), "not r2, now at its place");
    assert!(
        matches!(state.worker_question_status(), Some(WorkerQuestionStatus::Gone(ref why)) if why.contains("answered elsewhere")),
        "{:?}",
        state.worker_question_status()
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Allow),
        Some(false)
    );
}

#[test]
fn enter_opens_the_highlighted_question_after_the_list_changed() {
    let mut state = state_with_questions(&["r1", "r2"]);
    let rows = open_asking_list(&mut state);
    mouse_moved(&mut state, (rows[1].x + 2, rows[1].y));
    set_questions(
        &mut state,
        vec![
            question("w1", "r0", 500),
            question("w1", "r1", 1_000),
            question("w1", "r2", 1_001),
        ],
    );
    // The highlight stays on r2 in the next frame.
    state.compose(106, 30).unwrap();
    let keys = state.notification_log_row_keys();
    assert_eq!(state.notification_log_highlighted(&keys), Some(2));
    state.handle_raw_events(vec![crate::raw_input::RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()),
    )]);
    assert_eq!(open_request(&state), Some("r2"));
}

#[test]
fn the_dialog_shows_the_whole_command_line_by_line() {
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    let view = state.worker_question_view().expect("the dialog");
    assert_eq!(view.status, WorkerQuestionStatus::Open);
    assert_eq!(view.title, "worker w1 · fix login");
    assert_eq!(view.context, "repo · Bash");
    assert_eq!(view.owner, "no owner: it asks you directly");
    assert_eq!(
        view.lines,
        COMMAND
            .lines()
            .map(|line| WorkerQuestionLine::Text(line.to_owned()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Allow),
        Some(true)
    );
    // No other question waits: no "1 of N" and no next.
    assert_eq!(view.position, None);
    assert_eq!(button_enabled(&state, WorkerQuestionButton::Next), None);

    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    for line in COMMAND.lines() {
        assert!(
            rows.iter().any(|row| row.contains(line)),
            "{line:?} drawn: {rows:#?}"
        );
    }
}

#[test]
fn enter_answers_nothing_until_tab_chooses_a_button() {
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    let outcome = key(&mut state, KeyCode::Enter, KeyModifiers::empty());
    assert!(requests(&outcome.actions).is_empty(), "no default action");
    assert_eq!(open_request(&state), Some("r1"));

    key(&mut state, KeyCode::Tab, KeyModifiers::empty());
    assert_eq!(
        state
            .worker_question_dialog()
            .and_then(|dialog| dialog.focused),
        Some(WorkerQuestionButton::Allow)
    );
    let outcome = key(&mut state, KeyCode::Enter, KeyModifiers::empty());
    let sent = requests(&outcome.actions);
    assert!(
        matches!(sent.as_slice(), [(_, Method::WorkerAnswer(params))]
            if params.worker_id == "w1"
                && params.request_id.as_deref() == Some("r1")
                && params.decision == Some(WorkerDecision::Allow)),
        "{sent:?}"
    );
    // While it is on its way, nothing else can be pressed.
    assert_eq!(
        state.worker_question_status(),
        Some(WorkerQuestionStatus::Sending(
            super::super::worker_question::WorkerQuestionAction::Allow
        ))
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Allow),
        Some(false)
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Deny),
        Some(false)
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Stop),
        Some(false)
    );
    let again = press(&mut state, WorkerQuestionButton::Allow);
    assert!(requests(&again.actions).is_empty(), "sent once");
}

#[test]
fn an_answer_opens_the_next_question_and_the_last_one_closes_the_dialog() {
    let mut state = state_with_questions(&["r1", "r2"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    assert_eq!(state.worker_question_view().unwrap().position, Some((1, 2)));
    let outcome = press(&mut state, WorkerQuestionButton::Allow);
    let sent = requests(&outcome.actions);
    let [(id, Method::WorkerAnswer(_))] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    let worker_info = || {
        serde_json::from_value::<ResponseResult>(serde_json::json!({
            "type": "worker_info",
            "worker": {
                "worker_id": "w1", "state": "waiting_approval", "cwd": "/tmp/repo",
                "name": "fix login", "turns": 0, "journal_path": "/tmp/j"
            }
        }))
        .expect("worker_info")
    };
    // The reply comes before the snapshot drops r1: the next one opens.
    let (_, actions) = state.handle_endpoint_result("boot-1", id, Ok(worker_info()));
    assert_eq!(open_request(&state), Some("r2"));
    let sent = requests(&actions);
    let [(id, Method::WorkerQuestion(target))] = sent.as_slice() else {
        panic!("the next one is fetched: {sent:?}");
    };
    assert_eq!(target.request_id, "r2");
    state.handle_endpoint_result(
        "boot-1",
        id,
        Ok(ResponseResult::WorkerQuestionDetail {
            detail: detail("r2", None),
        }),
    );
    let outcome = press(&mut state, WorkerQuestionButton::Allow);
    let sent = requests(&outcome.actions);
    let [(id, Method::WorkerAnswer(params))] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(params.request_id.as_deref(), Some("r2"));
    let (_, actions) = state.handle_endpoint_result("boot-1", id, Ok(worker_info()));
    assert!(requests(&actions).is_empty());
    assert!(state.overlay.is_none(), "nothing left to answer");
}

#[test]
fn several_questions_say_their_place_and_next_walks_them() {
    let mut state = state_with_questions(&["r1", "r2", "r3"]);
    open_with_detail(&mut state, "r2", detail("r2", None));
    assert_eq!(state.worker_question_view().unwrap().position, Some((2, 3)));
    let outcome = key(&mut state, KeyCode::Char('n'), KeyModifiers::empty());
    assert_eq!(open_request(&state), Some("r3"));
    assert!(matches!(requests(&outcome.actions).as_slice(),
        [(_, Method::WorkerQuestion(target))] if target.request_id == "r3"));
    press(&mut state, WorkerQuestionButton::Next);
    assert_eq!(open_request(&state), Some("r1"), "it wraps around");
    press(&mut state, WorkerQuestionButton::Previous);
    assert_eq!(open_request(&state), Some("r3"));
}

#[test]
fn a_question_answered_elsewhere_or_a_worker_that_exits_disables_the_buttons() {
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.worker_questions.clear();
    state.set_snapshot(Box::new(projected.clone()));
    let view = state.worker_question_view().expect("the dialog stays open");
    assert!(matches!(view.status, WorkerQuestionStatus::Gone(_)));
    assert!(
        view.notice
            .as_ref()
            .is_some_and(|(text, warns)| *warns && text.contains("answered elsewhere")),
        "{:?}",
        view.notice
    );
    for button in [
        WorkerQuestionButton::Allow,
        WorkerQuestionButton::Deny,
        WorkerQuestionButton::Stop,
    ] {
        assert_eq!(button_enabled(&state, button), Some(false), "{button:?}");
    }
    assert!(requests(&press(&mut state, WorkerQuestionButton::Allow).actions).is_empty());
    // The log stays readable.
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::ViewLog),
        Some(true)
    );

    projected.workers = vec![worker("w1", "exited")];
    state.set_snapshot(Box::new(projected));
    let view = state.worker_question_view().expect("the dialog stays open");
    assert_eq!(view.status, WorkerQuestionStatus::WorkerEnded);
    assert_eq!(view.notice, Some(("the worker exited".to_owned(), true)));
}

#[test]
fn a_refused_fetch_says_why_the_question_is_gone() {
    let mut state = state_with_questions(&["r1"]);
    let mut outcome = ClientShellInput::default();
    state.open_worker_question("w1".into(), "r1".into(), &mut outcome);
    let sent = requests(&outcome.actions);
    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Err(ClientShellEndpointError {
            code: Some("worker_question_gone".into()),
            message: "question r1 of worker w1 is no longer pending: answered".into(),
        }),
    );
    assert!(matches!(
        state.worker_question_status(),
        Some(WorkerQuestionStatus::Gone(why)) if why.contains("answered")
    ));
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Allow),
        Some(false)
    );
}

#[test]
fn deny_takes_an_optional_message() {
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    let outcome = press(&mut state, WorkerQuestionButton::Deny);
    assert!(
        requests(&outcome.actions).is_empty(),
        "deny asks for its message first"
    );
    assert!(state.insert_overlay_text("use a branch"));
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame)
        .iter()
        .any(|row| row.contains("message: use a branch")));
    let outcome = key(&mut state, KeyCode::Enter, KeyModifiers::empty());
    let sent = requests(&outcome.actions);
    assert!(
        matches!(sent.as_slice(), [(_, Method::WorkerAnswer(params))]
            if params.decision == Some(WorkerDecision::Deny)
                && params.message.as_deref() == Some("use a branch")),
        "{sent:?}"
    );

    // Without a message none is sent; Esc leaves the field without denying.
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    press(&mut state, WorkerQuestionButton::Deny);
    let outcome = key(&mut state, KeyCode::Esc, KeyModifiers::empty());
    assert!(requests(&outcome.actions).is_empty());
    assert_eq!(open_request(&state), Some("r1"));
    press(&mut state, WorkerQuestionButton::Deny);
    let outcome = press(&mut state, WorkerQuestionButton::SendDeny);
    assert!(matches!(requests(&outcome.actions).as_slice(),
        [(_, Method::WorkerAnswer(params))]
            if params.decision == Some(WorkerDecision::Deny) && params.message.is_none()));
}

#[test]
fn stop_worker_is_confirmed_then_denies_and_stops() {
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", None));
    let outcome = press(&mut state, WorkerQuestionButton::Stop);
    assert!(requests(&outcome.actions).is_empty(), "asks first");
    assert!(state.worker_question_view().unwrap().confirm_stop);
    key(&mut state, KeyCode::Esc, KeyModifiers::empty());
    assert!(!state.worker_question_view().unwrap().confirm_stop);
    assert_eq!(
        open_request(&state),
        Some("r1"),
        "Esc cancels the stop only"
    );

    press(&mut state, WorkerQuestionButton::Stop);
    let outcome = press(&mut state, WorkerQuestionButton::ConfirmStop);
    let sent = requests(&outcome.actions);
    assert!(
        matches!(sent.as_slice(), [(_, Method::WorkerDenyAndStop(params))]
            if params.worker_id == "w1" && params.request_id == "r1"),
        "{sent:?}"
    );
}

#[test]
fn an_escalated_question_names_its_owner_and_opens_it_while_it_lives() {
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", Some("pane_1")));
    let view = state.worker_question_view().unwrap();
    assert!(
        view.owner.starts_with("owner: pane pane_1 in tab") && view.owner.ends_with("tenure c-7"),
        "{}",
        view.owner
    );
    assert_eq!(
        view.escalation.as_deref(),
        Some("escalated: the coordinator's pane closed")
    );
    let outcome = press(&mut state, WorkerQuestionButton::OpenOwner);
    assert!(state.overlay.is_none());
    assert!(matches!(requests(&outcome.actions).as_slice(),
        [(_, Method::PaneFocus(target))] if target.pane_id == "pane_1"));

    // A closed owner pane is named, not offered.
    let mut state = state_with_questions(&["r1"]);
    open_with_detail(&mut state, "r1", detail("r1", Some("pane_9")));
    assert!(state
        .worker_question_view()
        .unwrap()
        .owner
        .contains("pane_9 (closed)"));
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::OpenOwner),
        None
    );
}

#[test]
fn a_choice_question_shows_its_options_as_buttons() {
    let mut state = state_with_questions(&["r1"]);
    let mut choice = detail("r1", None);
    choice.question.kind = WorkerQuestionKind::Choice;
    choice.question.tool_name = "AskUserQuestion".into();
    choice.question.questions = vec![WorkerChoiceQuestion {
        question: "Which branch?".into(),
        header: None,
        options: vec!["main".into(), "next".into()],
        multi_select: false,
    }];
    open_with_detail(&mut state, "r1", choice);
    let view = state.worker_question_view().unwrap();
    assert_eq!(
        view.lines[0],
        WorkerQuestionLine::Heading("Which branch?".into())
    );
    assert_eq!(button_enabled(&state, WorkerQuestionButton::Allow), None);
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::SendAnswer),
        Some(false),
        "nothing picked yet"
    );
    state.compose(106, 30).unwrap();
    let (option, _) = *state
        .hits
        .worker_question_buttons
        .iter()
        .find(|(_, button)| *button == WorkerQuestionButton::Pick(0, 1))
        .expect("the option is a button");
    let outcome = left_click(&mut state, (option.x + 1, option.y));
    assert!(
        requests(&outcome.actions).is_empty(),
        "a pick sends nothing"
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::SendAnswer),
        Some(true)
    );
    let outcome = press(&mut state, WorkerQuestionButton::SendAnswer);
    assert!(matches!(requests(&outcome.actions).as_slice(),
        [(_, Method::WorkerAnswer(params))]
            if params.answers == vec!["2".to_owned()]
                && params.decision == Some(WorkerDecision::Allow)));
}

#[test]
fn a_worker_line_with_a_question_opens_the_dialog_instead_of_the_log() {
    let mut config = config_with_sidebar_width(26);
    config.spaces.tabs = true;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    projected.workers = vec![worker("w1", "waiting_approval")];
    projected.worker_questions = vec![question("w1", "r1", 1_000)];
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).unwrap();
    let line = state
        .hits
        .space_tabs
        .iter()
        .find(|(_, id)| super::super::space_tabs::worker_line_worker(id) == Some("w1"))
        .map(|(rect, _)| *rect)
        .expect("the worker's line");
    let outcome = left_click(&mut state, (line.x + 6, line.y));
    assert_eq!(open_request(&state), Some("r1"));
    let sent = requests(&outcome.actions);
    assert!(
        matches!(sent.as_slice(), [(_, Method::WorkerQuestion(_))]),
        "{sent:?}"
    );
}

#[test]
fn a_server_without_the_whole_input_offers_only_the_log() {
    let mut state = state_with_questions(&["r1"]);
    state.set_endpoint_methods(Some(vec!["worker.open_log".into()]));
    let mut outcome = ClientShellInput::default();
    state.open_worker_question("w1".into(), "r1".into(), &mut outcome);
    assert!(
        requests(&outcome.actions).is_empty(),
        "nothing it cannot answer"
    );
    let view = state.worker_question_view().unwrap();
    assert_eq!(view.status, WorkerQuestionStatus::Open);
    assert!(view
        .notice
        .as_ref()
        .is_some_and(|(text, _)| text.contains("cannot send the whole input")));
    // The preview is shown, but cannot be allowed on.
    assert_eq!(
        view.lines,
        vec![WorkerQuestionLine::Text(
            "git add -A git commit -m 'fix' git push origin master".into()
        )]
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Allow),
        Some(false)
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::Stop),
        Some(false)
    );
    assert_eq!(
        button_enabled(&state, WorkerQuestionButton::ViewLog),
        Some(true)
    );
    let outcome = press(&mut state, WorkerQuestionButton::ViewLog);
    assert!(state.overlay.is_none());
    assert!(matches!(requests(&outcome.actions).as_slice(),
        [(_, Method::WorkerOpenLog(target))] if target.worker_id == "w1"));
}

#[test]
fn a_long_command_scrolls_and_is_never_cut() {
    let mut state = state_with_questions(&["r1"]);
    let mut long = detail("r1", None);
    let lines: Vec<String> = (0..60).map(|index| format!("echo line {index}")).collect();
    long.input_text = lines.join("\n");
    open_with_detail(&mut state, "r1", long);
    state.compose(106, 30).unwrap();
    assert!(state.hits.worker_question_max_scroll > 0);
    key(&mut state, KeyCode::End, KeyModifiers::empty());
    let frame = state.compose(106, 30).unwrap();
    let rows = frame_rows(&frame);
    assert!(
        rows.iter().any(|row| row.contains("echo line 59")),
        "{rows:#?}"
    );
    assert!(!rows.iter().any(|row| row.contains("echo line 0 ")));
    key(&mut state, KeyCode::Home, KeyModifiers::empty());
    let frame = state.compose(106, 30).unwrap();
    assert!(frame_rows(&frame)
        .iter()
        .any(|row| row.contains("echo line 0")));
}
