use std::time::{Duration, Instant};

use crate::api::schema::{
    AgentPromptConfirmedParams, AgentPromptParams, AgentPromptStatusParams,
    AgentPromptTrackedParams, AgentPromptTurnParams, AgentPromptWaitOptions, AgentReadParams,
    AgentRenameParams, AgentSendKeysParams, AgentStartParams, AgentTarget, AgentWaitChangeParams,
    AgentWaitParams, AgentWaitTurnParams, EmptyParams, ErrorBody, ErrorResponse, Method,
    PaneProcessInfoParams, PaneTarget, ReadFormat, ReadSource, Request,
};

const AGENT_START_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PANE_SHELL_READINESS_RETRY_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_agent_help();
        return Ok(2);
    };

    // The prompt text is the second positional argument and may be `--global` itself.
    let text_position = (subcommand == "prompt").then_some(1);
    let (global, scoped_args) = split_global_flag(&args[1..], text_position);
    let scope = || NameScope::for_caller(global);
    match subcommand {
        "get" => agent_get(&scoped_args, scope()),
        "read" => agent_read(&scoped_args, scope()),
        "send-keys" => agent_send_keys(&scoped_args, scope()),
        "prompt" => agent_prompt(&scoped_args, scope()),
        "rename" => agent_rename(&scoped_args, scope()),
        "focus" => agent_focus(&scoped_args, scope()),
        "wait" => agent_wait(&scoped_args, scope()),
        "wait-turn" => agent_wait_turn(&args[1..]),
        "wait-change" => agent_wait_change(&scoped_args, scope()),
        "prompt-status" => agent_prompt_status(&args[1..]),
        "attach" => agent_attach(&scoped_args, scope()),
        "explain" => agent_explain(&scoped_args, scope()),
        "list" => agent_list(&args[1..]),
        "awaiting-reply" => agent_awaiting_reply(&args[1..]),
        "limited" => agent_limited(&args[1..]),
        "set-task" => agent_set_task(&args[1..]),
        "start" => agent_start(&args[1..]),
        "handoff" => agent_handoff(&args[1..]),
        "help" | "--help" | "-h" => {
            print_agent_help();
            Ok(0)
        }
        _ => {
            print_agent_help();
            Ok(2)
        }
    }
}

/// Where an agent name given as a target is looked up first. A pane id or
/// terminal id target is unaffected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NameScope {
    prefer_workspace_id: Option<String>,
}

impl NameScope {
    /// The caller's workspace (`HERDR_WORKSPACE_ID`) unless `--global` was
    /// given; a caller outside a pane or on a remote machine has none.
    fn for_caller(global: bool) -> Self {
        Self::from_parts(global, super::target::caller_workspace_id().as_deref())
    }

    fn from_parts(global: bool, caller_workspace_id: Option<&str>) -> Self {
        Self {
            prefer_workspace_id: (!global)
                .then_some(caller_workspace_id)
                .flatten()
                .map(super::normalize_workspace_id),
        }
    }

    fn workspace(workspace_id: Option<&str>) -> Self {
        Self {
            prefer_workspace_id: workspace_id.map(str::to_owned),
        }
    }

    fn target(&self, target: &str) -> AgentTarget {
        AgentTarget {
            target: target.to_owned(),
            prefer_workspace_id: self.prefer_workspace_id.clone(),
        }
    }
}

/// Removes every `--global` from `args` except one that lands at
/// `keep_position` among the remaining arguments, and reports whether one
/// was removed.
fn split_global_flag(args: &[String], keep_position: Option<usize>) -> (bool, Vec<String>) {
    let mut global = false;
    let mut rest = Vec::with_capacity(args.len());
    for arg in args {
        if arg == "--global" && Some(rest.len()) != keep_position {
            global = true;
        } else {
            rest.push(arg.clone());
        }
    }
    (global, rest)
}

fn agent_explain(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let mut file = None;
    let mut agent = None;
    let mut json = false;
    let mut verbose = false;
    let mut target = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --file");
                    return Ok(2);
                };
                file = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                match value.as_str() {
                    "json" => json = true,
                    "text" => json = false,
                    other => {
                        eprintln!("invalid --format: {other} (expected text or json)");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent explain <target> [--json|--verbose]");
                eprintln!(
                    "usage: herdr agent explain --file PATH --agent LABEL [--json|--verbose]"
                );
                return Ok(0);
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Ok(2);
            }
            value => {
                if target.is_some() {
                    eprintln!("usage: herdr agent explain <target> [--json]");
                    return Ok(2);
                }
                target = Some(value.to_string());
                index += 1;
            }
        }
    }

    let explain = if let Some(path) = file {
        if target.is_some() {
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        }
        let Some(agent_label) = agent else {
            eprintln!("herdr agent explain --file requires --agent LABEL");
            return Ok(2);
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                let response = ErrorResponse {
                    id: "cli:agent:explain".into(),
                    error: ErrorBody {
                        code: "agent_explain_file_read_failed".into(),
                        message: format!("failed to read agent explain file {path}: {err}"),
                    },
                };
                let response = serde_json::to_string(&response).map_err(std::io::Error::other)?;
                eprintln!("{response}");
                return Ok(1);
            }
        };
        crate::detect::manifest::explain_to_json_value(&crate::detect::manifest::explain_for_label(
            &agent_label,
            &content,
        ))
    } else {
        let Some(target) = target else {
            eprintln!("usage: herdr agent explain <target> [--json]");
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        };
        if agent.is_some() {
            eprintln!("--agent is only valid with --file");
            return Ok(2);
        }

        let response = super::send_request(&Request {
            id: "cli:agent:explain".into(),
            method: Method::AgentExplain(scope.target(&target)),
        })?;
        if response.get("error").is_some() {
            eprintln!("{}", serde_json::to_string(&response).unwrap());
            return Ok(1);
        }
        response["result"]["explain"].clone()
    };

    if json {
        println!("{explain}");
    } else {
        print_agent_explain_text(&explain, verbose);
    }
    Ok(0)
}

