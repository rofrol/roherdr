use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use regex::Regex;

use crate::api::schema::{
    ErrorBody, ErrorResponse, EventData, EventEnvelope, EventKind, EventMatch, EventsWaitParams,
    Method, Request, ResponseResult, Subscription, SubscriptionEventData,
    SubscriptionEventEnvelope, SuccessResponse,
};
use crate::api::server::{
    dispatch_to_app_with_caller_timeout, dispatch_to_app_with_timeout, should_stop_connection,
    APP_RESPONSE_TIMEOUT, CONNECTION_POLL_INTERVAL,
};
use crate::api::subscriptions::ActiveSubscription;
use crate::api::subscriptions::{match_output, output_match_read_source};
use crate::api::{ApiRequestSender, EventHub};
use crate::ipc::LocalStream;

pub(super) fn wait_for_output(
    request_id: String,
    params: crate::api::schema::PaneWaitForOutputParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    crate::logging::api_wait_started(&request_id, &params.pane_id, params.timeout_ms);
    let deadline = params
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));

    let regex = match &params.r#match {
        crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
            Ok(regex) => Some(regex),
            Err(err) => {
                return Ok(Some(
                    serde_json::to_string(&ErrorResponse {
                        id: request_id,
                        error: ErrorBody {
                            code: "invalid_regex".into(),
                            message: err.to_string(),
                        },
                    })
                    .unwrap(),
                ));
            }
        },
        crate::api::schema::OutputMatch::Substring { .. } => None,
    };

    loop {
        if should_stop_connection(stream, running)? {
            crate::logging::api_wait_completed(&request_id, &params.pane_id, "client_disconnected");
            return Ok(None);
        }

        let read_request = Request {
            id: format!("{request_id}:read"),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: params.pane_id.clone(),
                source: output_match_read_source(&params.source),
                lines: params.lines,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: params.strip_ansi,
                intent: crate::api::schema::ReadIntent::Passive,
            }),
        };
        let response =
            dispatch_to_app_with_timeout(read_request, api_tx, Some(APP_RESPONSE_TIMEOUT));
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return Ok(Some(response));
        };
        if value.get("error").is_some() {
            let mut value = value;
            value["id"] = serde_json::Value::String(request_id.clone());
            return Ok(Some(serde_json::to_string(&value).unwrap()));
        }

        let read_value = value["result"]["read"].clone();
        let Ok(read) = serde_json::from_value::<crate::api::schema::PaneReadResult>(read_value)
        else {
            return Ok(Some(
                serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "internal_error".into(),
                        message: "failed to decode pane read result".into(),
                    },
                })
                .unwrap(),
            ));
        };

        let matched_line = match_output(&read.text, &params.r#match, regex.as_ref());
        if matched_line.is_some() {
            let revision = read.revision;
            crate::logging::api_wait_completed(&request_id, &params.pane_id, "matched");
            return Ok(Some(
                serde_json::to_string(&SuccessResponse {
                    id: request_id,
                    result: ResponseResult::OutputMatched {
                        pane_id: read.pane_id.clone(),
                        revision,
                        matched_line,
                        read,
                    },
                })
                .unwrap(),
            ));
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            crate::logging::api_wait_timed_out(&request_id, &params.pane_id);
            return Ok(Some(
                serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "timeout".into(),
                        message: "timed out waiting for output match".into(),
                    },
                })
                .unwrap(),
            ));
        }

        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

