//! `herdr usage --workspace`: tokens and an estimated cost per workspace,
//! read from the transcripts of the agent sessions the server lists
//! (`workspace.list`, `pane.list`, `worker.list`). The transcripts are read
//! here, in the CLI, only when the user runs it.

use std::collections::HashMap;

use serde_json::Value;

use crate::api::schema::{EmptyParams, Method, PaneListParams, Request};
use crate::usage::estimate::{
    self, ModelUsage, SessionKey, SessionSighting, TranscriptRoots, UsageEstimate, WorkspaceRef,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr usage --workspace [<id|number|label>] [--since <date>] [--json]
      Tokens (input, output, cache writes and reads) per model and an
      estimated cost for each workspace, or only the one named, summed over
      its agent sessions: those its panes run or last ran, and the headless
      workers its panes own (a worker without a live owner counts in the space
      it was started in). Each session counts once by its id, even when it was
      resumed in several panes or tabs, under the first space it was seen in.
      Reads Claude Code, Codex and pi transcripts; other agents' sessions are
      listed without tokens. Only sessions herdr still knows are counted:
      the current or last session of each open pane and the workers the
      server lists. --since counts only entries from that date (YYYY-MM-DD,
      from local midnight) or RFC 3339 time on.
The cost is an estimate, not the provider's bill: herdr's list prices (see
`price_source` in --json) times the tokens, ignoring subscriptions,
discounts and surcharges; models without a price add tokens but no cost.";

struct Options {
    selector: Option<String>,
    since: Option<String>,
    json: bool,
}

fn parse(args: &[String]) -> Result<Option<Options>, String> {
    if args.is_empty()
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        return Ok(None);
    }
    let json = args.iter().any(|arg| arg == "--json");
    let rest: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--json")
        .cloned()
        .collect();
    let (since, rest) = take_string_option(&rest, "--since")?;
    let Some(position) = rest.iter().position(|arg| arg == "--workspace") else {
        return Err("herdr usage needs --workspace".into());
    };
    let mut rest = rest;
    rest.remove(position);
    let selector = match rest.get(position) {
        Some(value) if !value.starts_with("--") => Some(rest.remove(position)),
        _ => None,
    };
    if !rest.is_empty() {
        return Err(format!("unexpected arguments: {}", rest.join(" ")));
    }
    Ok(Some(Options {
        selector,
        since,
        json,
    }))
}

pub(super) fn run_usage_command(args: &[String]) -> std::io::Result<i32> {
    let options = match parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return Ok(0);
        }
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    let utc_offset_secs = crate::usage::local_utc_offset_secs();
    let since = match options
        .since
        .as_deref()
        .map(|value| estimate::parse_since(value, utc_offset_secs))
        .transpose()
    {
        Ok(since) => since,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    let workspaces = request(
        "cli:usage:workspaces",
        Method::WorkspaceList(EmptyParams::default()),
    )?;
    let workspaces = match workspaces {
        Ok(result) => workspace_refs(&result),
        Err(response) => return super::print_response(&response),
    };
    let panes = match request(
        "cli:usage:panes",
        Method::PaneList(PaneListParams { workspace_id: None }),
    )? {
        Ok(result) => result,
        Err(response) => return super::print_response(&response),
    };
    // A server without headless workers lists no workers.
    let workers = match request(
        "cli:usage:workers",
        Method::WorkerList(EmptyParams::default()),
    )? {
        Ok(result) => result,
        Err(_) => Value::Null,
    };

    let only = match &options.selector {
        Some(selector) => match select(&workspaces.1, selector) {
            Some(id) => Some(id),
            None => {
                eprintln!("no workspace matches `{selector}`");
                return Ok(1);
            }
        },
        None => None,
    };
    let sightings = sightings(&panes, &workers);
    let report = estimate::build_report(
        &workspaces.1,
        &sightings,
        &TranscriptRoots::from_env(),
        since,
        only.as_deref(),
    );
    if options.json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("cannot print the report: {error}");
                return Ok(1);
            }
        }
    } else {
        print!("{}", render(&report, &workspaces.0));
    }
    Ok(0)
}

