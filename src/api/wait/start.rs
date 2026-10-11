//! `agent.wait_start`: the end of a launch `agent.start` typed into a pane. Herdr registered the
//! name before it typed the launch, so the name resolves from the start; the wait reads it again
//! on each event about its pane and ends on the first outcome:
//! - the agent takes prompts: its integration reported the launch's session (Claude's
//!   `SessionStart`), or screen detection saw it ready (an agent without an integration);
//! - it stopped at a dialog (the folder-trust prompt), or its integration reported a limit;
//! - it exited, the pane closed, another program took the pane, or the name went to another
//!   terminal.
//!
//! No duration decides any of these. The caller may bound the wait itself (`timeout_ms`); past
//! that bound a launch whose agent never appeared gives its name up (see
//! `TerminalState::reconcile_managed_agent_at`), and the wait answers `timeout`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::api::schema::{
    AgentInfo, AgentStatus, AgentTarget, AgentWaitStartParams, ErrorBody, ErrorResponse,
    ResponseResult, SuccessResponse,
};
use crate::api::{ApiRequestSender, EventHub};
use crate::ipc::LocalStream;

/// How a launch ended, as far as one read of the agent shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LaunchOutcome {
    /// The agent takes prompts.
    Ready(Box<AgentInfo>),
    /// It stopped at a dialog during startup.
    Blocked(Box<AgentInfo>),
    /// The launch failed or is no longer this one's: `(code, message)`.
    Failed(&'static str, String),
}

/// What one read shows about the launch: `named` is the agent the launch's name resolves to,
/// `in_pane` the agent in the launch's pane, read only when the name no longer resolves, and
/// `pane_gone` whether that pane closed or exited. `None` while the launch goes on.
pub(super) fn launch_outcome(
    params: &AgentWaitStartParams,
    named: Option<&AgentInfo>,
    in_pane: Option<&AgentInfo>,
    pane_gone: bool,
) -> Option<LaunchOutcome> {
    let name = &params.name;
    let name_lost = || {
        LaunchOutcome::Failed(
            "agent_name_not_found",
            format!("named agent {name} no longer owns the target terminal"),
        )
    };
    let kind_mismatch = |actual: &str| {
        LaunchOutcome::Failed(
            "agent_kind_mismatch",
            format!("expected {}, detected {actual}", params.kind),
        )
    };
    let Some(agent) = named else {
        if pane_gone {
            return Some(LaunchOutcome::Failed(
                "agent_start_failed",
                format!(
                    "pane {} closed before agent {name} started its session",
                    params.pane_id
                ),
            ));
        }
        let Some(agent) = in_pane else {
            // The name went with the launch: its agent exited, or another program took the
            // pane (a recorded exit frees the name, see `set_detected_state...`).
            return Some(LaunchOutcome::Failed(
                "agent_start_failed",
                format!(
                    "the {} launch of agent {name} in pane {} ended before its session started: the agent exited, or its command did not start",
                    params.kind, params.pane_id
                ),
            ));
        };
        if agent.terminal_id != params.terminal_id {
            return Some(name_lost());
        }
        if let Some(actual) = agent
            .agent
            .as_deref()
            .filter(|actual| *actual != params.kind)
        {
            return Some(kind_mismatch(actual));
        }
        if agent.name.is_none() {
            // Only the caller's bound frees the name of a launch whose agent never appeared.
            return Some(LaunchOutcome::Failed(
                "agent_name_not_found",
                format!(
                    "{kind} started in pane {pane} only after the caller's bound released the name {name}; name it with `herdr agent rename {pane} {name}`",
                    kind = params.kind,
                    pane = agent.pane_id,
                ),
            ));
        }
        return Some(name_lost());
    };
    if agent.terminal_id != params.terminal_id || agent.name.as_deref() != Some(name) {
        return Some(name_lost());
    }
    if let Some(actual) = agent
        .agent
        .as_deref()
        .filter(|actual| *actual != params.kind)
    {
        return Some(kind_mismatch(actual));
    }
    if agent.interactive_ready {
        return Some(LaunchOutcome::Ready(Box::new(agent.clone())));
    }
    if let Some(limit) = agent.limited.as_ref() {
        let message = limit
            .message
            .as_deref()
            .map(|message| format!(": {message}"))
            .unwrap_or_default();
        return Some(LaunchOutcome::Failed(
            "agent_limited",
            format!("agent {name} hit a limit during startup{message}"),
        ));
    }
    if agent.launch_pending {
        return (agent.agent_status == AgentStatus::Blocked)
            .then(|| LaunchOutcome::Blocked(Box::new(agent.clone())));
    }
    // Named, but no launch of herdr's: it ended without becoming interactive.
    Some(LaunchOutcome::Failed(
        "agent_start_failed",
        "agent process exited before becoming interactive".to_string(),
    ))
}