pub(super) fn wait_for_agent(
    request_id: String,
    params: crate::api::schema::AgentWaitParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let last_event_sequence = event_hub.current_sequence();
    let target = crate::api::schema::AgentTarget {
        target: params.target,
        prefer_workspace_id: params.prefer_workspace_id,
    };
    let initial = match agent_get(&request_id, &target, api_tx) {
        Ok(agent) => agent,
        Err(response) => {
            return serde_json::to_string(&response)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    };
    let until = agent_wait_statuses(params.until);
    if agent_wait_matches(&initial, &until, None) {
        return agent_wait_success(request_id, initial).map(Some);
    }

    match wait_for_resolved_agent(
        request_id.clone(),
        ResolvedAgentWait {
            target,
            until,
            timeout_ms: params.timeout_ms,
            initial,
            last_event_sequence,
            after_state_change_seq: None,
            changed_from_state_change_seq: None,
            accept_transient_status: true,
        },
        stream,
        api_tx,
        event_hub,
        running,
    )? {
        Some(AgentWaitOutcome::Matched(agent)) => agent_wait_success(request_id, *agent).map(Some),
        Some(AgentWaitOutcome::Response(response)) => Ok(Some(response)),
        None => Ok(None),
    }
}

/// `agent.wait_change`: answers with the agent once its `state_change_seq` differs from the one
/// the caller read. It takes the event cursor before it first reads the agent, so a change
/// between the caller's read and this wait answers at once; afterwards it reads the agent again
/// only when an event about its pane arrives. The agent exiting, moving away or its pane closing
/// answers `agent_not_running`.
pub(super) fn wait_for_agent_change(
    request_id: String,
    params: crate::api::schema::AgentWaitChangeParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    use crate::api::schema::AgentStatus;

    let last_event_sequence = event_hub.current_sequence();
    let target = crate::api::schema::AgentTarget {
        target: params.target,
        prefer_workspace_id: params.prefer_workspace_id,
    };
    let initial = match agent_get(&request_id, &target, api_tx) {
        Ok(agent) => agent,
        Err(response) => {
            return serde_json::to_string(&response)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    };
    let wait = ResolvedAgentWait {
        target,
        until: vec![
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Unknown,
        ],
        timeout_ms: None,
        initial,
        last_event_sequence,
        after_state_change_seq: None,
        changed_from_state_change_seq: Some(params.state_change_seq),
        accept_transient_status: false,
    };
    if wait.matches(&wait.initial) {
        return agent_wait_success(request_id, wait.initial).map(Some);
    }
    match wait_for_resolved_agent(request_id.clone(), wait, stream, api_tx, event_hub, running)? {
        Some(AgentWaitOutcome::Matched(agent)) => agent_wait_success(request_id, *agent).map(Some),
        Some(AgentWaitOutcome::Response(response)) => Ok(Some(response)),
        None => Ok(None),
    }
}

pub(super) fn prompt_agent(
    request_id: String,
    mut params: crate::api::schema::AgentPromptParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let Some(wait) = params.wait.clone() else {
        return Ok(Some(dispatch_to_app_with_timeout(
            Request {
                id: request_id,
                method: Method::AgentPrompt(params),
            },
            api_tx,
            None,
        )));
    };

    let wait_started = std::time::Instant::now();
    let target = crate::api::schema::AgentTarget {
        target: params.target.clone(),
        prefer_workspace_id: params.prefer_workspace_id.clone(),
    };
    let before_prompt =
        match agent_get_for_prompt(&request_id, &target, api_tx, wait.timeout_ms, wait_started) {
            Ok(agent) => agent,
            Err(response) => {
                return serde_json::to_string(&response)
                    .map(Some)
                    .map_err(std::io::Error::other);
            }
        };
    let deadline = wait
        .timeout_ms
        .map(|timeout_ms| wait_started + std::time::Duration::from_millis(timeout_ms));
    if let Some(prompt_wait) = params.wait.as_mut() {
        prompt_wait.submission_deadline = deadline;
    }
    let last_event_sequence = event_hub.current_sequence();
    let prompt_request = Request {
        id: request_id.clone(),
        method: Method::AgentPrompt(params),
    };
    #[cfg(windows)]
    let prompt_response = dispatch_to_app_with_caller_timeout(
        prompt_request,
        api_tx,
        remaining_timeout_ms(wait.timeout_ms, wait_started).map(std::time::Duration::from_millis),
    );
    #[cfg(not(windows))]
    let prompt_response = dispatch_to_app_with_timeout(prompt_request, api_tx, None);
    let Ok(prompted) = agent_from_response(&request_id, &prompt_response) else {
        return Ok(Some(prompt_response));
    };
    let prompt_request = prompt_request_from_response(&prompt_response);
    if !agent_wait_identity_matches(
        &prompted,
        &before_prompt.terminal_id,
        before_prompt
            .name
            .as_deref()
            .filter(|name| *name == target.target),
        before_prompt.agent.as_deref(),
    ) {
        return agent_wait_not_running(request_id).map(Some);
    }

    let until = agent_wait_statuses(wait.until);
    // The prompt counts as accepted only on the agent's acknowledgement (see
    // `await_prompt_acknowledgement`), never after a fixed time: unrelated `idle`, `done` or
    // session changes do not complete the wait.
    let acknowledgement = await_prompt_acknowledgement(
        &request_id,
        PromptAcknowledgementWait {
            target: &target,
            before: &before_prompt,
            prompted,
            prompt_request,
            last_event_sequence,
            deadline,
        },
        stream,
        api_tx,
        event_hub,
        running,
    )?;
    let (initial, prompt_request) = match acknowledgement {
        None => return Ok(None),
        Some(PromptAcknowledgement::Accepted {
            agent,
            prompt_request,
        }) => (*agent, prompt_request),
        // A dialog the caller waits for is its answer, as before; any other dialog
        // before the acknowledgement means the prompt may not have arrived.
        Some(PromptAcknowledgement::Blocked {
            agent,
            prompt_request,
        }) => {
            if until.contains(&crate::api::schema::AgentStatus::Blocked) {
                return agent_prompt_success(request_id, *agent, prompt_request).map(Some);
            }
            return agent_prompt_blocked(request_id, &target, prompt_request.as_ref(), api_tx)
                .map(Some);
        }
        Some(PromptAcknowledgement::Unacknowledged { agent }) => {
            return agent_prompt_stalled(request_id, &agent).map(Some);
        }
        Some(PromptAcknowledgement::Response(response)) => return Ok(Some(response)),
    };
    // After a prompt typed into a non-working agent, only a state reached since then answers:
    // the agent read when the turn report arrived may still show the state from before.
    let after_state_change_seq = (before_prompt.agent_status
        != crate::api::schema::AgentStatus::Working)
        .then_some(before_prompt.state_change_seq);
    if agent_wait_matches(&initial, &until, after_state_change_seq) {
        return agent_prompt_success(request_id, initial, prompt_request).map(Some);
    }

    let Some(outcome) = wait_for_resolved_agent(
        request_id.clone(),
        ResolvedAgentWait {
            target,
            until,
            timeout_ms: remaining_timeout_ms(wait.timeout_ms, wait_started),
            initial,
            // Replay from before submission so terminal lifecycle events consumed by
            // the acknowledgement still terminate this settled-state wait.
            last_event_sequence,
            after_state_change_seq,
            changed_from_state_change_seq: None,
            accept_transient_status: false,
        },
        stream,
        api_tx,
        event_hub,
        running,
    )?
    else {
        return Ok(None);
    };
    let agent = match outcome {
        AgentWaitOutcome::Matched(agent) => *agent,
        AgentWaitOutcome::Response(response) => return Ok(Some(response)),
    };
    agent_prompt_success(request_id, agent, prompt_request).map(Some)
}

fn remaining_timeout_ms(total_ms: Option<u64>, started: std::time::Instant) -> Option<u64> {
    total_ms.map(|total_ms| {
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        total_ms.saturating_sub(elapsed_ms)
    })
}

fn agent_prompt_success(
    request_id: String,
    agent: crate::api::schema::AgentInfo,
    prompt_request: Option<crate::api::schema::AgentPromptRequest>,
) -> std::io::Result<String> {
    serde_json::to_string(&SuccessResponse {
        id: request_id,
        result: ResponseResult::AgentPrompted {
            agent,
            prompt_request,
        },
    })
    .map_err(std::io::Error::other)
}

fn prompt_request_from_response(response: &str) -> Option<crate::api::schema::AgentPromptRequest> {
    let value: serde_json::Value = serde_json::from_str(response).ok()?;
    serde_json::from_value(value["result"]["prompt_request"].clone()).ok()
}

/// `agent.prompt_turn`: types the prompt (refused first when the agent does not report turns),
/// then waits until the agent reports that the turn this prompt started ended. A turn already
/// running when the prompt is typed does not end the wait: only a turn reported as started with
/// this prompt does.
pub(super) fn prompt_agent_turn(
    request_id: String,
    params: crate::api::schema::AgentPromptTurnParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let started = std::time::Instant::now();
    let deadline = params
        .timeout_ms
        .map(|ms| started + std::time::Duration::from_millis(ms));
    let mut last_event_sequence = event_hub.current_sequence();
    let prompt_response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::AgentPrompt(crate::api::schema::AgentPromptParams {
                target: params.target,
                prefer_workspace_id: params.prefer_workspace_id,
                text: params.text,
                wait: None,
                follow_turn: true,
            }),
        },
        api_tx,
        None,
    );
    let Ok(agent) = agent_from_response(&request_id, &prompt_response) else {
        return Ok(Some(prompt_response));
    };
    let Some(mut prompt_request) = prompt_request_from_response(&prompt_response) else {
        return internal_error(request_id, "agent prompt returned no request").map(Some);
    };
    let pane_id = agent.pane_id.clone();

    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }
        let mut should_probe = false;
        for (sequence, event) in event_hub.events_after(last_event_sequence) {
            last_event_sequence = sequence;
            match event.data {
                EventData::PaneUpdated { pane } if pane.pane_id == pane_id => should_probe = true,
                EventData::PaneMoved {
                    previous_pane_id, ..
                } if previous_pane_id == pane_id => should_probe = true,
                EventData::PaneClosed {
                    pane_id: event_pane,
                    ..
                }
                | EventData::PaneExited {
                    pane_id: event_pane,
                    ..
                } if event_pane == pane_id => should_probe = true,
                _ => {}
            }
        }
        let timed_out = deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if should_probe || timed_out {
            let status = dispatch_to_app_with_timeout(
                Request {
                    id: format!("{request_id}:prompt_status"),
                    method: Method::AgentPromptStatus(
                        crate::api::schema::AgentPromptStatusParams {
                            request_id: prompt_request.request_id.clone(),
                        },
                    ),
                },
                api_tx,
                Some(APP_RESPONSE_TIMEOUT),
            );
            let value: serde_json::Value =
                serde_json::from_str(&status).unwrap_or(serde_json::Value::Null);
            if value.get("error").is_some() {
                // The request went with its pane or its agent.
                return agent_wait_not_running(request_id).map(Some);
            }
            if let Ok(current) = serde_json::from_value(value["result"]["prompt_request"].clone()) {
                prompt_request = current;
            }
            if prompt_request.state.is_turn_end() {
                return agent_prompt_success(request_id, agent, Some(prompt_request)).map(Some);
            }
            if timed_out {
                let state = serde_json::to_value(prompt_request.state)
                    .ok()
                    .and_then(|state| state.as_str().map(str::to_string))
                    .unwrap_or_default();
                return serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "timeout".into(),
                        message: format!(
                            "timed out waiting for the prompt's turn to finish; request {} is {state}",
                            prompt_request.request_id
                        ),
                    },
                })
                .map(Some)
                .map_err(std::io::Error::other);
            }
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

