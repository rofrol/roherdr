//! `herdr coordinator`: claims, ends and lists coordination tenures (one
//! active coordinator per repository).

use crate::api::schema::{
    CoordinatorAckParams, CoordinatorAcquireParams, CoordinatorEndParams,
    CoordinatorHandoffAbortParams, CoordinatorHandoffParams, CoordinatorHandoffSuccessorParams,
    CoordinatorReadyParams, CoordinatorStartParams, CoordinatorStatusParams, Method, Request,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr coordinator start [--repo DIR] [--token]
      Claim the repository's coordination for this pane ($HERDR_PANE_ID):
      refused (coordinator_active, naming the holder) while another pane or
      process holds it. DIR is a directory in the repository; this pane's
      directory by default. Setting a tab's role to coordinator does the
      same. The repository rooted at the home directory (dotfiles) is
      claimed only explicitly, with --repo ~: a pane's directory there is
      never a claim. When it keeps its git directory outside HOME, this asks
      git in HOME for it (as your shell's git finds it) and the server
      records it. With --token the reply is coordinator.acquire's for this
      pane: the tenure and a new token, which replaces any earlier one;
      herdr worker run needs it while the tenure is active.
  herdr coordinator acquire [--repo DIR] [--pid PID]
      Acquire the repository's tenure for a process outside herdr's panes
      (an orchestrator started from a shell): PID, else the process that ran
      this command (its parent). Prints the tenure and its token, the only
      place the token is shown; herdr keeps its sha256. Refused
      (coordinator_active, naming the holder) while another pane or process
      holds the repository. The same process acquiring again gets the same
      tenure with a new token, which replaces the earlier one. The tenure
      ends when that process exits (herdr watches its exit event; a server
      that starts, also after an update, ends it when the process is gone),
      or with herdr coordinator end --id. DIR is a directory in the
      repository; the current directory by default.
  herdr coordinator end [--reason TEXT] [--id COORDINATOR_ID]
      End this pane's tenure, or the one named.
  herdr coordinator status [--repo DIR]
      List the active tenures, of one repository with --repo, each with
      the TODO item its run works on and its holder (holder_kind pane,
      process with holder_pid, or headless); never a token.
  herdr coordinator handoff --to PANE [--id COORDINATOR_ID]
      Hand this pane's tenure (or the one named) to PANE: it ends
      handed_off and the next tenure, one epoch on, starts there with its
      workers, runs and item.
  herdr coordinator ready --note FILE
      Freeze this pane's tenure for a handoff to a fresh session of the same
      agent: herdr copies FILE (the note for the successor) and records its
      sha256. A frozen tenure refuses handoff, todo resume, worker start,
      answer and ack until the handoff ends. Refused when the agent is not
      a claude or pi session, or is blocked on a question.
  herdr coordinator handoff --successor
      After ready: write herdr's snapshot and the handoff manifest, open a
      pane beside this one and start a fresh session of the same agent kind
      there with one launch prompt that names the manifest. End your turn
      afterwards; if the successor exits or reports a limit before its ack,
      the handoff aborts and this tenure unfreezes.
  herdr coordinator handoff --abort [--id COORDINATOR_ID]
      Cancel a pending handoff from the old pane, the successor's pane, or
      (with --id) from outside a pane: the tenure unfreezes and keeps its
      authority, and the successor's pane is closed (unless the successor
      itself aborts). Use it when the successor never starts.
  herdr coordinator ack --epoch N --snapshot SHA256
      From the successor's pane: take the tenure over (the epoch, workers,
      runs and the tab's coordinator role move here in one step). The old
      pane is then closed, which ends its agent session; when it cannot be
      closed the reply says so and that session stays fenced as superseded.";

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
    let token_for_pane = match &method {
        Method::CoordinatorStart(params) if args.iter().any(|arg| arg == "--token") => {
            params.pane_id.clone()
        }
        _ => None,
    };
    let response = super::send_request(&Request { id, method })?;
    let Some(pane_id) = token_for_pane.filter(|_| response.get("error").is_none()) else {
        return super::print_response(&response);
    };
    super::print_response(&super::send_request(&Request {
        id: "cli:coordinator:acquire".into(),
        method: Method::CoordinatorAcquire(CoordinatorAcquireParams {
            pane_id: Some(pane_id),
            ..CoordinatorAcquireParams::default()
        }),
    })?)
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
            let (repo, mut rest) = take_string_option(rest, "--repo")?;
            rest.retain(|arg| arg != "--token");
            if !rest.is_empty() {
                return Err("start takes only --repo and --token".into());
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
        "acquire" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            let (pid, rest) = take_string_option(&rest, "--pid")?;
            if !rest.is_empty() {
                return Err("acquire takes only --repo and --pid".into());
            }
            let holder_pid = match pid {
                Some(pid) => pid
                    .parse::<u32>()
                    .map_err(|_| format!("--pid must be a process id, not {pid}"))?,
                None => crate::platform::process_parent(std::process::id())
                    .ok_or("acquire cannot find the process that ran it here: pass --pid")?,
            };
            let repo = match repo {
                Some(repo) => repo,
                None => std::env::current_dir()
                    .map_err(|error| format!("the current directory: {error}"))?
                    .display()
                    .to_string(),
            };
            let repo = absolute(Some(repo))?;
            let home_git_dir = repo.as_deref().and_then(home_git_dir);
            Method::CoordinatorAcquire(CoordinatorAcquireParams {
                repo,
                holder_pid: Some(holder_pid),
                pane_id: None,
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
        "ready" => {
            let (note, rest) = take_string_option(rest, "--note")?;
            if !rest.is_empty() {
                return Err("ready takes only --note".into());
            }
            let note = note.ok_or("ready needs --note FILE")?;
            let note_path = std::path::absolute(&note)
                .map_err(|error| format!("--note {note}: {error}"))?
                .display()
                .to_string();
            let pane_id = super::target::caller_pane_id()
                .ok_or("coordinator ready runs in a herdr pane: HERDR_PANE_ID is not set")?;
            Method::CoordinatorReady(CoordinatorReadyParams { pane_id, note_path })
        }
        "ack" => {
            let (epoch, rest) = take_string_option(rest, "--epoch")?;
            let (snapshot, rest) = take_string_option(&rest, "--snapshot")?;
            if !rest.is_empty() {
                return Err("ack takes only --epoch and --snapshot".into());
            }
            let epoch = epoch
                .ok_or("ack needs --epoch N")?
                .parse::<i64>()
                .map_err(|_| "--epoch must be a number")?;
            let snapshot_sha256 = snapshot.ok_or("ack needs --snapshot SHA256")?;
            let pane_id = super::target::caller_pane_id()
                .ok_or("coordinator ack runs in a herdr pane: HERDR_PANE_ID is not set")?;
            Method::CoordinatorAck(CoordinatorAckParams {
                pane_id,
                epoch,
                snapshot_sha256,
            })
        }
        "handoff" if rest.first().map(String::as_str) == Some("--abort") => {
            let (coordinator_id, extra) = take_string_option(&rest[1..], "--id")?;
            if !extra.is_empty() {
                return Err("handoff --abort takes only --id".into());
            }
            let pane_id = super::target::caller_pane_id();
            if coordinator_id.is_none() && pane_id.is_none() {
                return Err("handoff --abort needs --id outside a herdr pane".into());
            }
            Method::CoordinatorHandoffAbort(CoordinatorHandoffAbortParams {
                pane_id,
                coordinator_id,
            })
        }
        "handoff" if rest == ["--successor"] => {
            let pane_id = super::target::caller_pane_id().ok_or(
                "coordinator handoff --successor runs in a herdr pane: HERDR_PANE_ID is not set",
            )?;
            Method::CoordinatorHandoffSuccessor(CoordinatorHandoffSuccessorParams { pane_id })
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
        let Ok(Some(Method::CoordinatorHandoffAbort(params))) =
            parse(&args(&["handoff", "--abort", "--id", "c-abcd2345"]))
        else {
            panic!("handoff --abort did not parse");
        };
        assert_eq!(params.coordinator_id.as_deref(), Some("c-abcd2345"));
        assert!(parse(&args(&["handoff", "--abort", "--to", "w1-2"])).is_err());
        assert!(parse(&args(&["nope"])).is_err());
        assert!(parse(&args(&["ack", "--epoch", "x", "--snapshot", "ab"])).is_err());
        assert!(parse(&args(&["ready"])).is_err());
        assert!(matches!(parse(&args(&["help"])), Ok(None)));
    }

    #[test]
    fn acquire_names_its_holder_process_and_repository() {
        let Ok(Some(Method::CoordinatorAcquire(params))) =
            parse(&args(&["acquire", "--repo", "/repo", "--pid", "4242"]))
        else {
            panic!("acquire did not parse");
        };
        assert_eq!(params.holder_pid, Some(4242));
        assert_eq!(params.repo.as_deref(), Some("/repo"));
        assert_eq!(params.pane_id, None);
        // Without --pid the holder is the process that ran the command,
        // where the platform can name it.
        match (
            parse(&args(&["acquire", "--repo", "/repo"])),
            crate::platform::process_parent(std::process::id()),
        ) {
            (Ok(Some(Method::CoordinatorAcquire(params))), Some(parent)) => {
                assert_eq!(params.holder_pid, Some(parent));
            }
            (Err(message), None) => assert!(message.contains("--pid"), "{message}"),
            _ => panic!("acquire without --pid parsed against the platform's parent"),
        }
        assert!(parse(&args(&["acquire", "--pid", "x"])).is_err());
        assert!(parse(&args(&["acquire", "--extra"])).is_err());
    }
}