pub(in crate::api) fn wait_agent_start(
    request_id: String,
    params: AgentWaitStartParams,
    stream: &mut LocalStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
    running: &Arc<AtomicBool>,
) -> std::io::Result<Option<String>> {
    let started = std::time::Instant::now();
    // Only the caller's own bound (`timeout_ms`) can pass; herdr adds none.
    let past_bound = || super::remaining_timeout_ms(params.timeout_ms, started) == Some(0);
    let named_target = AgentTarget {
        target: params.name.clone(),
        prefer_workspace_id: params.prefer_workspace_id.clone(),
    };
    let bound_seen = std::cell::Cell::new(false);
    super::follow_pane_events(
        stream,
        event_hub,
        running,
        // The caller's bound passing is the only read without an event.
        || !bound_seen.get() && past_bound(),
        |pane_id, pane_gone| {
            let followed = pane_id
                .get_or_insert_with(|| params.pane_id.clone())
                .clone();
            let timed_out = past_bound();
            bound_seen.set(bound_seen.get() || timed_out);
            // `agent.get` applies the caller's bound to the launch first.
            let named = match super::agent_get(&request_id, &named_target, api_tx) {
                Ok(agent) => Some(agent),
                Err(error) if error.error.code == "agent_not_found" => None,
                Err(error) => return pass_on(&request_id, error),
            };
            if let Some(agent) = named.as_ref() {
                // A pane that moved keeps its terminal: follow its new id.
                *pane_id = Some(agent.pane_id.clone());
            }
            let in_pane = match named {
                Some(_) => None,
                None if pane_gone => None,
                None => {
                    let pane_target = AgentTarget {
                        target: followed,
                        prefer_workspace_id: None,
                    };
                    match super::agent_get(&request_id, &pane_target, api_tx) {
                        Ok(agent) => Some(agent),
                        Err(error)
                            if matches!(
                                error.error.code.as_str(),
                                "agent_not_found" | "pane_not_found"
                            ) =>
                        {
                            None
                        }
                        Err(error) => return pass_on(&request_id, error),
                    }
                }
            };
            let outcome = launch_outcome(&params, named.as_ref(), in_pane.as_ref(), pane_gone);
            match outcome {
                Some(LaunchOutcome::Ready(agent)) => success(&request_id, *agent),
                Some(LaunchOutcome::Blocked(agent)) => {
                    let dialog = super::blocking_dialog(&request_id, &named_target, api_tx)
                        .unwrap_or_else(|| "a dialog".to_string());
                    failure(
                        &request_id,
                        "agent_not_ready",
                        format!(
                            "agent {} is blocked during startup by {dialog} and is not ready for prompts; answer it in pane {} (an agent started in a folder it never trusted asks whether to trust it)",
                            params.name, agent.pane_id,
                        ),
                    )
                }
                // With a caller's bound, a name gone with no agent in the pane may be that bound
                // releasing a launch that never appeared: the server's bound started when the
                // launch was typed, a moment before this wait's. Its answer comes at this
                // wait's bound, which reads once more.
                Some(LaunchOutcome::Failed(..))
                    if params.timeout_ms.is_some()
                        && named.is_none()
                        && in_pane.is_none()
                        && !pane_gone =>
                {
                    if timed_out {
                        failure(&request_id, "timeout", timeout_message(&params, None))
                    } else {
                        Ok(None)
                    }
                }
                Some(LaunchOutcome::Failed(code, message)) => failure(&request_id, code, message),
                None if timed_out => failure(
                    &request_id,
                    "timeout",
                    timeout_message(&params, named.as_ref()),
                ),
                None => Ok(None),
            }
        },
    )
}

/// The caller's bound passed. A launch whose agent runs keeps its name, so the message says
/// where it is.
fn timeout_message(params: &AgentWaitStartParams, still_named: Option<&AgentInfo>) -> String {
    match still_named {
        Some(agent) => format!(
            "the caller's timeout passed while agent {} starts: it runs in pane {} and keeps its name; it takes prompts once it reports its session or idles (read the pane: it may show a dialog, such as the folder-trust prompt)",
            params.name, agent.pane_id,
        ),
        None => format!(
            "the caller's timeout passed and the {} launch of agent {} runs no agent in pane {} (it never appeared, or it exited); its name is free again",
            params.kind, params.name, params.pane_id
        ),
    }
}