/// `agent.prompt_tracked`: types the prompt like `agent.prompt_turn` (refused first when the
/// agent does not report turns) and answers at once with the followed request.
pub(super) fn prompt_agent_tracked(
    request_id: String,
    params: crate::api::schema::AgentPromptTrackedParams,
    api_tx: &ApiRequestSender,
) -> String {
    dispatch_to_app_with_timeout(
        Request {
            id: request_id,
            method: Method::AgentPrompt(crate::api::schema::AgentPromptParams {
                target: params.target,
                prefer_workspace_id: params.prefer_workspace_id,
                text: params.text,
                wait: None,
                follow_turn: true,
            }),
        },
        api_tx,
        None,
    )
}

/// `agent.prompt_confirmed`: types the prompt like `agent.prompt`, then answers only once the
/// agent shows it accepted it (see `await_prompt_acknowledgement`).
///
/// A dialog that is on screen before typing is refused by `agent.prompt` itself; one that
/// appears afterwards without an acknowledgement answers `agent_prompt_blocked`, and both name
/// the dialog when screen detection recognizes it. Nothing is ever typed a second time. Without
/// any of these events the call waits until the agent exits or the caller disconnects.
pub(super) fn prompt_agent_confirmed(
    request_id: String,
    params: crate::api::schema::AgentPromptConfirmedParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let target = crate::api::schema::AgentTarget {
        target: params.target.clone(),
        prefer_workspace_id: params.prefer_workspace_id.clone(),
    };
    let before = match agent_get(&request_id, &target, api_tx) {
        Ok(agent) => agent,
        Err(response) => {
            return serde_json::to_string(&response)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    };
    // Taken before typing, so an acknowledgement that arrives while the prompt call is
    // answered is still seen below.
    let last_event_sequence = event_hub.current_sequence();
    let prompt_response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::AgentPrompt(crate::api::schema::AgentPromptParams {
                target: params.target,
                prefer_workspace_id: params.prefer_workspace_id,
                text: params.text,
                wait: None,
                follow_turn: false,
            }),
        },
        api_tx,
        None,
    );
    let prompted = match agent_from_response(&request_id, &prompt_response) {
        Ok(agent) => agent,
        Err(mut response) => {
            if matches!(
                response.error.code.as_str(),
                "agent_blocked" | "agent_not_ready"
            ) {
                if let Some(dialog) = blocking_dialog(&request_id, &target, api_tx) {
                    response.error.message = format!(
                        "{}; it shows {dialog}; nothing was typed",
                        response.error.message
                    );
                }
            }
            return serde_json::to_string(&response)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    };
    let Some(prompt_request) = prompt_request_from_response(&prompt_response) else {
        return internal_error(request_id, "agent prompt returned no request").map(Some);
    };
    if !agent_wait_identity_matches(
        &prompted,
        &before.terminal_id,
        before.name.as_deref().filter(|name| *name == target.target),
        before.agent.as_deref(),
    ) {
        return agent_wait_not_running(request_id).map(Some);
    }
    let acknowledgement = await_prompt_acknowledgement(
        &request_id,
        PromptAcknowledgementWait {
            target: &target,
            before: &before,
            prompted,
            prompt_request: Some(prompt_request),
            last_event_sequence,
            deadline: None,
        },
        stream,
        api_tx,
        event_hub,
        running,
    )?;
    match acknowledgement {
        None => Ok(None),
        Some(PromptAcknowledgement::Accepted {
            agent,
            prompt_request,
        }) => agent_prompt_success(request_id, *agent, prompt_request).map(Some),
        Some(PromptAcknowledgement::Blocked { prompt_request, .. }) => {
            agent_prompt_blocked(request_id, &target, prompt_request.as_ref(), api_tx).map(Some)
        }
        // Without a deadline the wait never ends unacknowledged.
        Some(PromptAcknowledgement::Unacknowledged { .. }) => internal_error(
            request_id,
            "agent prompt acknowledgement ended without a deadline",
        )
        .map(Some),
        Some(PromptAcknowledgement::Response(response)) => Ok(Some(response)),
    }
}