fn print_agent_explain_text(explain: &serde_json::Value, verbose: bool) {
    println!("agent: {}", explain["agent"].as_str().unwrap_or("unknown"));
    println!("state: {}", explain["state"].as_str().unwrap_or("unknown"));
    println!(
        "manifest: {} {}",
        explain["manifest_source"].as_str().unwrap_or("none"),
        explain["manifest_version"].as_str().unwrap_or("unknown")
    );
    if let Some(rule) = explain["matched_rule"].as_object() {
        let rule_id = rule
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        println!(
            "rule: {} (region={} priority={})",
            rule_id,
            rule.get("region")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            rule.get("priority")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        if let Some(preview) = matched_rule_region_preview(explain, rule_id) {
            println!("evidence: {preview:?}");
        }
    } else {
        println!("rule: none");
    }
    if let Some(reason) = explain["fallback_reason"].as_str() {
        println!("fallback_reason: {reason}");
    }
    if let Some(reason) = explain["screen_detection_skip_reason"].as_str() {
        println!("screen_detection_skip_reason: {reason}");
    }
    if let Some(reason) = explain["skipped_update_reason"].as_str() {
        println!("skipped_update_reason: {reason}");
    }
    if let Some(warning) = explain["warning"].as_str() {
        println!("warning: {warning}");
    }

    if !verbose {
        return;
    }

    println!(
        "visible: idle={} blocker={} working={}",
        explain["visible_idle"].as_bool().unwrap_or(false),
        explain["visible_blocker"].as_bool().unwrap_or(false),
        explain["visible_working"].as_bool().unwrap_or(false)
    );
    println!(
        "cached_remote_version: {}",
        explain["cached_remote_version"].as_str().unwrap_or("none")
    );
    println!(
        "local_override_shadowing_remote: {}",
        explain["local_override_shadowing_remote"]
            .as_bool()
            .unwrap_or(false)
    );
    if let Some(status) = explain["remote_update_status"].as_str() {
        println!("remote_update_status: {status}");
    }
    if let Some(error) = explain["remote_update_error"].as_str() {
        println!("remote_update_error: {error}");
    }
    if let Some(evaluated_rules) = explain["evaluated_rules"]
        .as_array()
        .filter(|rules| !rules.is_empty())
    {
        println!("evaluated_rules:");
        for rule in evaluated_rules {
            println!(
                "  {} {} priority={} region={} state={}",
                if rule["matched"].as_bool().unwrap_or(false) {
                    "✓"
                } else {
                    "✗"
                },
                rule["id"].as_str().unwrap_or("-"),
                rule["priority"].as_i64().unwrap_or(0),
                rule["region"].as_str().unwrap_or("-"),
                rule["state"].as_str().unwrap_or("unknown")
            );
            let evidence = &rule["evidence"];
            println!(
                "    matchers: contains={:?} regex={:?} line_regex={:?} all={} any={} not={}",
                evidence["contains"],
                evidence["regex"],
                evidence["line_regex"],
                evidence["all_count"].as_u64().unwrap_or(0),
                evidence["any_count"].as_u64().unwrap_or(0),
                evidence["not_count"].as_u64().unwrap_or(0)
            );
            println!(
                "    region: bytes={} preview={:?}",
                evidence["region_bytes"].as_u64().unwrap_or(0),
                evidence["region_preview"].as_str().unwrap_or("")
            );
        }
    }
}

fn matched_rule_region_preview<'a>(
    explain: &'a serde_json::Value,
    rule_id: &str,
) -> Option<&'a str> {
    explain["evaluated_rules"]
        .as_array()?
        .iter()
        .find(|rule| rule["id"].as_str() == Some(rule_id))?["evidence"]["region_preview"]
        .as_str()
        .filter(|preview| !preview.is_empty())
}

