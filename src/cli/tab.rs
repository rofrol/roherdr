use std::collections::HashMap;

use std::io::IsTerminal;

use crate::api::schema::{
    Method, Request, TabCreateParams, TabListParams, TabRenameParams, TabSetParentParams,
    TabSetStatusParams, TabStatus, TabTarget,
};

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "rename" => tab_rename(&args[1..]),
        "close" => tab_close(&args[1..]),
        "parent" => tab_parent(&args[1..]),
        "status" => tab_status(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::runtime::tab_list(TabListParams { workspace_id })
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;
    let mut env = HashMap::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--env" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --env");
                    return Ok(2);
                };
                let (key, value) = match super::parse_env_assignment(value) {
                    Ok(pair) => pair,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                env.insert(key, value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::runtime::tab_create(TabCreateParams {
        workspace_id,
        cwd,
        focus,
        label,
        env,
    })
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_get(super::normalize_tab_id(raw_tab_id))
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_focus(super::normalize_tab_id(raw_tab_id))
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    super::runtime::tab_rename(TabRenameParams {
        tab_id: super::normalize_tab_id(&args[0]),
        label: args[1..].join(" "),
    })
}

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    }

    let tab_id = super::normalize_tab_id(raw_tab_id);
    let response = super::send_request(&Request {
        id: "cli:tab:close".into(),
        method: Method::TabClose(TabTarget {
            tab_id: tab_id.clone(),
        }),
    })?;
    if response
        .pointer("/error/code")
        .and_then(|code| code.as_str())
        != Some("tab_has_children")
    {
        return super::print_response(&response);
    }
    // The server never closes child tabs (usually running jobs) implicitly: ask.
    if !std::io::stdin().is_terminal() {
        return super::print_response(&response);
    }
    let Some((tab_id, children)) = tab_children(&tab_id)? else {
        return super::print_response(&response);
    };
    let prompt = format!(
        "Tab {tab_id} has {} child tab(s): {}. Close them and the tab?",
        children.len(),
        child_status_summary(&children)
    );
    if !super::plugin::confirm(&prompt)? {
        eprintln!("tab close cancelled");
        return Ok(1);
    }
    for (child_id, _) in children {
        let response = super::send_request(&Request {
            id: "cli:tab:close".into(),
            method: Method::TabClose(TabTarget { tab_id: child_id }),
        })?;
        if response.get("error").is_some() {
            return super::print_response(&response);
        }
    }
    super::runtime::tab_close(tab_id)
}

/// The tab's canonical id and its child tabs' ids and statuses.
#[allow(clippy::type_complexity)] // a one-off pair of (id, children) read from JSON
fn tab_children(
    tab_id: &str,
) -> std::io::Result<Option<(String, Vec<(String, Option<TabStatus>)>)>> {
    let tab = super::send_request(&Request {
        id: "cli:tab:get".into(),
        method: Method::TabGet(TabTarget {
            tab_id: tab_id.to_string(),
        }),
    })?;
    let (Some(tab_id), Some(workspace_id)) = (
        tab.pointer("/result/tab/tab_id").and_then(|v| v.as_str()),
        tab.pointer("/result/tab/workspace_id")
            .and_then(|v| v.as_str()),
    ) else {
        return Ok(None);
    };
    let list = super::send_request(&Request {
        id: "cli:tab:list".into(),
        method: Method::TabList(TabListParams {
            workspace_id: Some(workspace_id.to_string()),
        }),
    })?;
    let children = list
        .pointer("/result/tabs")
        .and_then(|tabs| tabs.as_array())
        .into_iter()
        .flatten()
        .filter(|tab| tab.get("parent_tab_id").and_then(|v| v.as_str()) == Some(tab_id))
        .filter_map(|tab| {
            let id = tab.get("tab_id")?.as_str()?.to_string();
            let status = tab
                .get("status")
                .and_then(|status| serde_json::from_value(status.clone()).ok());
            Some((id, status))
        })
        .collect();
    Ok(Some((tab_id.to_string(), children)))
}

fn child_status_summary(children: &[(String, Option<TabStatus>)]) -> String {
    let count = |wanted: Option<TabStatus>| {
        children
            .iter()
            .filter(|(_, status)| *status == wanted)
            .count()
    };
    [
        (count(Some(TabStatus::Running)), "running"),
        (count(Some(TabStatus::Failed)), "failed"),
        (count(Some(TabStatus::Succeeded)), "succeeded"),
        (
            children.len()
                - count(Some(TabStatus::Running))
                - count(Some(TabStatus::Failed))
                - count(Some(TabStatus::Succeeded)),
            "other",
        ),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, name)| format!("{count} {name}"))
    .collect::<Vec<_>>()
    .join(", ")
}

fn tab_parent(args: &[String]) -> std::io::Result<i32> {
    let (Some(raw_tab_id), Some(parent), 2) = (args.first(), args.get(1), args.len()) else {
        eprintln!("usage: herdr tab parent <tab_id> <parent_tab_id>|none");
        return Ok(2);
    };
    let parent_tab_id = (parent != "none").then(|| super::normalize_tab_id(parent));
    super::print_response(&super::send_request(&Request {
        id: "cli:tab:parent".into(),
        method: Method::TabSetParent(TabSetParentParams {
            tab_id: super::normalize_tab_id(raw_tab_id),
            parent_tab_id,
        }),
    })?)
}

fn tab_status(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: herdr tab status <tab_id> running|succeeded|failed|none";
    let (Some(raw_tab_id), Some(value), 2) = (args.first(), args.get(1), args.len()) else {
        eprintln!("{usage}");
        return Ok(2);
    };
    let status = match value.as_str() {
        "running" => Some(TabStatus::Running),
        "succeeded" => Some(TabStatus::Succeeded),
        "failed" => Some(TabStatus::Failed),
        "none" => None,
        _ => {
            eprintln!("{usage}");
            return Ok(2);
        }
    };
    super::print_response(&super::send_request(&Request {
        id: "cli:tab:status".into(),
        method: Method::TabSetStatus(TabSetStatusParams {
            tab_id: super::normalize_tab_id(raw_tab_id),
            status,
        }),
    })?)
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab close <tab_id>");
    eprintln!("  herdr tab parent <tab_id> <parent_tab_id>|none");
    eprintln!("  herdr tab status <tab_id> running|succeeded|failed|none");
}
