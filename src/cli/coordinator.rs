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
      Setting a tab's role to coordinator does the same. The repository
      rooted at the home directory (dotfiles) is claimed only explicitly,
      with --repo ~: a pane's directory there is never a claim. When it
      keeps its git directory outside HOME, this asks git in HOME for it
      (as your shell's git finds it) and the server records it.
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

/// Whether `dir` is `home`, both resolved.
fn is_home(dir: &str, home: &std::path::Path) -> bool {
    let real = |path: &std::path::Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    real(std::path::Path::new(dir)) == real(home)
}

/// For `--repo` naming the home directory: the common git directory that
/// git in HOME finds with this shell's environment (a wrapper or alias
/// that pairs a bare dotfiles repository with HOME), which the server
/// cannot find by itself. None elsewhere, or when git finds none there.
fn home_git_dir(dir: &str) -> Option<String> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    if !is_home(dir, std::path::Path::new(&home)) {
        return None;
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&home)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|dir| !dir.is_empty())
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
            let repo = absolute(repo)?;
            let home_git_dir = repo.as_deref().and_then(home_git_dir);
            Method::CoordinatorStart(CoordinatorStartParams {
                repo,
                pane_id: Some(pane_id),
                home_git_dir,
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