fn agent_start(args: &[String]) -> std::io::Result<i32> {
    let Some(name) = args.first() else {
        eprintln!("usage: herdr agent start <name> --kind KIND --pane ID [--timeout MS] [-- <agent-args...>]");
        return Ok(2);
    };
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut kind = None;
    let mut pane_id = None;
    let mut timeout_ms = None;
    let mut index = 1;
    while index < separator {
        match args[index].as_str() {
            "--kind" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --kind");
                    return Ok(2);
                };
                kind = Some(value.clone());
                index += 2;
            }
            "--pane" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --pane");
                    return Ok(2);
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = match parse_timeout(value) {
                    Ok(timeout_ms) => Some(timeout_ms),
                    Err(exit_code) => return Ok(exit_code),
                };
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let Some(kind) = kind else {
        eprintln!("missing required --kind");
        return Ok(2);
    };
    let Some(pane_id) = pane_id else {
        eprintln!("missing required --pane");
        return Ok(2);
    };
    let Some(expected_kind) = crate::detect::parse_agent_label(&kind) else {
        eprintln!("unsupported interactive agent kind: {kind}");
        return Ok(2);
    };
    let expected_kind = crate::detect::agent_label(expected_kind).to_string();
    let agent_args = if separator < args.len() {
        args[separator + 1..].to_vec()
    } else {
        Vec::new()
    };
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
    let retryable_timeout = timeout > crate::app::AGENT_START_SETTLE_DELAY
        && timeout <= crate::app::MAX_AGENT_START_TIMEOUT;
    let pinned_terminal_id = pane_terminal_id(&pane_id)?;
    let mut retry_deadline = None;
    let mut previous_busy_response = None;
    let mut response = loop {
        if let Some(previous_busy_response) = previous_busy_response.as_ref() {
            let retry_expired = retry_deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if retry_expired
                || pane_terminal_id(&pane_id)? != pinned_terminal_id
                || !pane_shell_is_initializing(&pane_id)?
            {
                return super::print_response(previous_busy_response);
            }
        }

        let response = super::send_request(&Request {
            id: "cli:agent:start".into(),
            method: Method::AgentStart(AgentStartParams {
                name: name.clone(),
                kind: kind.clone(),
                pane_id: pane_id.clone(),
                args: agent_args.clone(),
                timeout_ms,
            }),
        })?;
        if response.get("error").is_none() {
            break response;
        }
        if response["error"]["code"].as_str() != Some("agent_pane_busy")
            || !retryable_timeout
            || pinned_terminal_id.is_none()
            || pane_terminal_id(&pane_id)? != pinned_terminal_id
            || !pane_shell_is_initializing(&pane_id)?
        {
            return super::print_response(&response);
        }

        let deadline = *retry_deadline
            .get_or_insert_with(|| Instant::now() + PANE_SHELL_READINESS_RETRY_TIMEOUT);
        previous_busy_response = Some(response);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if let Some(previous_busy_response) = previous_busy_response.as_ref() {
                return super::print_response(previous_busy_response);
            }
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL.min(remaining));
    };

    let Some(expected_terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        return super::print_response(&cli_agent_error(
            "cli:agent:start",
            "agent_start_failed",
            "agent start response did not include terminal_id",
        ));
    };
    if pinned_terminal_id
        .as_deref()
        .is_some_and(|pinned| pinned != expected_terminal_id)
    {
        return super::print_response(&agent_name_lost_error("cli:agent:start", name));
    }
    // Names are unique per workspace, so look the new name up in the started pane's workspace.
    let scope = NameScope::workspace(response["result"]["agent"]["workspace_id"].as_str());
    let waited = wait_for_named_agent(
        name,
        &scope,
        &pane_id,
        timeout,
        &expected_kind,
        expected_terminal_id,
    );
    match waited {
        Ok(Ok(agent)) => {
            response["result"]["agent"] = agent;
            super::print_response(&response)
        }
        Ok(Err(error)) => super::print_response(&error),
        Err(err) => {
            print_agent_transport_error(err, "cli:agent:start", "agent_start_transport_failed")
        }
    }
}

fn agent_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr agent list");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?)
}

fn agent_get(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(scope.target(target)),
    })?)
}

fn agent_focus(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(scope.target(target)),
    })?)
}

/// Run by the agent itself right before it ends a turn with a question for the user,
/// optionally with the question in a few words.
fn agent_awaiting_reply(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr agent awaiting-reply [--pane PANE_ID] [QUESTION]";
    let Some((pane_id, words)) = pane_and_words(args) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let Some(pane_id) = pane_id.or_else(super::target::caller_pane_id) else {
        eprintln!("herdr agent awaiting-reply: no --pane given and HERDR_PANE_ID is not set");
        return Ok(2);
    };
    let question = words.join(" ");
    super::send_ok_request(Method::PaneReportAwaitingReply(
        crate::api::schema::PaneReportAwaitingReplyParams {
            pane_id,
            question: (!question.trim().is_empty()).then_some(question),
        },
    ))
}

/// Run by an agent's hook when a limit ended its turn: `usage` (it resets on its own) or
/// `credits` (waiting does not help), with the agent's error text.
fn agent_limited(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr agent limited [--pane PANE_ID] usage|credits [MESSAGE]";
    let Some((pane_id, words)) = pane_and_words(args) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let kind = match words.first().copied() {
        Some("usage") => crate::api::schema::AgentLimitKind::Usage,
        Some("credits") => crate::api::schema::AgentLimitKind::Credits,
        _ => {
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    let Some(pane_id) = pane_id.or_else(super::target::caller_pane_id) else {
        eprintln!("herdr agent limited: no --pane given and HERDR_PANE_ID is not set");
        return Ok(2);
    };
    let message = words[1..].join(" ");
    super::send_ok_request(Method::PaneReportLimit(
        crate::api::schema::PaneReportLimitParams {
            pane_id,
            kind,
            message: (!message.trim().is_empty()).then_some(message),
        },
    ))
}

/// Splits `[--pane PANE_ID] [WORDS...]`, all words after `--`; None when `--pane` has no
/// value.
fn pane_and_words(args: &[String]) -> Option<(Option<String>, Vec<&str>)> {
    let mut pane_id = None;
    let mut words = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pane" => pane_id = Some(args.next()?.clone()),
            "--" => {
                words.extend(args.by_ref().map(String::as_str));
            }
            word => words.push(word),
        }
    }
    Some((pane_id, words))
}

