//! `herdr decision`: the decision ledger in herdr's worker store
//! (`decision.add`, `decision.decide`, `decision.list`, `decision.get`),
//! printed one record per line or, with `--json`, as the server's reply.

use crate::api::schema::{
    DecisionAddParams, DecisionDecideParams, DecisionGetParams, DecisionListParams, DecisionRecord,
    DecisionSource, DecisionStatus, Method, Request,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr decision add STATEMENT [--scope SCOPE] [--item ITEM_ID] [--entry TITLE]
                     [--supersedes ID] [--answer TEXT --source SOURCE [--by NAME]]
                     [--repo DIR | --global] [--json]
      Record a question for the user (open), or with --answer a decision the
      user made (decided). SOURCE says where it came from: user (said in the
      conversation or written in the user's rules), relayed (passed on by a
      coordinator, named with --by) or menu (picked in a question menu).
      --item names the TODO item, --entry the DECISIONS.md section that
      records it; --supersedes marks an earlier record superseded. The record
      belongs to DIR's repository (default: the current directory's); with
      --global it holds in every repository. Prints its id (d-...).
  herdr decision decide ID --answer TEXT --source SOURCE [--by NAME] [--json]
      Record the user's answer to an open record. A decided record is not
      decided again: record the change with add --supersedes ID.
  herdr decision list [--repo DIR | --all] [--open | --decided | --superseded] [--json]
      The records of DIR's repository (default: the current directory's)
      and those that hold everywhere, oldest first; --all lists every
      repository's.
  herdr decision get ID [--json]
      One record.
In a coordinator tab, AskUserQuestion is allowed only when each question
cites an open record's id (in its header or text) or names a capability only
the user has (header Grant, Login, Trust or Live test); a decided id is
refused with its answer.";