struct PromptAcknowledgementWait<'a> {
    target: &'a crate::api::schema::AgentTarget,
    /// The agent as read before the prompt was typed.
    before: &'a crate::api::schema::AgentInfo,
    /// The agent as `agent.prompt` answered after typing.
    prompted: crate::api::schema::AgentInfo,
    prompt_request: Option<crate::api::schema::AgentPromptRequest>,
    /// The event cursor taken before typing.
    last_event_sequence: u64,
    /// The caller's own deadline, if it gave one; herdr adds none.
    deadline: Option<std::time::Instant>,
}

enum PromptAcknowledgement {
    Accepted {
        agent: Box<crate::api::schema::AgentInfo>,
        prompt_request: Option<crate::api::schema::AgentPromptRequest>,
    },
    /// A dialog appeared after typing, before any acknowledgement.
    Blocked {
        agent: Box<crate::api::schema::AgentInfo>,
        prompt_request: Option<crate::api::schema::AgentPromptRequest>,
    },
    /// The caller's deadline passed before any acknowledgement.
    Unacknowledged {
        agent: Box<crate::api::schema::AgentInfo>,
    },
    Response(String),
}

/// Waits until the agent shows it accepted a typed prompt. The acknowledgement is an event,
/// never a duration:
/// - an agent that reports turns: the followed request leaves `accepted`, which happens when
///   the agent reports the turn this prompt started (Claude's `UserPromptSubmit` hook);
/// - any other agent: its state turns `working` after the prompt was typed;
/// - an agent already `working` when the prompt is typed: its input is open and queues the
///   prompt, so the written submission is the acknowledgement (a prompt typed into a working
///   Pi joins its running turn and is never reported as a turn of its own).
///
/// A dialog that appears after typing without an acknowledgement ends it as `Blocked`. Without
/// any of these events it waits until the agent exits, the caller disconnects, or the caller's
/// own deadline passes.
fn await_prompt_acknowledgement(
    request_id: &str,
    wait: PromptAcknowledgementWait<'_>,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<PromptAcknowledgement>> {
    use crate::api::schema::{AgentPromptRequestState, AgentStatus};

    let not_running = |request_id: &str| {
        agent_wait_not_running(request_id.to_string())
            .map(PromptAcknowledgement::Response)
            .map(Some)
    };
    let PromptAcknowledgementWait {
        target,
        before,
        prompted,
        mut prompt_request,
        mut last_event_sequence,
        deadline,
    } = wait;
    if before.agent_status == AgentStatus::Working {
        return Ok(Some(PromptAcknowledgement::Accepted {
            agent: Box::new(prompted),
            prompt_request,
        }));
    }
    let followed = prompt_request
        .as_ref()
        .is_some_and(|request| request.state == AgentPromptRequestState::Accepted);
    let baseline_seq = before.state_change_seq;
    let pane_id = prompted.pane_id.clone();
    let expected_name = prompted.name.clone().filter(|name| *name == target.target);
    let expected_agent = prompted.agent.clone();
    let mut agent = prompted;
    let mut saw_working = false;
    let mut should_probe = true;
    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }
        let mut pane_gone = false;
        match event_hub.events_after_checked(last_event_sequence) {
            Ok(events) => {
                for (sequence, event) in events {
                    last_event_sequence = sequence;
                    match event.data {
                        EventData::PaneAgentStatusChanged {
                            pane_id: event_pane,
                            agent_status,
                            ..
                        } if event_pane == pane_id => {
                            saw_working |= agent_status == AgentStatus::Working;
                            should_probe = true;
                        }
                        EventData::PaneUpdated { pane } if pane.pane_id == pane_id => {
                            should_probe = true;
                        }
                        EventData::PaneAgentDetected {
                            pane_id: event_pane,
                            released,
                            ..
                        } if event_pane == pane_id => {
                            pane_gone |= released;
                            should_probe = true;
                        }
                        EventData::PaneMoved {
                            previous_pane_id, ..
                        } if previous_pane_id == pane_id => pane_gone = true,
                        EventData::PaneClosed {
                            pane_id: event_pane,
                            ..
                        }
                        | EventData::PaneExited {
                            pane_id: event_pane,
                            ..
                        } if event_pane == pane_id => pane_gone = true,
                        _ => {}
                    }
                }
            }
            // Events were dropped before this wait read them: read the state again.
            Err(_) => {
                last_event_sequence = event_hub.current_sequence();
                should_probe = true;
            }
        }
        let past_deadline = deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if should_probe || pane_gone || past_deadline {
            should_probe = false;
            if let Some(request) = prompt_request.as_mut().filter(|_| followed) {
                let status = dispatch_to_app_with_timeout(
                    Request {
                        id: format!("{request_id}:prompt_status"),
                        method: Method::AgentPromptStatus(
                            crate::api::schema::AgentPromptStatusParams {
                                request_id: request.request_id.clone(),
                            },
                        ),
                    },
                    api_tx,
                    Some(APP_RESPONSE_TIMEOUT),
                );
                let value: serde_json::Value =
                    serde_json::from_str(&status).unwrap_or(serde_json::Value::Null);
                match serde_json::from_value::<crate::api::schema::AgentPromptRequest>(
                    value["result"]["prompt_request"].clone(),
                ) {
                    Ok(current) => *request = current,
                    // The request went with its pane or its agent.
                    Err(_) => return not_running(request_id),
                }
                match request.state {
                    AgentPromptRequestState::Accepted => {}
                    AgentPromptRequestState::Exited => return not_running(request_id),
                    _ => {
                        return Ok(Some(PromptAcknowledgement::Accepted {
                            agent: Box::new(agent),
                            prompt_request,
                        }));
                    }
                }
            }
            if pane_gone {
                return not_running(request_id);
            }
            agent = match agent_get(request_id, target, api_tx) {
                Ok(current) => current,
                Err(response) => {
                    return agent_wait_probe_error(response)
                        .map(PromptAcknowledgement::Response)
                        .map(Some);
                }
            };
            if !agent_wait_identity_matches(
                &agent,
                &before.terminal_id,
                expected_name.as_deref(),
                expected_agent.as_deref(),
            ) {
                return not_running(request_id);
            }
            let changed = agent.state_change_seq > baseline_seq;
            if !followed && (saw_working || changed && agent.agent_status == AgentStatus::Working) {
                return Ok(Some(PromptAcknowledgement::Accepted {
                    agent: Box::new(agent),
                    prompt_request,
                }));
            }
            if changed && agent.agent_status == AgentStatus::Blocked {
                return Ok(Some(PromptAcknowledgement::Blocked {
                    agent: Box::new(agent),
                    prompt_request,
                }));
            }
            if past_deadline {
                return Ok(Some(PromptAcknowledgement::Unacknowledged {
                    agent: Box::new(agent),
                }));
            }
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

/// `agent_prompt_blocked`: the prompt was typed, then a dialog appeared before the agent
/// acknowledged it. Names the dialog when herdr recognizes it.
fn agent_prompt_blocked(
    request_id: String,
    target: &crate::api::schema::AgentTarget,
    prompt_request: Option<&crate::api::schema::AgentPromptRequest>,
    api_tx: &ApiRequestSender,
) -> std::io::Result<String> {
    let dialog =
        blocking_dialog(&request_id, target, api_tx).unwrap_or_else(|| "a dialog".to_string());
    let request = prompt_request
        .map(|request| format!(" (prompt request {})", request.request_id))
        .unwrap_or_default();
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "agent_prompt_blocked".into(),
            message: format!(
                "the prompt was typed, but before agent {} accepted it, it showed {dialog}; it may still arrive after the dialog, so check the agent before sending it again{request}",
                target.target
            ),
        },
    })
    .map_err(std::io::Error::other)
}