/// Run by the agent itself when it starts a task: a few words that name the
/// tab while it works on it.
fn agent_set_task(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr agent set-task [--pane PANE_ID] <task>|--clear";
    let mut pane_id = None;
    let mut words = Vec::new();
    let mut clear = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pane" => match args.next() {
                Some(pane) => pane_id = Some(pane.clone()),
                None => {
                    eprintln!("{USAGE}");
                    return Ok(2);
                }
            },
            "--clear" => clear = true,
            _ => words.push(arg.as_str()),
        }
    }
    let task = words.join(" ");
    // Exactly one of a task and `--clear`.
    let has_task = !task.trim().is_empty();
    if clear == has_task {
        eprintln!("{USAGE}");
        return Ok(2);
    }
    let Some(pane_id) = pane_id.or_else(super::target::caller_pane_id) else {
        eprintln!("herdr agent set-task: no --pane given and HERDR_PANE_ID is not set");
        return Ok(2);
    };
    super::send_ok_request(Method::PaneReportTask(
        crate::api::schema::PaneReportTaskParams {
            pane_id,
            task: (!clear).then_some(task),
        },
    ))
}

fn agent_handoff(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr agent handoff <pane> --to claude|pi|codex [--focus]";
    let mut pane_id = None;
    let mut to = None;
    let mut focus = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--to" => match args.next() {
                Some(agent) => to = Some(agent.clone()),
                None => {
                    eprintln!("{USAGE}");
                    return Ok(2);
                }
            },
            "--focus" => focus = true,
            pane if pane_id.is_none() && !pane.starts_with('-') => {
                pane_id = Some(super::normalize_pane_id(pane));
            }
            _ => {
                eprintln!("{USAGE}");
                return Ok(2);
            }
        }
    }
    let (Some(pane_id), Some(to)) = (pane_id, to) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:handoff".into(),
        method: Method::AgentHandoff(crate::api::schema::AgentHandoffParams { pane_id, to, focus }),
    })?)
}

fn agent_attach(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let (target, takeover) =
        match super::parse_attach_target(args, "usage: herdr agent attach <target> [--takeover]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let response = resolve_agent_target(&target, &scope, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    Ok(0)
}

fn agent_wait(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent wait <target> [--until STATUS]... [--timeout MS]");
        return Ok(2);
    };
    let mut until = Vec::new();
    let mut timeout_ms = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--until" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--until requires at least one status");
                    return Ok(2);
                };
                let status = match super::parse_agent_status(value) {
                    Ok(status) => status,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                until.push(status);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = match parse_timeout(value) {
                    Ok(timeout_ms) => Some(timeout_ms),
                    Err(exit_code) => return Ok(exit_code),
                };
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent wait <target> [--until STATUS]... [--timeout MS]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let what = format!("agent {target}");
    let reply = super::reconnect::wait("agent wait", &what, |elapsed| Request {
        id: "cli:agent:wait".into(),
        method: Method::AgentWait(AgentWaitParams {
            target: target.clone(),
            prefer_workspace_id: scope.prefer_workspace_id.clone(),
            until: until.clone(),
            timeout_ms: super::reconnect::remaining_ms(timeout_ms, elapsed),
        }),
    })?;
    super::reconnect::report_gave_up("agent wait", &what, &reply);
    super::print_response(&reply.response)
}

/// `agent.wait_change`, sent once: a live handoff answers `server_handed_off`, and the caller
/// reads the agent again, since the new server's `state_change_seq` differs anyway.
fn agent_wait_change(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr agent wait-change <target> --after <state_change_seq>";
    let (Some(target), Some("--after"), Some(seq), None) = (
        args.first(),
        args.get(1).map(String::as_str),
        args.get(2),
        args.get(3),
    ) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let Ok(state_change_seq) = seq.parse::<u64>() else {
        eprintln!("--after takes a number (the agent's state_change_seq): {seq}");
        return Ok(2);
    };
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:wait_change".into(),
        method: Method::AgentWaitChange(AgentWaitChangeParams {
            target: target.clone(),
            prefer_workspace_id: scope.prefer_workspace_id,
            state_change_seq,
        }),
    })?)
}

fn agent_prompt_status(args: &[String]) -> std::io::Result<i32> {
    let [request_id] = args else {
        eprintln!("usage: herdr agent prompt-status <request_id>");
        return Ok(2);
    };
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:prompt_status".into(),
        method: Method::AgentPromptStatus(AgentPromptStatusParams {
            request_id: request_id.clone(),
        }),
    })?)
}

