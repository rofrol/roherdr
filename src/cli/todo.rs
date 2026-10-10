//! `herdr todo`: drives a TODO item from preflight through the cherry-pick
//! onto `master`, the install, the TODO update, the push and the cleanup
//! (`todo.run`), and lets the coordinator wait on and answer the run's
//! events.

use std::time::Duration;

use crate::api::client::{ApiClient, ApiClientError};
use crate::api::schema::{
    Method, Request, TodoAction, TodoNextParams, TodoResumeParams, TodoReviewParams, TodoRunParams,
    TodoRunTarget, TodoRunsParams, TodoStopParams, TodoWaitParams, WorkerDecision,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr todo run <item-id> --task FILE --message SUBJECT --paths GLOB... --check NAME...
                 [--ignore-usage]
      Preflight (the item in TODO.md, the folder slot free and clean, the disk
      above the guard threshold, SUBJECT a lowercase conventional subject, the
      paths relative git globs, each NAME registered in .herdr/checks.toml;
      the verify runs them in order and every one must pass; the usage gate:
      Claude's usage read from the provider now; refused as usage_gate while
      any Claude window is 90% or more used or the read fails, and after such
      a refusal until a read shows every Claude window below 80%;
      --ignore-usage, on the user's word, starts anyway), then
      start a headless worker in the folder slot ../herdr-worktrees/worker on
      the branch todo/<item-id>-<attempt> from master with FILE's text as its
      task (followed by SUBJECT, the GLOBs and the WORKER-DONE line it must
      keep), owned by this pane. Prints the run (its id r-...). The run then
      waits for you on its events (todo wait).
  herdr todo wait <run-id> [--after EVENT_ID]
      Blocks until the run waits on an event after EVENT_ID (a question the
      worker policy left, the worker's turn end, a failed verify), or ended
      (done, blocked or aborted), or a still_alive event todo status raised.
      Prints the event with its event_id, the actions it takes and its
      evidence (questions, diff stat, commits, the verify). When the server
      goes away meanwhile (a live handoff), it waits for the server socket to
      accept again and goes on waiting on the same run after EVENT_ID; it
      gives up only when the new server does not know the run.
  herdr todo resume <run-id> --event EVENT_ID
                    --action approve|retry|answer|verify|force-stop|
                             retry-install|skip-install|retry-todo|skip-todo|
                             retry-push|abort
                    [--task FILE [--ignore-usage]] [--note FILE | --close FILE]
                    [--request REQUEST_ID] [--message TEXT]
                    [allow|deny|<choice>...]
      Answers the pending event; any other EVENT_ID is refused as stale.
      approve stops the worker, verifies its commit with the checks,
      cherry-picks it onto master with the trailers Herdr-Item: <item-id> and
      Herdr-Run: <run-id>/<attempt> (only when the branch is still at the
      commit the event showed; otherwise a new review event), runs the
      [install] of .herdr/checks.toml,
      appends --note FILE's lines to the item in TODO.md (or, with --close
      FILE, removes the item and adds FILE as a section of DECISIONS.md)
      with scripts/todo_edit.py and commits that by path, pushes master to
      origin only as a fast-forward, and deletes the run's merged branches;
      retry starts the next attempt with FILE's text as the review of this
      one: its branch starts from this attempt's commit (cherry-picked onto
      the base) and its task is this attempt's with the review appended (at
      most 3 attempts, then the run is blocked; the usage gate of todo run
      applies, and its refusal leaves the run waiting on the same event); a
      commit that does not cherry-pick is a retry_conflict event, whose retry
      (no --task) starts that attempt from the base; answer sends the worker allow, deny or one
      choice per question; verify runs the verify again; force-stop SIGKILLs
      a worker still alive after its stop. After install_failed, todo_failed
      or push_failed: retry-install or skip-install, retry-todo or skip-todo,
      retry-push. abort answers any event the run waits on, or the blocked
      event of a blocked run: the run stops its worker when it still runs,
      waits for its exit and ends aborted, with --message TEXT as the reason;
      its branches and commits (a commit already on master too) stay as they
      are and the aborted event names them. Every resume sends this shell's environment again, which the
      checks, the install and the push run with (the server never stores
      it); without it a check is unavailable and an install or push fails.
  herdr todo status <run-id>
      Prints the run. Asked while its worker has not exited since the stop,
      it raises a still_alive event first, which takes force-stop.
  herdr todo review <run-id> [--attempt N] [--diff] [--json]
      Prints in one view what you review of the run's attempt N (default:
      its current one): the task the worker got with the contract appended,
      its last reply, its questions with their answers, the tool calls that
      failed or were denied, git diff --stat base commit (with --diff the
      full diff too), the verify's checks with their outcome and evidence,
      your decision, and the (commit, base) an approval is bound to. --json
      prints the server's reply.
  herdr todo runs [--repo DIR] [--commit SHA]
      The runs, of DIR's repository with --repo. --commit SHA prints only the
      run that landed SHA (the landed commit or the worker's) and that
      landing, found in the worker store or by the commit's Herdr-Run
      trailer in the history of DIR (default: the current directory).
  herdr todo next [--repo DIR] [--continue]
      Start a fresh headless item coordinator for the top item of the
      \"Next, in order\" section of DIR's (default: the current directory's)
      TODO.md: a Claude worker in the repository, owned by this pane, under
      a headless coordination tenure of its own and herdr's coordinator
      allowlist. Its prompt, built by herdr, names the item, what to read
      and the driver commands; it writes the worker's task, runs the item
      with todo run, reviews and answers each event, closes the item and
      ends with COORDINATOR-DONE, -ESCALATED or -BLOCKED. Its questions go to
      the user's list. Herdr records its outcome in the item's history
      (herdr history --item). Refused while the repository has a run in
      progress or an active coordinator, when the section has no open item,
      and when the usage gate of todo run refuses. With --continue, each
      coordinator whose item is done starts the next one on its exit, until
      the section is empty, an item escalates, is blocked or fails, or todo
      stop. The checks, install and push of its runs use this shell's
      environment, kept in the server's memory only.
  herdr todo stop [--repo DIR]
      Stop the repository's chain: the coordinator that runs finishes its
      item, and no next one starts.";

pub(super) fn run_todo_command(args: &[String]) -> std::io::Result<i32> {
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
    let request = Request {
        id: format!("cli:todo:{}", args[0]),
        method,
    };
    if let Method::TodoReview(_) = &request.method {
        let json = args.iter().any(|arg| arg == "--json");
        return super::todo_review::print_review(&super::send_request(&request)?, json);
    }
    if matches!(request.method, Method::TodoWait(_)) && !super::target::is_remote() {
        return wait_across_handoffs(&request);
    }
    super::print_response(&super::send_request(&request)?)
}

/// How often a `todo wait` that lost its server tries to connect again.
/// External polling: nothing notifies a client when the new server of a
/// live handoff binds the socket. No deadline: only the connection that
/// the socket accepts ends the wait for it.
const RECONNECT_POLL: Duration = Duration::from_millis(50);

/// `todo wait`, which a live handoff does not end: the old server closes
/// the connection without an answer, and the wait goes on with the new one.
fn wait_across_handoffs(request: &Request) -> std::io::Result<i32> {
    let client = super::target::api_client()?;
    super::ensure_server_protocol_compatible(&client, &request.id)?;
    let response = request_reconnecting(&client, request, || {
        super::ensure_server_protocol_compatible(&client, &request.id)
    })?;
    super::print_response(&response)
}

/// Whether `error` means the server went away with the request in flight
/// (it closed the connection without an answer), or, once it did, that its
/// replacement does not accept yet.
fn connection_lost(error: &ApiClientError, lost_before: bool) -> bool {
    use std::io::ErrorKind;
    match error {
        ApiClientError::EmptyResponse => true,
        ApiClientError::Io(error) => match error.kind() {
            ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof => true,
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => lost_before,
            _ => false,
        },
        _ => false,
    }
}

/// Blocks until the socket accepts a connection.
fn wait_until_accepting(client: &ApiClient) -> std::io::Result<()> {
    loop {
        match crate::ipc::connect_local_stream(&client.socket_path()) {
            Ok(_) => return Ok(()),
            Err(error) if super::server_not_running_error(&error) => {
                std::thread::sleep(RECONNECT_POLL);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Sends `request` and returns the server's answer, across lost
/// connections ([`send_reconnecting`]).
fn request_reconnecting(
    client: &ApiClient,
    request: &Request,
    reconnected: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<serde_json::Value> {
    send_reconnecting(
        || client.request_value(request),
        || wait_until_accepting(client),
        reconnected,
    )?
    .map_err(|error| super::map_server_not_running_or_io(error, &request.id, client))
}

/// Sends a request with `send` and returns the server's answer. When the
/// connection is lost before the answer, it waits until the socket accepts
/// again (`accepting`), runs `reconnected` (the protocol check of the new
/// server) and sends the same request again: `todo.wait` only reads, so
/// sending it twice is safe. Any answer ends it, an error answer too
/// (`todo_run_not_found` from a server that does not know the run); the
/// inner error is a failure that is not a lost connection.
fn send_reconnecting(
    mut send: impl FnMut() -> Result<serde_json::Value, ApiClientError>,
    mut accepting: impl FnMut() -> std::io::Result<()>,
    mut reconnected: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<Result<serde_json::Value, ApiClientError>> {
    let mut lost = false;
    loop {
        if lost {
            accepting()?;
            match reconnected() {
                Ok(()) => {}
                // It went away again before it answered the check.
                Err(error) if super::server_not_running_was_reported(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        match send() {
            Ok(response) => return Ok(Ok(response)),
            Err(error) if connection_lost(&error, lost) => {
                if !lost {
                    eprintln!(
                        "herdr todo wait: lost the server connection ({error}); waiting for the \
                         server to accept again to go on waiting on the same run"
                    );
                }
                lost = true;
            }
            Err(error) => return Ok(Err(error)),
        }
    }
}

fn read_task(path: &str) -> Result<String, String> {
    read_text("--task", path)
}

fn read_text(option: &str, path: &str) -> Result<String, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{option} {path}: {error}"))?;
    if text.trim().is_empty() {
        return Err(format!("{option} {path} is empty"));
    }
    Ok(text)
}

/// Takes `option VALUE...` (every argument up to the next option, such as
/// `--paths GLOB...` or `--check NAME...`) out of `args`.
fn take_list(args: &[String], option: &str) -> Result<(Option<Vec<String>>, Vec<String>), String> {
    let Some(at) = args.iter().position(|arg| arg == option) else {
        return Ok((None, args.to_vec()));
    };
    let values: Vec<String> = args[at + 1..]
        .iter()
        .take_while(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    if values.is_empty() {
        return Err(format!("missing value for {option}"));
    }
    let mut rest = args[..at].to_vec();
    rest.extend_from_slice(&args[at + 1 + values.len()..]);
    if rest.iter().any(|arg| arg == option) {
        return Err(format!("{option} given twice"));
    }
    Ok((Some(values), rest))
}

fn one_run_id(subcommand: &str, rest: &[String]) -> Result<String, String> {
    match rest {
        [run_id] if !run_id.starts_with("--") => Ok(run_id.clone()),
        _ => Err(format!("{subcommand} takes one run id")),
    }
}

/// Takes the switch `option` out of `args`: whether it was there.
fn take_switch(args: &[String], option: &str) -> Result<(bool, Vec<String>), String> {
    let rest: Vec<String> = args.iter().filter(|arg| *arg != option).cloned().collect();
    match args.len() - rest.len() {
        0 => Ok((false, rest)),
        1 => Ok((true, rest)),
        _ => Err(format!("{option} given twice")),
    }
}

/// The caller's agent session, sent only with its pane, as `worker start`
/// does.
fn caller_session(pane: Option<&str>) -> Option<String> {
    pane.and(std::env::var("CLAUDE_CODE_SESSION_ID").ok())
        .filter(|session| !session.trim().is_empty())
}

/// `Ok(None)` asks for help.
fn parse(args: &[String]) -> Result<Option<Method>, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(String::new());
    };
    let rest = &args[1..];
    Ok(Some(match subcommand {
        "run" => {
            let (ignore_usage, rest) = take_switch(rest, "--ignore-usage")?;
            let (paths, rest) = take_list(&rest, "--paths")?;
            let (checks, rest) = take_list(&rest, "--check")?;
            let (task, rest) = take_string_option(&rest, "--task")?;
            let (message, rest) = take_string_option(&rest, "--message")?;
            let item = match rest.as_slice() {
                [item] if !item.starts_with("--") => item.clone(),
                _ => {
                    return Err(
                        "run takes one item id, --task, --message, --paths and --check".into(),
                    )
                }
            };
            let (Some(task), Some(message), Some(paths), Some(checks)) =
                (task, message, paths, checks)
            else {
                return Err("run takes one item id, --task, --message, --paths and --check".into());
            };
            let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
            let pane = super::target::caller_pane_id();
            Method::TodoRun(TodoRunParams {
                cwd: std::path::absolute(&cwd)
                    .map_err(|error| error.to_string())?
                    .display()
                    .to_string(),
                item,
                task: read_task(&task)?,
                message,
                paths,
                checks,
                owner_session_id: caller_session(pane.as_deref()),
                owner_pane_id: pane,
                workspace_id: super::target::caller_workspace_id(),
                env: super::worker::caller_env(),
                ignore_usage,
            })
        }
        "wait" => {
            let (after, rest) = take_string_option(rest, "--after")?;
            let after = after
                .map(|value| {
                    value
                        .parse::<i64>()
                        .map_err(|_| format!("--after takes an event id, not {value}"))
                })
                .transpose()?;
            Method::TodoWait(TodoWaitParams {
                run_id: one_run_id("wait", &rest)?,
                after,
            })
        }
        "resume" => {
            let (ignore_usage, rest) = take_switch(rest, "--ignore-usage")?;
            let (event, rest) = take_string_option(&rest, "--event")?;
            let (action, rest) = take_string_option(&rest, "--action")?;
            let (task, rest) = take_string_option(&rest, "--task")?;
            let (request_id, rest) = take_string_option(&rest, "--request")?;
            let (message, rest) = take_string_option(&rest, "--message")?;
            let (note, rest) = take_string_option(&rest, "--note")?;
            let (close, rest) = take_string_option(&rest, "--close")?;
            let event = event
                .ok_or("resume takes --event EVENT_ID, the event it answers")?
                .parse::<i64>()
                .map_err(|_| "--event takes an event id".to_owned())?;
            let action = match action.as_deref() {
                Some("approve") => TodoAction::Approve,
                Some("retry") => TodoAction::Retry,
                Some("answer") => TodoAction::Answer,
                Some("verify") => TodoAction::Verify,
                Some("force-stop") => TodoAction::ForceStop,
                Some("retry-install") => TodoAction::RetryInstall,
                Some("skip-install") => TodoAction::SkipInstall,
                Some("retry-todo") => TodoAction::RetryTodo,
                Some("skip-todo") => TodoAction::SkipTodo,
                Some("retry-push") => TodoAction::RetryPush,
                Some("abort") => TodoAction::Abort,
                _ => {
                    return Err("resume takes --action approve, retry, answer, verify, \
                         force-stop, retry-install, skip-install, retry-todo, skip-todo, \
                         retry-push or abort"
                        .into())
                }
            };
            let Some((run_id, answer)) = rest.split_first() else {
                return Err("resume takes a run id".into());
            };
            if action != TodoAction::Answer && (!answer.is_empty() || request_id.is_some()) {
                return Err("only --action answer takes an answer and --request".into());
            }
            if action != TodoAction::Retry && task.is_some() {
                return Err("only --action retry takes --task".into());
            }
            if action != TodoAction::Retry && ignore_usage {
                return Err("only --action retry takes --ignore-usage".into());
            }
            if action != TodoAction::Approve && (note.is_some() || close.is_some()) {
                return Err("only --action approve takes --note or --close".into());
            }
            if note.is_some() && close.is_some() {
                return Err("--note and --close exclude each other".into());
            }
            let note = note.map(|path| read_text("--note", &path)).transpose()?;
            let close = close.map(|path| read_text("--close", &path)).transpose()?;
            let task = task.map(|path| read_task(&path)).transpose()?;
            let (decision, answers) = match answer {
                [] if action == TodoAction::Answer => {
                    return Err("--action answer takes allow, deny or the chosen options".into())
                }
                [word] if word == "allow" => (Some(WorkerDecision::Allow), Vec::new()),
                [word] if word == "deny" => (Some(WorkerDecision::Deny), Vec::new()),
                answers => (None, answers.to_vec()),
            };
            let pane = super::target::caller_pane_id();
            Method::TodoResume(TodoResumeParams {
                run_id: run_id.clone(),
                action,
                event,
                task,
                request_id,
                decision,
                answers,
                message,
                env: super::worker::caller_env(),
                note,
                close,
                caller_session_id: caller_session(pane.as_deref()),
                caller_pane_id: pane,
                caller_workspace_id: super::target::caller_workspace_id(),
                ignore_usage,
            })
        }
        "status" => Method::TodoStatus(TodoRunTarget {
            run_id: one_run_id("status", rest)?,
        }),
        "review" => {
            let (_, rest) = take_switch(rest, "--json")?;
            let (diff, rest) = take_switch(&rest, "--diff")?;
            let (attempt, rest) = take_string_option(&rest, "--attempt")?;
            let attempt = attempt
                .map(|value| {
                    value
                        .parse::<u32>()
                        .ok()
                        .filter(|attempt| *attempt > 0)
                        .ok_or_else(|| format!("--attempt takes an attempt number, not {value}"))
                })
                .transpose()?;
            Method::TodoReview(TodoReviewParams {
                run_id: one_run_id("review", &rest)?,
                attempt,
                diff,
            })
        }
        "next" | "stop" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            let (chain, rest) = take_switch(&rest, "--continue")?;
            if !rest.is_empty() || (subcommand == "stop" && chain) {
                return Err(format!(
                    "{subcommand} takes only --repo{}",
                    if subcommand == "next" {
                        " and --continue"
                    } else {
                        ""
                    }
                ));
            }
            let dir = match repo {
                Some(dir) => std::path::PathBuf::from(dir),
                None => std::env::current_dir().map_err(|error| error.to_string())?,
            };
            let cwd = std::path::absolute(&dir)
                .map_err(|error| format!("--repo {}: {error}", dir.display()))?
                .display()
                .to_string();
            if subcommand == "stop" {
                Method::TodoStop(TodoStopParams { cwd })
            } else {
                let pane = super::target::caller_pane_id();
                Method::TodoNext(TodoNextParams {
                    cwd,
                    chain,
                    owner_session_id: caller_session(pane.as_deref()),
                    owner_pane_id: pane,
                    workspace_id: super::target::caller_workspace_id(),
                    env: super::worker::caller_env(),
                })
            }
        }
        "runs" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            let (commit, rest) = take_string_option(&rest, "--commit")?;
            if !rest.is_empty() {
                return Err("runs takes only --repo and --commit".into());
            }
            // A commit's trailers are read in the current directory's
            // repository unless --repo names another.
            let repo = match (repo, &commit) {
                (None, Some(_)) => Some(".".to_owned()),
                (repo, _) => repo,
            };
            let repo = repo
                .map(|dir| {
                    std::path::absolute(&dir)
                        .map(|dir| dir.display().to_string())
                        .map_err(|error| format!("--repo {dir}: {error}"))
                })
                .transpose()?;
            Method::TodoRuns(TodoRunsParams { repo, commit })
        }
        "help" | "--help" | "-h" => return Ok(None),
        other => return Err(format!("unknown todo command: {other}")),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn run_takes_the_paths_up_to_the_next_option_and_reads_the_task() {
        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let task = dir.join("task.md");
        std::fs::write(&task, "Do the thing\n").unwrap();
        let task = task.display().to_string();
        let Ok(Some(Method::TodoRun(params))) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--paths",
            "src/**",
            "AGENTS.md",
            "--task",
            &task,
            "--message",
            "feat: x",
            "--check",
            "workers",
            "windows-lint",
        ])) else {
            panic!("run did not parse");
        };
        assert_eq!(params.item, "t-abcd2345");
        assert_eq!(params.paths, ["src/**", "AGENTS.md"]);
        assert_eq!(params.task, "Do the thing\n");
        assert_eq!(params.checks, ["workers", "windows-lint"]);
        assert!(!params.ignore_usage);
        let Ok(Some(Method::TodoRun(params))) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--ignore-usage",
            "--task",
            &task,
            "--message",
            "feat: x",
            "--paths",
            "src/**",
            "--check",
            "workers",
        ])) else {
            panic!("run --ignore-usage did not parse");
        };
        assert!(params.ignore_usage);
        assert_eq!(params.paths, ["src/**"]);
        assert!(parse(&args(&["run", "t-abcd2345", "--task", &task])).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resume_needs_the_event_and_an_action_with_its_arguments() {
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "42",
            "--action",
            "answer",
            "--request",
            "perm-1",
            "allow",
        ])) else {
            panic!("resume did not parse");
        };
        assert_eq!(params.event, 42);
        assert_eq!(params.action, TodoAction::Answer);
        assert_eq!(params.decision, Some(WorkerDecision::Allow));
        assert_eq!(params.request_id.as_deref(), Some("perm-1"));
        // Every resume sends the caller's environment again.
        assert!(params
            .env
            .is_some_and(|env| env.keys().all(|key| !key.starts_with("HERDR_"))));
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "9",
            "--action",
            "force-stop",
        ])) else {
            panic!("force-stop did not parse");
        };
        assert_eq!(params.action, TodoAction::ForceStop);
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "9",
            "--action",
            "retry-install",
        ])) else {
            panic!("retry-install did not parse");
        };
        assert_eq!(params.action, TodoAction::RetryInstall);
        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-note-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let note = dir.join("note.md");
        std::fs::write(&note, "Done by w1.\n").unwrap();
        let note = note.display().to_string();
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "9",
            "--action",
            "approve",
            "--note",
            &note,
        ])) else {
            panic!("approve --note did not parse");
        };
        assert_eq!(params.note.as_deref(), Some("Done by w1.\n"));
        assert_eq!(params.close, None);
        for bad in [
            &["--action", "approve", "--note", &note, "--close", &note][..],
            &["--action", "abort", "--note", &note][..],
        ] {
            let mut words = vec!["resume", "r-abcd2345", "--event", "9"];
            words.extend_from_slice(bad);
            assert!(parse(&args(&words)).is_err(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
        assert!(parse(&args(&["resume", "r-abcd2345", "--action", "approve"])).is_err());
        // A retry of a retry_conflict event takes no task; the server
        // refuses one without a task for any other event.
        let Ok(Some(Method::TodoResume(retry))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "1",
            "--action",
            "retry",
        ])) else {
            panic!("retry without --task did not parse");
        };
        assert_eq!((retry.action, retry.task), (TodoAction::Retry, None));
        let Ok(Some(Method::TodoRuns(runs))) =
            parse(&args(&["runs", "--commit", "abc1234", "--repo", "/repo"]))
        else {
            panic!("runs --commit did not parse");
        };
        assert_eq!(
            (runs.repo.as_deref(), runs.commit.as_deref()),
            (Some("/repo"), Some("abc1234"))
        );
        assert!(parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "1",
            "--action",
            "approve",
            "allow"
        ]))
        .is_err());
        let Ok(Some(Method::TodoWait(wait))) =
            parse(&args(&["wait", "r-abcd2345", "--after", "7"]))
        else {
            panic!("wait did not parse");
        };
        assert_eq!(wait.after, Some(7));
        assert!(parse(&args(&["wait"])).is_err());
        assert!(matches!(parse(&args(&["help"])), Ok(None)));
    }

    #[test]
    fn next_and_stop_take_the_repository_and_next_the_chain() {
        let Ok(Some(Method::TodoNext(next))) =
            parse(&args(&["next", "--repo", "/repo", "--continue"]))
        else {
            panic!("next did not parse");
        };
        assert_eq!(next.cwd, "/repo");
        assert!(next.chain);
        assert!(next
            .env
            .is_some_and(|env| env.keys().all(|key| !key.starts_with("HERDR_"))));
        let Ok(Some(Method::TodoNext(next))) = parse(&args(&["next"])) else {
            panic!("next without options did not parse");
        };
        assert!(!next.chain);
        assert!(std::path::Path::new(&next.cwd).is_absolute());
        let Ok(Some(Method::TodoStop(stop))) = parse(&args(&["stop", "--repo", "/repo"])) else {
            panic!("stop did not parse");
        };
        assert_eq!(stop.cwd, "/repo");
        assert!(parse(&args(&["stop", "--continue"])).is_err());
        assert!(parse(&args(&["next", "t-abcd2345"])).is_err());
    }

    #[test]
    fn abort_takes_a_reason() {
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "3",
            "--action",
            "abort",
            "--message",
            "superseded",
        ])) else {
            panic!("abort did not parse");
        };
        assert_eq!(params.action, TodoAction::Abort);
        assert_eq!(params.message.as_deref(), Some("superseded"));
    }

    /// A simulated server replacement: the old server closes the wait's
    /// connection without an answer, the socket is gone for one try, then
    /// the new server answers the same request.
    #[test]
    fn a_lost_connection_is_waited_out_and_the_same_request_sent_again() {
        use std::io::{Error, ErrorKind};
        let mut replies = vec![
            Err(ApiClientError::EmptyResponse),
            Err(ApiClientError::Io(Error::from(ErrorKind::NotFound))),
            Err(ApiClientError::Io(Error::from(ErrorKind::ConnectionReset))),
            Ok(serde_json::json!({"id": "cli:todo:wait", "result": {}})),
        ]
        .into_iter();
        let (mut sent, mut accepted, mut checked) = (0, 0, 0);
        let response = send_reconnecting(
            || {
                sent += 1;
                replies.next().unwrap()
            },
            || {
                accepted += 1;
                Ok(())
            },
            || {
                checked += 1;
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(response["result"], serde_json::json!({}));
        assert_eq!((sent, accepted, checked), (4, 3, 3));

        // An error answer (the new server does not know the run) ends it.
        let unknown = serde_json::json!({"error": {"code": "todo_run_not_found"}});
        let mut replies = vec![Err(ApiClientError::EmptyResponse), Ok(unknown.clone())].into_iter();
        let response = send_reconnecting(|| replies.next().unwrap(), || Ok(()), || Ok(()));
        assert_eq!(response.unwrap().unwrap(), unknown);

        // No server at the first try is not a lost connection: no wait.
        let mut accepted = 0;
        let response = send_reconnecting(
            || Err(ApiClientError::Io(Error::from(ErrorKind::NotFound))),
            || {
                accepted += 1;
                Ok(())
            },
            || Ok(()),
        );
        assert!(matches!(response, Ok(Err(ApiClientError::Io(_)))));
        assert_eq!(accepted, 0);

        // A server that refuses the check (another protocol) ends it.
        let mut replies = vec![Err(ApiClientError::EmptyResponse)].into_iter();
        let response = send_reconnecting(
            || replies.next().unwrap(),
            || Ok(()),
            || Err(Error::other("protocol mismatch")),
        );
        assert!(response.is_err());
    }

    /// Simulated server replacements over a real local socket.
    #[cfg(unix)]
    mod reconnect {
        use super::*;

        fn wait_request() -> Request {
            Request {
                id: "cli:todo:wait".into(),
                method: Method::TodoWait(TodoWaitParams {
                    run_id: "r-abcd2345".into(),
                    after: Some(7),
                }),
            }
        }

        /// Accepts connections on `listener` until one sends a request line,
        /// and returns it with its stream; connections that only probe the
        /// socket send nothing.
        fn next_request(
            listener: &crate::ipc::LocalListener,
        ) -> (
            serde_json::Value,
            std::io::BufReader<crate::ipc::LocalStream>,
        ) {
            use interprocess::local_socket::traits::Listener as _;
            use std::io::BufRead as _;
            loop {
                let mut reader = std::io::BufReader::new(listener.accept().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() > 0 {
                    return (serde_json::from_str(&line).unwrap(), reader);
                }
            }
        }

        fn reply(
            reader: std::io::BufReader<crate::ipc::LocalStream>,
            response: &serde_json::Value,
        ) {
            use std::io::Write as _;
            let mut stream = reader.into_inner();
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
        }

        fn socket(name: &str) -> std::path::PathBuf {
            let path = std::env::temp_dir().join(format!("htw-{name}-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            path
        }

        /// A live handoff: the old server takes the wait and goes away without
        /// an answer (its socket removed first, as `perform_live_handoff`
        /// does), and a new server binds the same path. The wait reconnects,
        /// checks the new server and sends the same wait again.
        #[test]
        fn a_wait_goes_on_with_the_server_that_replaced_the_lost_one() {
            let path = socket("handoff");
            let old = crate::ipc::bind_private_local_listener(&path).unwrap();
            let server_path = path.clone();
            let servers = std::thread::spawn(move || {
                let (first, reader) = next_request(&old);
                std::fs::remove_file(&server_path).unwrap();
                drop(old);
                drop(reader);
                let new = crate::ipc::bind_private_local_listener(&server_path).unwrap();
                let (second, reader) = next_request(&new);
                let answer =
                    serde_json::json!({"id": "cli:todo:wait", "result": {"type": "todo_event"}});
                reply(reader, &answer);
                (first, second, answer)
            });
            let client = ApiClient::for_target(crate::api::client::ConnectionTarget::SocketPath(
                path.clone(),
            ));
            let mut checks = 0;
            let response = request_reconnecting(&client, &wait_request(), || {
                checks += 1;
                Ok(())
            })
            .unwrap();
            let (first, second, answer) = servers.join().unwrap();
            assert_eq!(response, answer);
            assert_eq!(first, second, "the same wait, after the same event");
            assert_eq!(first["method"], "todo.wait");
            assert_eq!(first["params"]["after"], 7);
            assert_eq!(checks, 1, "the new server's protocol is checked once");
            let _ = std::fs::remove_file(path);
        }

        /// The new server does not know the run: its error answer ends the wait.
        #[test]
        fn a_wait_gives_up_when_the_new_server_does_not_know_the_run() {
            let path = socket("unknown");
            let old = crate::ipc::bind_private_local_listener(&path).unwrap();
            let server_path = path.clone();
            let servers = std::thread::spawn(move || {
                let (_, reader) = next_request(&old);
                std::fs::remove_file(&server_path).unwrap();
                drop(old);
                drop(reader);
                let new = crate::ipc::bind_private_local_listener(&server_path).unwrap();
                let (_, reader) = next_request(&new);
                let answer = serde_json::json!({
                    "id": "cli:todo:wait",
                    "error": {"code": "todo_run_not_found", "message": "run r-abcd2345 not found"},
                });
                reply(reader, &answer);
                answer
            });
            let client = ApiClient::for_target(crate::api::client::ConnectionTarget::SocketPath(
                path.clone(),
            ));
            let response = request_reconnecting(&client, &wait_request(), || Ok(())).unwrap();
            assert_eq!(response, servers.join().unwrap());
            let _ = std::fs::remove_file(path);
        }
    }
}