/// The dialog the agent shows: `the "<rule>" dialog` when a blocking screen-detection rule
/// matches its screen, else the question its integration reported while blocked; `None` when
/// neither names it (a dialog herdr does not recognize).
fn blocking_dialog(
    request_id: &str,
    target: &crate::api::schema::AgentTarget,
    api_tx: &ApiRequestSender,
) -> Option<String> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: format!("{request_id}:explain"),
            method: Method::AgentExplain(target.clone()),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    let explained = serde_json::from_str::<serde_json::Value>(&response)
        .ok()
        .and_then(|value| blocking_dialog_from_explain(&value["result"]["explain"]));
    if explained.is_some() {
        return explained;
    }
    let question = agent_get(request_id, target, api_tx).ok()?.question?;
    Some(format!("the question \"{}\"", question.trim()))
}

fn blocking_dialog_from_explain(explain: &serde_json::Value) -> Option<String> {
    let rule = &explain["matched_rule"];
    (rule["state"] == "blocked")
        .then(|| rule["id"].as_str())
        .flatten()
        .map(|id| format!("the \"{id}\" dialog"))
}

/// `agent.wait_turn`: waits until the followed prompt's turn ends and answers how. It takes the
/// event cursor before it first reads the request, so a change between that read and the
/// first look at the events is never missed: a turn that ended before the wait started
/// answers at once. Afterwards it reads the request again only when an event about its pane
/// arrives (the turn report's `pane.updated`, the agent's release, the pane's exit or close).
pub(super) fn wait_agent_turn(
    request_id: String,
    params: crate::api::schema::AgentWaitTurnParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    use crate::api::schema::{AgentPromptRequestState, AgentTurnEndReason};

    let mut last_event_sequence = event_hub.current_sequence();
    let mut pane_id: Option<String> = None;
    let mut pane_gone = false;
    let mut should_probe = true;
    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }
        match event_hub.events_after_checked(last_event_sequence) {
            Ok(events) => {
                for (sequence, event) in events {
                    last_event_sequence = sequence;
                    let Some(followed) = pane_id.as_deref() else {
                        // The first read below is still to come and sees this change.
                        continue;
                    };
                    match event.data {
                        EventData::PaneUpdated { pane } if pane.pane_id == followed => {
                            should_probe = true;
                        }
                        EventData::PaneAgentDetected {
                            pane_id: event_pane,
                            ..
                        } if event_pane == followed => should_probe = true,
                        EventData::PaneMoved {
                            previous_pane_id,
                            pane,
                            ..
                        } if previous_pane_id == followed => {
                            pane_id = Some(pane.pane_id.clone());
                            should_probe = true;
                        }
                        EventData::PaneClosed {
                            pane_id: event_pane,
                            ..
                        }
                        | EventData::PaneExited {
                            pane_id: event_pane,
                            ..
                        } if event_pane == followed => {
                            pane_gone = true;
                            should_probe = true;
                        }
                        _ => {}
                    }
                }
            }
            // Events were dropped before this wait read them: read the request again.
            Err(_) => {
                last_event_sequence = event_hub.current_sequence();
                should_probe = true;
            }
        }
        if should_probe {
            should_probe = false;
            let status = dispatch_to_app_with_timeout(
                Request {
                    id: format!("{request_id}:prompt_status"),
                    method: Method::AgentPromptStatus(
                        crate::api::schema::AgentPromptStatusParams {
                            request_id: params.request_id.clone(),
                        },
                    ),
                },
                api_tx,
                Some(APP_RESPONSE_TIMEOUT),
            );
            let value: serde_json::Value =
                serde_json::from_str(&status).unwrap_or(serde_json::Value::Null);
            let error_code = value["error"]["code"].as_str();
            if error_code.is_some_and(|code| code != "prompt_request_not_found") {
                // The app did not answer (a timeout, a shutdown): pass its error on.
                let mut value = value;
                value["id"] = serde_json::Value::String(request_id);
                return serde_json::to_string(&value)
                    .map(Some)
                    .map_err(std::io::Error::other);
            }
            let request: Option<crate::api::schema::AgentPromptRequest> =
                serde_json::from_value(value["result"]["prompt_request"].clone()).ok();
            if let Some(current_pane) = value["result"]["pane_id"].as_str() {
                pane_id = Some(current_pane.to_string());
            }
            let reason = match request.as_ref().map(|request| request.state) {
                // Its terminal went with the pane.
                None if pane_gone => Some(AgentTurnEndReason::Exited),
                None => Some(AgentTurnEndReason::UnknownRequest),
                Some(AgentPromptRequestState::Finished) => Some(AgentTurnEndReason::Finished),
                Some(AgentPromptRequestState::Failed) => Some(AgentTurnEndReason::Failed),
                Some(AgentPromptRequestState::Interrupted) => Some(AgentTurnEndReason::Interrupted),
                Some(AgentPromptRequestState::Exited) => Some(AgentTurnEndReason::Exited),
                Some(AgentPromptRequestState::Unknown) => Some(AgentTurnEndReason::Unknown),
                // The pane's shell ended, and the agent with it.
                Some(_) if pane_gone => Some(AgentTurnEndReason::Exited),
                Some(_) => None,
            };
            if let Some(reason) = reason {
                return serde_json::to_string(&SuccessResponse {
                    id: request_id,
                    result: ResponseResult::AgentTurnEnded {
                        request_id: params.request_id,
                        reason,
                        pane_id,
                        error: request.and_then(|request| request.error),
                    },
                })
                .map(Some)
                .map_err(std::io::Error::other);
            }
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

fn internal_error(request_id: String, message: &str) -> std::io::Result<String> {
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: message.into(),
        },
    })
    .map_err(std::io::Error::other)
}