fn wait_for_named_agent(
    name: &str,
    scope: &NameScope,
    fallback_pane_id: &str,
    timeout: Duration,
    expected_kind: &str,
    expected_terminal_id: &str,
) -> std::io::Result<Result<serde_json::Value, serde_json::Value>> {
    let deadline = Instant::now().checked_add(timeout);
    let mut first_poll = true;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Let the server reconcile its matching startup deadline before
            // returning so the pending name is immediately reusable.
            let _ = resolve_agent_target_unchecked(name, scope, "cli:agent:start:timeout");
            return Ok(Err(agent_wait_timeout()));
        }
        let poll_id = "cli:agent:start";
        let mut response = if first_poll {
            first_poll = false;
            resolve_agent_target(name, scope, poll_id)?
        } else {
            resolve_agent_target_unchecked(name, scope, poll_id)?
        };
        if response.get("error").is_some() {
            response = resolve_agent_target_unchecked(fallback_pane_id, scope, poll_id)?;
            if response.get("error").is_some() {
                std::thread::sleep(AGENT_START_POLL_INTERVAL);
                continue;
            }
        }
        let agent = &response["result"]["agent"];
        let outcome = if agent["terminal_id"].as_str() != Some(expected_terminal_id) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else if let Some(actual) = agent["agent"]
            .as_str()
            .filter(|actual| *actual != expected_kind)
        {
            Some(Err(cli_agent_error(
                "cli:agent:start",
                "agent_kind_mismatch",
                format!("expected {expected_kind}, detected {actual}"),
            )))
        } else if agent["name"].as_str() != Some(name) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else {
            match agent["agent_status"].as_str() {
                Some("blocked") => Some(Err(cli_agent_error(
                    "cli:agent:start",
                    "agent_not_ready",
                    format!(
                        "agent {name} is blocked during startup by {} and is not ready for prompts; answer it in pane {} (an agent started in a folder it never trusted asks whether to trust it)",
                        startup_dialog(name, scope, agent),
                        agent["pane_id"].as_str().unwrap_or(fallback_pane_id),
                    ),
                ))),
                Some("unknown")
                    if expected_kind == "codex"
                        && agent["interactive_ready"].as_bool() == Some(true) =>
                {
                    Some(Ok(agent.clone()))
                }
                Some("working" | "unknown") => None,
                Some("idle" | "done") if agent["interactive_ready"].as_bool() == Some(true) => {
                    Some(Ok(agent.clone()))
                }
                Some("idle" | "done") if !agent["launch_pending"].as_bool().unwrap_or(false) => {
                    Some(Err(cli_agent_error(
                        "cli:agent:start",
                        "agent_start_failed",
                        "agent process exited before becoming interactive",
                    )))
                }
                _ => None,
            }
        };
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL);
    }
}

/// The dialog an agent blocked during startup shows: `the "<rule>" dialog` when a blocking
/// screen-detection rule matches its screen (a folder-trust prompt), else the question its
/// integration reported, else `a dialog`.
fn startup_dialog(name: &str, scope: &NameScope, agent: &serde_json::Value) -> String {
    let explained = super::send_request_unchecked(&Request {
        id: "cli:agent:start:explain".into(),
        method: Method::AgentExplain(scope.target(name)),
    });
    if let Some(dialog) = explained
        .ok()
        .and_then(|value| blocking_rule_dialog(&value["result"]["explain"]["matched_rule"]))
    {
        return dialog;
    }
    match agent["question"].as_str().map(str::trim) {
        Some(question) if !question.is_empty() => format!("the question \"{question}\""),
        _ => "a dialog".to_string(),
    }
}

/// `the "<id>" dialog` for a matched screen-detection rule whose state is `blocked`.
fn blocking_rule_dialog(rule: &serde_json::Value) -> Option<String> {
    (rule["state"] == "blocked")
        .then(|| rule["id"].as_str())
        .flatten()
        .map(|id| format!("the \"{id}\" dialog"))
}

fn pane_terminal_id(pane_id: &str) -> std::io::Result<Option<String>> {
    let response = super::send_request(&Request {
        id: "cli:agent:start:pane".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_owned(),
        }),
    })?;
    Ok(response["result"]["pane"]["terminal_id"]
        .as_str()
        .map(str::to_owned))
}

fn pane_shell_is_initializing(pane_id: &str) -> std::io::Result<bool> {
    let response = super::send_request(&Request {
        id: "cli:agent:start:process_info".into(),
        method: Method::PaneProcessInfo(PaneProcessInfoParams {
            pane_id: Some(pane_id.to_owned()),
        }),
    })?;
    Ok(process_info_shows_shell_initialization(
        &response["result"]["process_info"],
    ))
}

#[cfg(unix)]
fn process_info_shows_shell_initialization(process_info: &serde_json::Value) -> bool {
    let Some(shell_pid) = process_info["shell_pid"].as_u64() else {
        return false;
    };
    if process_info["foreground_process_group_id"].as_u64() != Some(shell_pid) {
        return false;
    }
    process_info["foreground_processes"]
        .as_array()
        .is_some_and(|processes| {
            processes.iter().any(|process| {
                process["pid"].as_u64() == Some(shell_pid)
                    && (process["name"]
                        .as_str()
                        .is_some_and(crate::platform::is_pane_shell_process_name)
                        || process["argv"]
                            .as_array()
                            .and_then(|argv| argv.first())
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(crate::platform::is_pane_shell_process_name))
            })
        })
}

// Windows exposes no foreground process group, so shell initialization is not
// observable and a busy `agent.start` is not retried there.
#[cfg(not(unix))]
fn process_info_shows_shell_initialization(_process_info: &serde_json::Value) -> bool {
    false
}