/// The method's `result`, or the whole response when it is an error.
fn request(id: &str, method: Method) -> std::io::Result<Result<Value, Value>> {
    let response = super::send_request(&Request {
        id: id.into(),
        method,
    })?;
    if response.get("error").is_some() {
        return Ok(Err(response));
    }
    Ok(Ok(response.get("result").cloned().unwrap_or(Value::Null)))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// The spaces in the server's order, with each one's number.
fn workspace_refs(result: &Value) -> (HashMap<String, u64>, Vec<WorkspaceRef>) {
    let mut numbers = HashMap::new();
    let mut refs = Vec::new();
    for workspace in result
        .get("workspaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = text(workspace, "workspace_id") else {
            continue;
        };
        if let Some(number) = workspace.get("number").and_then(Value::as_u64) {
            numbers.insert(id.to_string(), number);
        }
        refs.push(WorkspaceRef {
            workspace_id: id.to_string(),
            label: text(workspace, "label").unwrap_or("").to_string(),
        });
    }
    (numbers, refs)
}

/// A space by its id, then its number (1-based, as the sidebar shows), then
/// its label (exact, then ignoring case).
fn select(workspaces: &[WorkspaceRef], selector: &str) -> Option<String> {
    let by = |matches: &dyn Fn(&WorkspaceRef) -> bool| {
        workspaces
            .iter()
            .find(|w| matches(w))
            .map(|w| w.workspace_id.clone())
    };
    by(&|w| w.workspace_id == selector)
        .or_else(|| {
            selector
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1)
                .and_then(|n| workspaces.get(n - 1))
                .map(|w| w.workspace_id.clone())
        })
        .or_else(|| by(&|w| w.label == selector))
        .or_else(|| by(&|w| w.label.eq_ignore_ascii_case(selector)))
}

/// The sessions the panes report, then the workers' sessions under their
/// owner's space.
fn sightings(panes: &Value, workers: &Value) -> Vec<SessionSighting> {
    let mut sightings = Vec::new();
    let mut pane_workspaces = HashMap::new();
    for pane in panes
        .get("panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(pane_id), Some(workspace_id)) =
            (text(pane, "pane_id"), text(pane, "workspace_id"))
        else {
            continue;
        };
        pane_workspaces.insert(pane_id.to_string(), workspace_id.to_string());
        let Some(session) = pane.get("agent_session") else {
            continue;
        };
        let (Some(agent), Some(value)) = (text(session, "agent"), text(session, "value")) else {
            continue;
        };
        let key = match text(session, "kind") {
            Some("path") => SessionKey::Path(value.to_string()),
            _ => SessionKey::Id(value.to_string()),
        };
        sightings.push(SessionSighting {
            workspace_id: Some(workspace_id.to_string()),
            agent: agent.to_string(),
            session: key,
            seen_in: format!("pane {pane_id}"),
        });
    }
    for worker in workers
        .get("workers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(worker_id), Some(session_id)) =
            (text(worker, "worker_id"), text(worker, "session_id"))
        else {
            continue;
        };
        sightings.push(SessionSighting {
            workspace_id: estimate::worker_workspace(
                text(worker, "owner_pane_id"),
                text(worker, "workspace_id"),
                &pane_workspaces,
            ),
            // Headless workers run Claude Code.
            agent: "claude".into(),
            session: SessionKey::Id(session_id.to_string()),
            seen_in: format!("worker {worker_id}"),
        });
    }
    sightings
}

fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}K", count as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", count as f64 / 1e6),
        _ => format!("{:.1}B", count as f64 / 1e9),
    }
}

fn cost(model: &ModelUsage) -> String {
    match (model.cost_usd, model.cost_source) {
        (Some(cost), Some("agent_reported")) => format!("${cost:.2} (agent's figure)"),
        (Some(cost), _) => format!("${cost:.2}"),
        (None, _) => "no price".into(),
    }
}

fn model_lines(out: &mut String, indent: &str, models: &[ModelUsage]) {
    for model in models {
        out.push_str(&format!(
            "{indent}{:<24} in {:>7}  out {:>7}  cache write {:>7}  read {:>7}  {}\n",
            model.model,
            tokens(model.input_tokens),
            tokens(model.output_tokens),
            tokens(model.cache_write_tokens),
            tokens(model.cache_read_tokens),
            cost(model)
        ));
    }
}