struct ResolvedAgentWait {
    target: crate::api::schema::AgentTarget,
    until: Vec<crate::api::schema::AgentStatus>,
    timeout_ms: Option<u64>,
    initial: crate::api::schema::AgentInfo,
    last_event_sequence: u64,
    after_state_change_seq: Option<u64>,
    /// Matches only once the agent's `state_change_seq` differs from this one
    /// (`agent.wait_change`).
    changed_from_state_change_seq: Option<u64>,
    accept_transient_status: bool,
}

impl ResolvedAgentWait {
    fn matches(&self, agent: &crate::api::schema::AgentInfo) -> bool {
        agent_wait_matches(agent, &self.until, self.after_state_change_seq)
            && self
                .changed_from_state_change_seq
                .is_none_or(|seq| agent.state_change_seq != seq)
    }
}

enum AgentWaitOutcome {
    Matched(Box<crate::api::schema::AgentInfo>),
    Response(String),
}

fn wait_for_resolved_agent(
    request_id: String,
    wait: ResolvedAgentWait,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<AgentWaitOutcome>> {
    let deadline = wait
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
    let expected_terminal_id = wait.initial.terminal_id.clone();
    let expected_name = wait
        .initial
        .name
        .as_ref()
        .filter(|name| name.as_str() == wait.target.target)
        .cloned();
    let expected_agent = wait.initial.agent.clone();
    let pane_id = wait.initial.pane_id.clone();
    let mut last_event_sequence = wait.last_event_sequence;

    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }

        let mut should_probe = false;
        let mut matched_event_status = None;
        for (sequence, event) in event_hub.events_after(last_event_sequence) {
            last_event_sequence = sequence;
            match event.data {
                EventData::PaneAgentDetected {
                    pane_id: event_pane,
                    agent,
                    released,
                    final_status,
                    ..
                } if event_pane == pane_id => {
                    if released {
                        if let Some(status) = final_status
                            .filter(|status| wait.until.contains(status))
                            .or(matched_event_status)
                        {
                            let mut matched = wait.initial.clone();
                            matched.agent_status = status;
                            return Ok(Some(AgentWaitOutcome::Matched(Box::new(matched))));
                        }
                        return agent_wait_not_running(request_id)
                            .map(AgentWaitOutcome::Response)
                            .map(Some);
                    }
                    if agent.is_some() && expected_agent.is_some() && agent != expected_agent {
                        return agent_wait_not_running(request_id)
                            .map(AgentWaitOutcome::Response)
                            .map(Some);
                    }
                    should_probe = true;
                }
                EventData::PaneAgentStatusChanged {
                    pane_id: event_pane,
                    agent_status,
                    ..
                } if event_pane == pane_id => {
                    if wait.accept_transient_status && wait.until.contains(&agent_status) {
                        matched_event_status = Some(agent_status);
                    }
                    should_probe = true;
                }
                EventData::PaneUpdated { pane } if pane.pane_id == pane_id => should_probe = true,
                EventData::PaneMoved {
                    previous_pane_id, ..
                } if previous_pane_id == pane_id => {
                    return agent_wait_not_running(request_id)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
                EventData::PaneClosed {
                    pane_id: event_pane,
                    ..
                }
                | EventData::PaneExited {
                    pane_id: event_pane,
                    ..
                } if event_pane == pane_id => {
                    return agent_wait_not_running(request_id)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
                _ => {}
            }
        }

        if should_probe {
            let current = match agent_get(&request_id, &wait.target, api_tx) {
                Ok(agent) => agent,
                Err(response) => {
                    return agent_wait_probe_error(response)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
            };
            if !agent_wait_identity_matches(
                &current,
                &expected_terminal_id,
                expected_name.as_deref(),
                expected_agent.as_deref(),
            ) {
                return agent_wait_not_running(request_id)
                    .map(AgentWaitOutcome::Response)
                    .map(Some);
            }
            if let Some(status) = matched_event_status {
                let mut matched = current;
                matched.agent_status = status;
                return Ok(Some(AgentWaitOutcome::Matched(Box::new(matched))));
            }
            if wait.matches(&current) {
                return Ok(Some(AgentWaitOutcome::Matched(Box::new(current))));
            }
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            let current = match agent_get(&request_id, &wait.target, api_tx) {
                Ok(agent) => agent,
                Err(response) => {
                    return agent_wait_probe_error(response)
                        .map(AgentWaitOutcome::Response)
                        .map(Some);
                }
            };
            if !agent_wait_identity_matches(
                &current,
                &expected_terminal_id,
                expected_name.as_deref(),
                expected_agent.as_deref(),
            ) {
                return agent_wait_not_running(request_id)
                    .map(AgentWaitOutcome::Response)
                    .map(Some);
            }
            if wait.matches(&current) {
                return Ok(Some(AgentWaitOutcome::Matched(Box::new(current))));
            }
            return agent_wait_timeout(request_id)
                .map(AgentWaitOutcome::Response)
                .map(Some);
        }
        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

fn agent_wait_statuses(
    until: Vec<crate::api::schema::AgentStatus>,
) -> Vec<crate::api::schema::AgentStatus> {
    if until.is_empty() {
        vec![
            crate::api::schema::AgentStatus::Idle,
            crate::api::schema::AgentStatus::Done,
            crate::api::schema::AgentStatus::Blocked,
        ]
    } else {
        until
    }
}

fn agent_wait_identity_matches(
    agent: &crate::api::schema::AgentInfo,
    expected_terminal_id: &str,
    expected_name: Option<&str>,
    expected_agent: Option<&str>,
) -> bool {
    agent.terminal_id == expected_terminal_id
        && expected_name.is_none_or(|name| agent.name.as_deref() == Some(name))
        && match (expected_agent, agent.agent.as_deref()) {
            (Some(expected), Some(current)) => expected == current,
            (Some(_), None) => agent.name.is_some(),
            (None, _) => true,
        }
}

fn agent_wait_matches(
    agent: &crate::api::schema::AgentInfo,
    until: &[crate::api::schema::AgentStatus],
    after_state_change_seq: Option<u64>,
) -> bool {
    until.contains(&agent.agent_status)
        && after_state_change_seq.is_none_or(|baseline| agent.state_change_seq > baseline)
}

fn agent_get(
    request_id: &str,
    target: &crate::api::schema::AgentTarget,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::AgentInfo, ErrorResponse> {
    let response = dispatch_to_app_with_timeout(
        Request {
            id: format!("{request_id}:agent"),
            method: Method::AgentGet(target.clone()),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    agent_from_response(request_id, &response)
}

fn agent_get_for_prompt(
    request_id: &str,
    target: &crate::api::schema::AgentTarget,
    api_tx: &ApiRequestSender,
    total_timeout_ms: Option<u64>,
    started: std::time::Instant,
) -> Result<crate::api::schema::AgentInfo, ErrorResponse> {
    let request = Request {
        id: format!("{request_id}:agent"),
        method: Method::AgentGet(target.clone()),
    };
    let remaining_ms = remaining_timeout_ms(total_timeout_ms, started);
    let response = match remaining_ms {
        Some(timeout_ms) if timeout_ms <= APP_RESPONSE_TIMEOUT.as_millis() as u64 => {
            dispatch_to_app_with_caller_timeout(
                request,
                api_tx,
                Some(std::time::Duration::from_millis(timeout_ms)),
            )
        }
        _ => dispatch_to_app_with_timeout(request, api_tx, Some(APP_RESPONSE_TIMEOUT)),
    };
    agent_from_response(request_id, &response)
}

fn agent_from_response(
    request_id: &str,
    response: &str,
) -> Result<crate::api::schema::AgentInfo, ErrorResponse> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|_| ErrorResponse {
        id: request_id.into(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode agent response".into(),
        },
    })?;
    if value.get("error").is_some() {
        let error = serde_json::from_value(value["error"].clone()).map_err(|_| ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode agent error".into(),
            },
        })?;
        return Err(ErrorResponse {
            id: request_id.into(),
            error,
        });
    }
    serde_json::from_value(value["result"]["agent"].clone()).map_err(|_| ErrorResponse {
        id: request_id.into(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode agent result".into(),
        },
    })
}

fn agent_wait_success(
    request_id: String,
    agent: crate::api::schema::AgentInfo,
) -> std::io::Result<String> {
    serde_json::to_string(&SuccessResponse {
        id: request_id,
        result: ResponseResult::AgentInfo { agent },
    })
    .map_err(std::io::Error::other)
}

fn agent_wait_timeout(request_id: String) -> std::io::Result<String> {
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "timeout".into(),
            message: "timed out waiting for agent status".into(),
        },
    })
    .map_err(std::io::Error::other)
}