fn agent_name_lost_error(request_id: &str, expected_name: &str) -> serde_json::Value {
    cli_agent_error(
        request_id,
        "agent_name_not_found",
        format!("named agent {expected_name} no longer owns the target terminal"),
    )
}

fn print_agent_transport_error(
    err: std::io::Error,
    request_id: &str,
    code: &str,
) -> std::io::Result<i32> {
    if super::protocol_mismatch_was_reported(&err) {
        return Ok(1);
    }
    // A dead-server marker reaches here from `send_request` in the agent
    // startup path; surface its deferred response exactly once instead of
    // printing a second, generic transport-error line.
    if let Some(response) = super::server_not_running_reported_response(&err) {
        let value = serde_json::to_value(response).map_err(std::io::Error::other)?;
        return super::print_response(&value);
    }
    super::print_response(&cli_agent_error(request_id, code, err.to_string()))
}

fn agent_wait_timeout() -> serde_json::Value {
    cli_agent_error(
        "cli:agent:start",
        "timeout",
        "timed out waiting for agent startup",
    )
}

fn cli_agent_error(id: &str, code: &str, message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn resolve_agent_target(
    target: &str,
    scope: &NameScope,
    request_id: &str,
) -> std::io::Result<serde_json::Value> {
    super::send_request(&agent_get_request(target, scope, request_id))
}

fn resolve_agent_target_unchecked(
    target: &str,
    scope: &NameScope,
    request_id: &str,
) -> std::io::Result<serde_json::Value> {
    super::send_request_unchecked(&agent_get_request(target, scope, request_id))
}

fn agent_get_request(target: &str, scope: &NameScope, request_id: &str) -> Request {
    Request {
        id: request_id.into(),
        method: Method::AgentGet(scope.target(target)),
    }
}

fn agent_rename(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let [target, value] = args else {
        eprintln!("usage: herdr agent rename <target> <name>|--clear");
        return Ok(2);
    };
    let name = if value == "--clear" {
        None
    } else {
        Some(value.clone())
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:rename".into(),
        method: Method::AgentRename(AgentRenameParams {
            target: target.clone(),
            prefer_workspace_id: scope.prefer_workspace_id,
            name,
        }),
    })?)
}

fn agent_prompt(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!(
            "usage: herdr agent prompt <target> <text> [--wait] [--until STATUS]... [--timeout MS]"
        );
        return Ok(2);
    };
    let Some(text) = args.get(1) else {
        eprintln!("agent prompt requires text");
        return Ok(2);
    };
    let mut wait = false;
    let mut until = Vec::new();
    let mut timeout_ms = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--wait" => {
                wait = true;
                index += 1;
            }
            "--until" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--until requires at least one status");
                    return Ok(2);
                };
                let status = match super::parse_agent_status(value) {
                    Ok(status) => status,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                until.push(status);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = match parse_timeout(value) {
                    Ok(timeout_ms) => Some(timeout_ms),
                    Err(exit_code) => return Ok(exit_code),
                };
                index += 2;
            }
            option => {
                eprintln!("unknown option: {option}");
                return Ok(2);
            }
        }
    }
    if !until.is_empty() && !wait {
        eprintln!("--until requires --wait");
        return Ok(2);
    }
    if timeout_ms.is_some() && !wait {
        eprintln!("--timeout requires --wait");
        return Ok(2);
    }
    if wait && until.is_empty() {
        match prompt_and_wait_for_turn(target, text, &scope, timeout_ms)? {
            TurnWait::Done(exit_code) => return Ok(exit_code),
            TurnWait::Untracked => eprintln!(
                "herdr: agent {target} does not report its turns; waiting for its state instead, which may match a turn that was already running"
            ),
        }
    }
    if !wait {
        let response = super::send_request(&Request {
            id: "cli:agent:prompt_confirmed".into(),
            method: Method::AgentPromptConfirmed(AgentPromptConfirmedParams {
                target: target.clone(),
                prefer_workspace_id: scope.prefer_workspace_id.clone(),
                text: text.clone(),
            }),
        })?;
        if !method_unknown(&response, "agent.prompt_confirmed") {
            return super::print_response(&response);
        }
        // An older server refused the method before typing anything.
        eprintln!(
            "herdr: the server does not confirm prompts; sending without waiting for the agent to accept it"
        );
    }
    let response = super::send_request(&Request {
        id: "cli:agent:prompt".into(),
        method: Method::AgentPrompt(AgentPromptParams {
            follow_turn: false,
            target: target.clone(),
            prefer_workspace_id: scope.prefer_workspace_id,
            text: text.clone(),
            wait: wait.then_some(AgentPromptWaitOptions {
                until,
                timeout_ms,
                submission_deadline: None,
            }),
        }),
    })?;
    super::print_response(&response)
}

enum TurnWait {
    /// The prompt went out and the command is done, with this exit code.
    Done(i32),
    /// Nothing was typed: the agent reports no turns, or the server follows none.
    Untracked,
}

