//! `herdr coordinator`: claims, ends and lists coordination tenures (one
//! active coordinator per repository).

use crate::api::schema::{
    CoordinatorEndParams, CoordinatorHandoffParams, CoordinatorStartParams,
    CoordinatorStatusParams, Method, Request,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr coordinator start [--repo DIR]
      Claim the repository's coordination for this pane ($HERDR_PANE_ID):
      refused (coordinator_active) while another pane coordinates it. DIR is
      a directory in the repository; this pane's directory by default.
      Setting a tab's role to coordinator does the same.
  herdr coordinator end [--reason TEXT] [--id COORDINATOR_ID]
      End this pane's tenure, or the one named.
  herdr coordinator status [--repo DIR]
      List the active tenures, of one repository with --repo, each with
      the TODO item its run works on.
  herdr coordinator handoff --to PANE [--id COORDINATOR_ID]
      Hand this pane's tenure (or the one named) to PANE: it ends
      handed_off and the next tenure, one epoch on, starts there with its
      workers, runs and item.";

pub(super) fn run_coordinator_command(args: &[String]) -> std::io::Result<i32> {
    let method = match parse(args) {
        Ok(Some(method)) => method,
        Ok(None) => {
            println!("{USAGE}");
            return Ok(0);
        }
        Err(message) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    let id = format!("cli:coordinator:{}", args[0]);
    super::print_response(&super::send_request(&Request { id, method })?)
}

fn absolute(dir: Option<String>) -> Result<Option<String>, String> {
    dir.map(|dir| {
        std::path::absolute(&dir)
            .map(|dir| dir.display().to_string())
            .map_err(|error| format!("--repo {dir}: {error}"))
    })
    .transpose()
}

/// `Ok(None)` asks for help.
fn parse(args: &[String]) -> Result<Option<Method>, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(String::new());
    };
    let rest = &args[1..];
    Ok(Some(match subcommand {
        "start" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            if !rest.is_empty() {
                return Err("start takes only --repo".into());
            }
            let pane_id = super::target::caller_pane_id()
                .ok_or("coordinator start runs in a herdr pane: HERDR_PANE_ID is not set")?;
            Method::CoordinatorStart(CoordinatorStartParams {
                repo: absolute(repo)?,
                pane_id: Some(pane_id),
            })
        }
        "end" => {
            let (reason, rest) = take_string_option(rest, "--reason")?;
            let (coordinator_id, rest) = take_string_option(&rest, "--id")?;
            if !rest.is_empty() {
                return Err("end takes only --reason and --id".into());
            }
            let pane_id = super::target::caller_pane_id();
            if coordinator_id.is_none() && pane_id.is_none() {
                return Err("end needs --id outside a herdr pane".into());
            }
            Method::CoordinatorEnd(CoordinatorEndParams {
                reason,
                coordinator_id,
                pane_id,
            })
        }
        "handoff" => {
            let (to_pane_id, rest) = take_string_option(rest, "--to")?;
            let (coordinator_id, rest) = take_string_option(&rest, "--id")?;
            if !rest.is_empty() {
                return Err("handoff takes only --to and --id".into());
            }
            let to_pane_id = to_pane_id.ok_or("handoff needs --to PANE")?;
            let pane_id = super::target::caller_pane_id();
            if coordinator_id.is_none() && pane_id.is_none() {
                return Err("handoff needs --id outside a herdr pane".into());
            }
            Method::CoordinatorHandoff(CoordinatorHandoffParams {
                to_pane_id,
                coordinator_id,
                pane_id,
            })
        }
        "status" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            if !rest.is_empty() {
                return Err("status takes only --repo".into());
            }
            Method::CoordinatorStatus(CoordinatorStatusParams {
                repo: absolute(repo)?,
            })
        }
        "help" | "--help" | "-h" => return Ok(None),
        other => return Err(format!("unknown coordinator command: {other}")),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn end_and_status_parse_their_options() {
        let Ok(Some(Method::CoordinatorEnd(params))) =
            parse(&args(&["end", "--reason", "done", "--id", "c-abcd2345"]))
        else {
            panic!("end did not parse");
        };
        assert_eq!(params.reason.as_deref(), Some("done"));
        assert_eq!(params.coordinator_id.as_deref(), Some("c-abcd2345"));
        let Ok(Some(Method::CoordinatorStatus(params))) = parse(&args(&["status"])) else {
            panic!("status did not parse");
        };
        assert_eq!(params.repo, None);
        assert!(parse(&args(&["status", "extra"])).is_err());
        let Ok(Some(Method::CoordinatorHandoff(params))) =
            parse(&args(&["handoff", "--to", "w1-2", "--id", "c-abcd2345"]))
        else {
            panic!("handoff did not parse");
        };
        assert_eq!(params.to_pane_id, "w1-2");
        assert_eq!(params.coordinator_id.as_deref(), Some("c-abcd2345"));
        assert!(parse(&args(&["handoff", "--id", "c-abcd2345"])).is_err());
        assert!(parse(&args(&["nope"])).is_err());
        assert!(matches!(parse(&args(&["help"])), Ok(None)));
    }
}