/// `agent_prompt_stalled`: the caller's timeout passed after the prompt was typed and before
/// the agent acknowledged it (the prompt's turn report or a `working` state).
fn agent_prompt_stalled(
    request_id: String,
    current: &crate::api::schema::AgentInfo,
) -> std::io::Result<String> {
    let status = format!("{:?}", current.agent_status).to_ascii_lowercase();
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "agent_prompt_stalled".into(),
            message: format!(
                "the prompt was typed, but agent {} did not acknowledge it (its turn report or a working state) before the caller's timeout; current status is {status}; it may still arrive, so check the agent before sending it again",
                current.name.as_deref().unwrap_or(&current.pane_id)
            ),
        },
    })
    .map_err(std::io::Error::other)
}

fn agent_wait_not_running(request_id: String) -> std::io::Result<String> {
    serde_json::to_string(&ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "agent_not_running".into(),
            message: "agent is no longer running in the target pane".into(),
        },
    })
    .map_err(std::io::Error::other)
}

fn agent_wait_probe_error(response: ErrorResponse) -> std::io::Result<String> {
    if response.error.code == "agent_not_found" {
        return agent_wait_not_running(response.id);
    }
    serde_json::to_string(&response).map_err(std::io::Error::other)
}

pub(super) fn wait_for_event(
    request_id: String,
    params: EventsWaitParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let deadline = params
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));

    let subscription = match event_match_subscription(&request_id, params.match_event) {
        Ok(subscription) => subscription,
        Err(response) => return Ok(Some(serde_json::to_string(&response).unwrap())),
    };
    let mut active = match ActiveSubscription::new(
        subscription,
        &request_id,
        0,
        api_tx,
        event_hub,
        event_hub.current_sequence(),
    ) {
        Ok(active) => active,
        Err(response) => return Ok(Some(serde_json::to_string(&response).unwrap())),
    };

    loop {
        if should_stop_connection(stream, running)? {
            return Ok(None);
        }

        match active.poll_for_wait(api_tx, event_hub) {
            Ok(Some(event)) => return Ok(Some(wait_matched_response(&request_id, event))),
            Ok(None) => {}
            Err(mut response) if response.error.code == "pane_not_found" => {
                response.id = request_id;
                return serde_json::to_string(&response)
                    .map(Some)
                    .map_err(std::io::Error::other);
            }
            Err(_) => {}
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Ok(Some(
                serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "timeout".into(),
                        message: "timed out waiting for event match".into(),
                    },
                })
                .unwrap(),
            ));
        }

        std::thread::sleep(CONNECTION_POLL_INTERVAL);
    }
}