/// Types the prompt and waits for the turn it starts. Without a timeout it uses
/// `agent.prompt_tracked`, prints the followed request's id, then waits with `agent.wait_turn`;
/// with a timeout, or against a server without those methods, `agent.prompt_turn`.
fn prompt_and_wait_for_turn(
    target: &str,
    text: &str,
    scope: &NameScope,
    timeout_ms: Option<u64>,
) -> std::io::Result<TurnWait> {
    if timeout_ms.is_none() {
        let response = super::send_request(&Request {
            id: "cli:agent:prompt_tracked".into(),
            method: Method::AgentPromptTracked(AgentPromptTrackedParams {
                target: target.to_string(),
                prefer_workspace_id: scope.prefer_workspace_id.clone(),
                text: text.to_string(),
            }),
        })?;
        if let Some(request_id) = response["result"]["prompt_request"]["request_id"].as_str() {
            // Printed before the wait, so a caller whose wait is cut off can resume it.
            eprintln!("herdr: waiting for the turn of prompt request {request_id}");
            return wait_for_turn(request_id).map(TurnWait::Done);
        }
        if !method_unknown(&response, "agent.prompt_tracked") {
            if prompt_turn_unavailable(&response) {
                return Ok(TurnWait::Untracked);
            }
            return super::print_response(&response).map(TurnWait::Done);
        }
    }
    let response = super::send_request(&Request {
        id: "cli:agent:prompt_turn".into(),
        method: Method::AgentPromptTurn(AgentPromptTurnParams {
            target: target.to_string(),
            prefer_workspace_id: scope.prefer_workspace_id.clone(),
            text: text.to_string(),
            timeout_ms,
        }),
    })?;
    if prompt_turn_unavailable(&response) {
        return Ok(TurnWait::Untracked);
    }
    super::print_response(&response).map(TurnWait::Done)
}

fn agent_wait_turn(args: &[String]) -> std::io::Result<i32> {
    let [request_id] = args else {
        eprintln!("usage: herdr agent wait-turn <request_id>");
        return Ok(2);
    };
    wait_for_turn(request_id)
}

/// Waits with `agent.wait_turn` and prints how the turn ended. Exit 0 only for `finished`.
fn wait_for_turn(request_id: &str) -> std::io::Result<i32> {
    let what = format!("the turn of prompt request {request_id}");
    let reply = super::reconnect::wait("agent wait-turn", &what, |_| Request {
        id: "cli:agent:wait_turn".into(),
        method: Method::AgentWaitTurn(AgentWaitTurnParams {
            request_id: request_id.to_string(),
        }),
    })?;
    super::reconnect::report_gave_up("agent wait-turn", &what, &reply);
    let response = reply.response;
    let exit_code = super::print_response(&response)?;
    if exit_code == 0 && response["result"]["reason"] != "finished" {
        return Ok(1);
    }
    Ok(exit_code)
}

/// Whether an older server refused `method` because it does not know it.
fn method_unknown(response: &serde_json::Value, method: &str) -> bool {
    let error = &response["error"];
    error["code"] == "invalid_request"
        && error["message"]
            .as_str()
            .is_some_and(|message| message.contains(method))
}

/// Whether `agent.prompt_turn` refused before typing anything: the agent does not report
/// turns, or an older server does not know the method.
fn prompt_turn_unavailable(response: &serde_json::Value) -> bool {
    let error = &response["error"];
    match error["code"].as_str() {
        Some("turn_tracking_unsupported") => true,
        Some("invalid_request") => error["message"]
            .as_str()
            .is_some_and(|message| message.contains("agent.prompt_turn")),
        _ => false,
    }
}

fn agent_send_keys(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr agent send-keys <target> <key> [key ...]");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:send-keys".into(),
        method: Method::AgentSendKeys(AgentSendKeysParams {
            target: args[0].clone(),
            prefer_workspace_id: scope.prefer_workspace_id,
            keys: args[1..].to_vec(),
        }),
    })?)
}