pub(super) fn run_decision_command(args: &[String]) -> std::io::Result<i32> {
    let (method, json) = match parse(args) {
        Ok(Some(parsed)) => parsed,
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
    let response = super::send_request(&Request {
        id: format!("cli:decision:{}", args[0]),
        method,
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    let result = &response["result"];
    let printed = match result["type"].as_str() {
        Some("decision") => serde_json::from_value(result["decision"].clone())
            .map(|record: DecisionRecord| println!("{}", line(&record))),
        Some("decisions") => serde_json::from_value(result["decisions"].clone()).map(
            |records: Vec<DecisionRecord>| {
                if records.is_empty() {
                    println!("no decisions recorded");
                }
                for record in &records {
                    println!("{}", line(record));
                }
            },
        ),
        _ => return super::print_response(&response),
    };
    match printed {
        Ok(()) => Ok(0),
        Err(_) => super::print_response(&response),
    }
}

/// One record as a line: its id, status, scope and statement, then the
/// answer and its source, or the record that superseded it.
fn line(record: &DecisionRecord) -> String {
    let status = match record.status {
        DecisionStatus::Open => "open",
        DecisionStatus::Decided => "decided",
        DecisionStatus::Superseded => "superseded",
        DecisionStatus::Unknown => "unknown",
    };
    let mut text = format!(
        "{}  {status:<10}  [{}] {}",
        record.id, record.scope, record.statement
    );
    if let Some(answer) = &record.answer {
        let source = match (record.source, &record.relayed_by) {
            (Some(DecisionSource::Relayed), Some(by)) => format!("relayed by {by}"),
            (Some(DecisionSource::User), _) => "user".into(),
            (Some(DecisionSource::Menu), _) => "menu".into(),
            _ => "unknown".into(),
        };
        text.push_str(&format!(" -> {answer} ({source})"));
    }
    if let Some(later) = &record.superseded_by {
        text.push_str(&format!(" (superseded by {later})"));
    }
    if let Some(item) = &record.item {
        text.push_str(&format!(" {{{item}}}"));
    }
    text
}

fn parse_source(value: Option<String>) -> Result<Option<DecisionSource>, String> {
    value
        .map(|value| match value.as_str() {
            "user" => Ok(DecisionSource::User),
            "relayed" => Ok(DecisionSource::Relayed),
            "menu" => Ok(DecisionSource::Menu),
            other => Err(format!("--source is user, relayed or menu, not {other}")),
        })
        .transpose()
}

fn absolute(dir: &str) -> Result<String, String> {
    std::path::absolute(dir)
        .map(|dir| dir.display().to_string())
        .map_err(|error| format!("--repo {dir}: {error}"))
}

fn current_dir() -> Result<String, String> {
    std::env::current_dir()
        .map(|dir| dir.display().to_string())
        .map_err(|error| error.to_string())
}

/// The single positional argument a subcommand takes.
fn one_positional(rest: Vec<String>, what: &str) -> Result<String, String> {
    let mut rest = rest.into_iter();
    match (rest.next(), rest.next()) {
        (Some(value), None) if !value.starts_with("--") => Ok(value),
        (Some(value), None) => Err(format!("unknown option {value}")),
        (None, _) => Err(format!("{what} is missing")),
        (Some(_), Some(extra)) => Err(format!("unexpected argument {extra}")),
    }
}

fn parse(args: &[String]) -> Result<Option<(Method, bool)>, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    if matches!(subcommand, "help" | "--help" | "-h")
        || args[1..]
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return Ok(None);
    }
    let take_flag = |rest: &mut Vec<String>, flag: &str| {
        let before = rest.len();
        rest.retain(|arg| arg != flag);
        before != rest.len()
    };
    let mut rest: Vec<String> = args[1..].to_vec();
    let json = take_flag(&mut rest, "--json");
    let method = match subcommand {
        "add" => {
            let global = take_flag(&mut rest, "--global");
            let (repo, rest) = take_string_option(&rest, "--repo")?;
            let (scope, rest) = take_string_option(&rest, "--scope")?;
            let (item, rest) = take_string_option(&rest, "--item")?;
            let (entry, rest) = take_string_option(&rest, "--entry")?;
            let (supersedes, rest) = take_string_option(&rest, "--supersedes")?;
            let (answer, rest) = take_string_option(&rest, "--answer")?;
            let (source, rest) = take_string_option(&rest, "--source")?;
            let (by, rest) = take_string_option(&rest, "--by")?;
            let statement = one_positional(rest, "STATEMENT")?;
            if global && repo.is_some() {
                return Err("--repo and --global exclude each other".into());
            }
            let cwd = match (global, repo) {
                (true, _) => None,
                (false, Some(dir)) => Some(absolute(&dir)?),
                (false, None) => Some(current_dir()?),
            };
            Method::DecisionAdd(DecisionAddParams {
                cwd,
                statement,
                scope,
                item,
                entry,
                supersedes,
                answer,
                source: parse_source(source)?,
                relayed_by: by,
            })
        }
        "decide" => {
            let (answer, rest) = take_string_option(&rest, "--answer")?;
            let (source, rest) = take_string_option(&rest, "--source")?;
            let (by, rest) = take_string_option(&rest, "--by")?;
            let id = one_positional(rest, "ID")?;
            let (Some(answer), Some(source)) = (answer, parse_source(source)?) else {
                return Err("decide takes ID, --answer and --source".into());
            };
            Method::DecisionDecide(DecisionDecideParams {
                id,
                answer,
                source,
                relayed_by: by,
            })
        }
        "list" => {
            let all = take_flag(&mut rest, "--all");
            let mut statuses = Vec::new();
            for (flag, status) in [
                ("--open", DecisionStatus::Open),
                ("--decided", DecisionStatus::Decided),
                ("--superseded", DecisionStatus::Superseded),
            ] {
                if take_flag(&mut rest, flag) {
                    statuses.push(status);
                }
            }
            let (repo, rest) = take_string_option(&rest, "--repo")?;
            if let Some(extra) = rest.first() {
                return Err(format!("unexpected argument {extra}"));
            }
            if statuses.len() > 1 {
                return Err("--open, --decided and --superseded exclude each other".into());
            }
            if all && repo.is_some() {
                return Err("--repo and --all exclude each other".into());
            }
            let cwd = match (all, repo) {
                (true, _) => None,
                (false, Some(dir)) => Some(absolute(&dir)?),
                (false, None) => Some(current_dir()?),
            };
            Method::DecisionList(DecisionListParams {
                cwd,
                status: statuses.pop(),
            })
        }
        "get" => Method::DecisionGet(DecisionGetParams {
            id: one_positional(rest, "ID")?,
        }),
        other => return Err(format!("unknown decision command: {other}")),
    };
    Ok(Some((method, json)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn add_takes_a_statement_and_an_optional_decision() {
        let Ok(Some((Method::DecisionAdd(add), false))) = parse(&args(&[
            "add",
            "Install the integration?",
            "--repo",
            "/repo",
            "--item",
            "t-abcdefgh",
        ])) else {
            panic!("add did not parse");
        };
        assert_eq!(add.statement, "Install the integration?");
        assert_eq!(add.cwd.as_deref(), Some("/repo"));
        assert_eq!(add.item.as_deref(), Some("t-abcdefgh"));
        assert_eq!(add.answer, None);
        let Ok(Some((Method::DecisionAdd(add), true))) = parse(&args(&[
            "add",
            "Host config is the coordinator's",
            "--global",
            "--answer",
            "yes",
            "--source",
            "relayed",
            "--by",
            "todo-herdr",
            "--json",
        ])) else {
            panic!("a decided add did not parse");
        };
        assert_eq!(add.cwd, None);
        assert_eq!(add.source, Some(DecisionSource::Relayed));
        assert_eq!(add.relayed_by.as_deref(), Some("todo-herdr"));
        for bad in [
            &["add"][..],
            &["add", "a", "b"],
            &["add", "a", "--source", "chat"],
            &["add", "a", "--global", "--repo", "/r"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn decide_list_and_get_parse() {
        let Ok(Some((Method::DecisionDecide(decide), _))) = parse(&args(&[
            "decide",
            "d-abcdefgh",
            "--answer",
            "shadow first",
            "--source",
            "menu",
        ])) else {
            panic!("decide did not parse");
        };
        assert_eq!(
            (decide.id.as_str(), decide.source),
            ("d-abcdefgh", DecisionSource::Menu)
        );
        assert!(parse(&args(&["decide", "d-abcdefgh", "--answer", "x"])).is_err());
        let Ok(Some((Method::DecisionList(list), _))) = parse(&args(&["list", "--all", "--open"]))
        else {
            panic!("list did not parse");
        };
        assert_eq!((list.cwd, list.status), (None, Some(DecisionStatus::Open)));
        assert!(parse(&args(&["list", "--open", "--decided"])).is_err());
        let Ok(Some((Method::DecisionGet(get), _))) = parse(&args(&["get", "d-abcdefgh"])) else {
            panic!("get did not parse");
        };
        assert_eq!(get.id, "d-abcdefgh");
        assert!(matches!(parse(&args(&["help"])), Ok(None)));
    }
}