fn render(report: &UsageEstimate, numbers: &HashMap<String, u64>) -> String {
    let mut out = format!(
        "Usage per workspace: {} (list prices as of {})\n",
        report.estimate, report.prices_as_of
    );
    out.push_str(&format!("{}\n", report.sessions_counted));
    if let Some(since) = &report.since {
        out.push_str(&format!("Since {since}\n"));
    }
    for workspace in &report.workspaces {
        let number = workspace
            .workspace_id
            .as_ref()
            .and_then(|id| numbers.get(id))
            .map(|n| format!("{n} "))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n{number}{}{}  ~${:.2}{}\n",
            workspace.label,
            workspace
                .workspace_id
                .as_ref()
                .map(|id| format!(" ({id})"))
                .unwrap_or_default(),
            workspace.cost_usd,
            if workspace.cost_incomplete {
                " + unpriced tokens"
            } else {
                ""
            }
        ));
        if workspace.sessions.is_empty() {
            out.push_str("  no agent sessions\n");
            continue;
        }
        for session in &workspace.sessions {
            out.push_str(&format!(
                "  {} {}  ({})",
                session.agent,
                session.session,
                session.seen_in.join(", ")
            ));
            if !session.also_in_workspaces.is_empty() {
                out.push_str(&format!(
                    "; also in {}",
                    session.also_in_workspaces.join(", ")
                ));
            }
            match &session.note {
                Some(note) => out.push_str(&format!(": {note}\n")),
                None => {
                    out.push('\n');
                    model_lines(&mut out, "    ", &session.models);
                }
            }
        }
        if workspace.sessions.len() > 1 && !workspace.models.is_empty() {
            out.push_str("  total\n");
            model_lines(&mut out, "    ", &workspace.models);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_workspace_selector_and_options() {
        let options = parse(&args(&[
            "--workspace",
            "herdr",
            "--since",
            "2026-10-01",
            "--json",
        ]))
        .expect("parsed")
        .expect("options");
        assert_eq!(options.selector.as_deref(), Some("herdr"));
        assert_eq!(options.since.as_deref(), Some("2026-10-01"));
        assert!(options.json);
        let options = parse(&args(&["--workspace", "--json"]))
            .expect("parsed")
            .expect("options");
        assert_eq!(options.selector, None);
        assert!(parse(&args(&["--since", "2026-10-01"])).is_err());
        assert!(parse(&args(&[])).expect("help").is_none());
    }

    #[test]
    fn selects_by_id_number_and_label() {
        let spaces = vec![
            WorkspaceRef {
                workspace_id: "w_a".into(),
                label: "herdr".into(),
            },
            WorkspaceRef {
                workspace_id: "w_b".into(),
                label: "Music".into(),
            },
        ];
        assert_eq!(select(&spaces, "w_b").as_deref(), Some("w_b"));
        assert_eq!(select(&spaces, "1").as_deref(), Some("w_a"));
        assert_eq!(select(&spaces, "music").as_deref(), Some("w_b"));
        assert_eq!(select(&spaces, "3"), None);
    }

    #[test]
    fn workers_count_under_their_owner_pane_space() {
        let panes = serde_json::json!({"panes": [
            {"pane_id": "p1", "workspace_id": "w_a",
             "agent_session": {"source": "herdr:claude", "agent": "claude", "kind": "id", "value": "s1"}},
            {"pane_id": "p2", "workspace_id": "w_b"}
        ]});
        let workers = serde_json::json!({"workers": [
            {"worker_id": "wk1", "session_id": "s2", "workspace_id": "w_a", "owner_pane_id": "p2"},
            {"worker_id": "wk2"}
        ]});
        let found = sightings(&panes, &workers);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].workspace_id.as_deref(), Some("w_a"));
        assert_eq!(found[1].workspace_id.as_deref(), Some("w_b"));
        assert_eq!(found[1].seen_in, "worker wk1");
    }

    #[test]
    fn the_text_says_it_is_an_estimate() {
        let report = estimate::build_report(&[], &[], &TranscriptRoots::default(), None, None);
        let text = render(&report, &HashMap::new());
        assert!(text.contains("estimate, not the provider's bill"));
        assert!(text.contains("Only sessions herdr still knows are counted"));
    }
}