fn event_match_subscription(
    request_id: &str,
    match_event: EventMatch,
) -> Result<Subscription, ErrorResponse> {
    match match_event {
        EventMatch::PaneAgentStatusChanged {
            pane_id,
            agent_status,
        } => Ok(Subscription::PaneAgentStatusChanged {
            pane_id,
            agent_status: Some(agent_status),
        }),
        _ => Err(ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "unsupported_event_wait_match".into(),
                message: "events.wait currently supports pane agent status matches".into(),
            },
        }),
    }
}

fn wait_matched_response(request_id: &str, event: serde_json::Value) -> String {
    let Ok(event) = serde_json::from_value::<SubscriptionEventEnvelope>(event) else {
        return serde_json::to_string(&ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode matched event".into(),
            },
        })
        .unwrap();
    };

    let SubscriptionEventData::PaneAgentStatusChanged(data) = event.data else {
        return serde_json::to_string(&ErrorResponse {
            id: request_id.into(),
            error: ErrorBody {
                code: "unsupported_event_wait_match".into(),
                message: "events.wait currently supports pane agent status matches".into(),
            },
        })
        .unwrap();
    };

    serde_json::to_string(&SuccessResponse {
        id: request_id.into(),
        result: ResponseResult::WaitMatched {
            event: EventEnvelope {
                event: EventKind::PaneAgentStatusChanged,
                data: EventData::PaneAgentStatusChanged {
                    pane_id: data.pane_id,
                    workspace_id: data.workspace_id,
                    agent_status: data.agent_status,
                    agent: data.agent,
                    title: data.title,
                    display_agent: data.display_agent,
                    state_labels: data.state_labels,
                },
            },
        },
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_blocking_rule_names_a_dialog() {
        let blocking = serde_json::json!({
            "matched_rule": {"id": "trust_directory", "state": "blocked"}
        });
        assert_eq!(
            blocking_dialog_from_explain(&blocking).as_deref(),
            Some("the \"trust_directory\" dialog")
        );
        let idle = serde_json::json!({"matched_rule": {"id": "live_prompt_box", "state": "idle"}});
        assert_eq!(blocking_dialog_from_explain(&idle), None);
        // Under hook authority screen detection is skipped and matches nothing.
        let skipped = serde_json::json!({"matched_rule": null, "screen_detection_skipped": true});
        assert_eq!(blocking_dialog_from_explain(&skipped), None);
    }

    #[test]
    fn agent_wait_probe_only_translates_agent_disappearance() {
        let disappeared = agent_wait_probe_error(ErrorResponse {
            id: "wait".into(),
            error: ErrorBody {
                code: "agent_not_found".into(),
                message: "missing".into(),
            },
        })
        .unwrap();
        let disappeared: ErrorResponse = serde_json::from_str(&disappeared).unwrap();
        assert_eq!(disappeared.id, "wait");
        assert_eq!(disappeared.error.code, "agent_not_running");

        let unavailable = agent_wait_probe_error(ErrorResponse {
            id: "wait".into(),
            error: ErrorBody {
                code: "server_unavailable".into(),
                message: "timed out waiting for app response".into(),
            },
        })
        .unwrap();
        let unavailable: ErrorResponse = serde_json::from_str(&unavailable).unwrap();
        assert_eq!(unavailable.id, "wait");
        assert_eq!(unavailable.error.code, "server_unavailable");
    }
}