fn agent_read(args: &[String], scope: NameScope) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent read <target> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = super::parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(super::parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = super::parse_read_format(value)?;
                strip_ansi = !matches!(format, ReadFormat::Ansi);
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let response = super::send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(AgentReadParams {
            target: target.clone(),
            prefer_workspace_id: scope.prefer_workspace_id,
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?;
    super::print_read_response(&response)
}

fn print_agent_help() {
    eprintln!("herdr agent commands:");
    eprintln!("  herdr agent list");
    eprintln!("  herdr agent get <target>");
    eprintln!("  herdr agent read <target> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!("  herdr agent send-keys <target> <key> [key ...]");
    eprintln!("  herdr agent prompt <target> <text> [--wait] [--until STATUS]... [--timeout MS]");
    eprintln!("  herdr agent rename <target> <name>|--clear");
    eprintln!("  herdr agent focus <target>");
    eprintln!("  herdr agent awaiting-reply [--pane PANE_ID] [QUESTION]");
    eprintln!("  herdr agent limited [--pane PANE_ID] usage|credits [MESSAGE]");
    eprintln!("  herdr agent set-task [--pane PANE_ID] <task>|--clear");
    eprintln!("  herdr agent wait <target> [--until STATUS]... [--timeout MS]");
    eprintln!("  herdr agent wait-turn <request_id>");
    eprintln!("  herdr agent wait-change <target> --after <state_change_seq>");
    eprintln!("  herdr agent prompt-status <request_id>");
    eprintln!("  herdr agent attach <target> [--takeover]");
    eprintln!(
        "  herdr agent start <name> --kind KIND --pane ID [--timeout MS] [-- <agent-args...>]"
    );
    eprintln!("  herdr agent handoff <pane> --to claude|pi|codex [--focus]");
    eprintln!("  herdr agent explain <target> [--json|--format text|json] [--verbose]");
    eprintln!(
        "  herdr agent explain --file PATH --agent LABEL [--json|--format text|json] [--verbose]"
    );
    eprintln!("  targets accept unique agent names and pane ids that currently host agents");
    eprintln!("  inside a pane, a name resolves in the caller's workspace first, then in all;");
    eprintln!(
        "  --global (get, read, send-keys, prompt, rename, focus, wait, wait-change, attach, explain)"
    );
    eprintln!("  looks a name up in every workspace at once");
    eprintln!("  kinds: {}", super::spec::agent_kind_values().join("|"));
}

fn parse_timeout(value: &str) -> Result<u64, i32> {
    super::parse_u64_flag("--timeout", value).map_err(|err| {
        eprintln!("{err}");
        2
    })
}

#[cfg(test)]
mod tests {
    use super::{
        blocking_rule_dialog, method_unknown, prompt_turn_unavailable, split_global_flag, NameScope,
    };

    #[test]
    fn a_blocking_rule_names_the_startup_dialog() {
        let rule = serde_json::json!({"id": "trust_directory", "state": "blocked"});
        assert_eq!(
            blocking_rule_dialog(&rule).as_deref(),
            Some("the \"trust_directory\" dialog")
        );
        let working = serde_json::json!({"id": "spinner", "state": "working"});
        assert_eq!(blocking_rule_dialog(&working), None);
        assert_eq!(blocking_rule_dialog(&serde_json::Value::Null), None);
    }

    #[test]
    fn an_older_server_without_prompt_tracked_is_recognized() {
        let request = serde_json::json!({
            "id": "x", "method": "agent.prompt_tracked_from_the_future",
            "params": {"target": "a", "text": "b"},
        });
        let message = format!(
            "invalid request: {}",
            serde_json::from_value::<crate::api::schema::Request>(request).unwrap_err()
        );
        let older = serde_json::json!({"error": {"code": "invalid_request", "message": message}});
        assert!(method_unknown(
            &older,
            "agent.prompt_tracked_from_the_future"
        ));
        let refused = serde_json::json!({
            "error": {"code": "agent_not_ready", "message": "agent.prompt_tracked"}
        });
        assert!(!method_unknown(&refused, "agent.prompt_tracked"));
    }

    #[test]
    fn prompt_turn_falls_back_only_when_nothing_was_typed() {
        let unsupported =
            serde_json::json!({"error": {"code": "turn_tracking_unsupported", "message": ""}});
        assert!(prompt_turn_unavailable(&unsupported));
        // What an older server answers for a method it does not know.
        let request = serde_json::json!({
            "id": "x", "method": "agent.prompt_turn", "params": {"target": "a", "text": "b"},
        });
        let older = serde_json::from_value::<crate::api::schema::Request>(request.clone());
        assert!(older.is_ok(), "this server knows the method");
        let mut unknown = request;
        unknown["method"] = "agent.prompt_turn_from_the_future".into();
        let message = format!(
            "invalid request: {}",
            serde_json::from_value::<crate::api::schema::Request>(unknown).unwrap_err()
        );
        assert!(prompt_turn_unavailable(
            &serde_json::json!({"error": {"code": "invalid_request", "message": message}})
        ));
        for code in ["timeout", "agent_not_running", "agent_blocked"] {
            let error =
                serde_json::json!({"error": {"code": code, "message": "agent.prompt_turn"}});
            assert!(!prompt_turn_unavailable(&error), "{code}");
        }
        assert!(!prompt_turn_unavailable(&serde_json::json!({"result": {}})));
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn global_flag_is_removed_wherever_it_appears() {
        assert_eq!(
            split_global_flag(&args(&["--global", "reviewer"]), None),
            (true, args(&["reviewer"]))
        );
        assert_eq!(
            split_global_flag(&args(&["reviewer", "--source", "recent", "--global"]), None),
            (true, args(&["reviewer", "--source", "recent"]))
        );
        assert_eq!(
            split_global_flag(&args(&["reviewer"]), None),
            (false, args(&["reviewer"]))
        );
    }

    #[test]
    fn global_flag_keeps_a_prompt_text_that_reads_global() {
        assert_eq!(
            split_global_flag(&args(&["reviewer", "--global", "--wait"]), Some(1)),
            (false, args(&["reviewer", "--global", "--wait"]))
        );
        assert_eq!(
            split_global_flag(&args(&["--global", "reviewer", "hello", "--wait"]), Some(1)),
            (true, args(&["reviewer", "hello", "--wait"]))
        );
        assert_eq!(
            split_global_flag(&args(&["reviewer", "hello", "--global"]), Some(1)),
            (true, args(&["reviewer", "hello"]))
        );
    }

    #[test]
    fn name_scope_prefers_the_caller_workspace_unless_global() {
        assert_eq!(
            NameScope::from_parts(false, Some("w_2"))
                .target("reviewer")
                .prefer_workspace_id
                .as_deref(),
            Some("w_2")
        );
        assert_eq!(
            NameScope::from_parts(true, Some("w_2")).prefer_workspace_id,
            None
        );
        assert_eq!(NameScope::from_parts(false, None).prefer_workspace_id, None);
    }
}