fn success(request_id: &str, agent: AgentInfo) -> std::io::Result<Option<String>> {
    serde_json::to_string(&SuccessResponse {
        id: request_id.to_string(),
        result: ResponseResult::AgentInfo { agent },
    })
    .map(Some)
    .map_err(std::io::Error::other)
}

fn failure(request_id: &str, code: &str, message: String) -> std::io::Result<Option<String>> {
    serde_json::to_string(&ErrorResponse {
        id: request_id.to_string(),
        error: ErrorBody {
            code: code.to_string(),
            message,
        },
    })
    .map(Some)
    .map_err(std::io::Error::other)
}

/// The app did not answer (a timeout, a shutdown): its error goes to the caller.
fn pass_on(request_id: &str, mut error: ErrorResponse) -> std::io::Result<Option<String>> {
    error.id = request_id.to_string();
    serde_json::to_string(&error)
        .map(Some)
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> AgentWaitStartParams {
        AgentWaitStartParams {
            name: "worker".into(),
            terminal_id: "term_1".into(),
            pane_id: "pane_1".into(),
            kind: "claude".into(),
            prefer_workspace_id: None,
            timeout_ms: None,
        }
    }

    fn agent(fields: serde_json::Value) -> AgentInfo {
        let mut value = serde_json::json!({
            "terminal_id": "term_1",
            "name": "worker",
            "agent": "claude",
            "agent_status": "working",
            "workspace_id": "workspace_1",
            "tab_id": "tab_1",
            "pane_id": "pane_1",
            "focused": false,
            "launch_pending": true,
            "revision": 0,
        });
        for (key, field) in fields.as_object().unwrap() {
            value[key] = field.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    fn failed_code(outcome: Option<LaunchOutcome>) -> &'static str {
        match outcome {
            Some(LaunchOutcome::Failed(code, _)) => code,
            other => panic!("expected a failure: {other:?}"),
        }
    }

    #[test]
    fn a_launch_goes_on_while_its_agent_works_or_is_not_detected_yet() {
        // Working on its first prompt, before its session report.
        let working = agent(serde_json::json!({}));
        assert_eq!(launch_outcome(&params(), Some(&working), None, false), None);
        // Not detected yet: the name is all there is.
        let undetected = agent(serde_json::json!({"agent": null, "agent_status": "unknown"}));
        assert_eq!(
            launch_outcome(&params(), Some(&undetected), None, false),
            None
        );
    }

    #[test]
    fn the_launch_ends_once_the_agent_takes_prompts() {
        let ready = agent(serde_json::json!({"launch_pending": false, "interactive_ready": true}));
        assert_eq!(
            launch_outcome(&params(), Some(&ready), None, false),
            Some(LaunchOutcome::Ready(Box::new(ready)))
        );
    }

    #[test]
    fn a_dialog_or_a_limit_ends_the_launch() {
        let blocked = agent(serde_json::json!({"agent_status": "blocked"}));
        assert_eq!(
            launch_outcome(&params(), Some(&blocked), None, false),
            Some(LaunchOutcome::Blocked(Box::new(blocked)))
        );
        let limited = agent(serde_json::json!({
            "agent_status": "idle",
            "limited": {"kind": "credits", "message": "out of credits"},
        }));
        assert_eq!(
            failed_code(launch_outcome(&params(), Some(&limited), None, false)),
            "agent_limited"
        );
    }

    #[test]
    fn an_exit_a_closed_pane_or_another_program_ends_the_launch() {
        assert_eq!(
            failed_code(launch_outcome(&params(), None, None, false)),
            "agent_start_failed"
        );
        assert_eq!(
            failed_code(launch_outcome(&params(), None, None, true)),
            "agent_start_failed"
        );
        let other_kind = agent(serde_json::json!({"name": null, "agent": "codex"}));
        assert_eq!(
            failed_code(launch_outcome(&params(), None, Some(&other_kind), false)),
            "agent_kind_mismatch"
        );
        let named_other_kind = agent(serde_json::json!({"agent": "codex"}));
        assert_eq!(
            failed_code(launch_outcome(
                &params(),
                Some(&named_other_kind),
                None,
                false
            )),
            "agent_kind_mismatch"
        );
    }

    #[test]
    fn a_name_that_moved_to_another_terminal_is_not_this_launch() {
        let elsewhere = agent(serde_json::json!({"terminal_id": "term_2"}));
        assert_eq!(
            failed_code(launch_outcome(&params(), Some(&elsewhere), None, false)),
            "agent_name_not_found"
        );
        let unnamed = agent(serde_json::json!({"name": null}));
        assert_eq!(
            failed_code(launch_outcome(&params(), None, Some(&unnamed), false)),
            "agent_name_not_found"
        );
    }
}
