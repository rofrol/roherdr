//! `herdr todo`: drives a TODO item from preflight through the cherry-pick
//! onto `master`, the install, the TODO update, the push and the cleanup
//! (`todo.run`), and lets the coordinator wait on and answer the run's
//! events.

use crate::api::schema::{
    Method, Request, TodoAction, TodoResumeParams, TodoRunParams, TodoRunTarget, TodoRunsParams,
    TodoWaitParams, WorkerDecision,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr todo run <item-id> --task FILE --message SUBJECT --paths GLOB... --check NAME...
      Preflight (the item in TODO.md, the folder slot free and clean, the disk
      above the guard threshold, SUBJECT a lowercase conventional subject, the
      paths relative git globs, each NAME registered in .herdr/checks.toml;
      the verify runs them in order and every one must pass), then
      start a headless worker in the folder slot ../herdr-worktrees/worker on
      the branch todo/<item-id>-<attempt> from master with FILE's text as its
      task (followed by SUBJECT, the GLOBs and the WORKER-DONE line it must
      keep), owned by this pane. Prints the run (its id r-...). The run then
      waits for you on its events (todo wait).
  herdr todo wait <run-id> [--after EVENT_ID]
      Blocks until the run waits on an event after EVENT_ID (a question the
      worker policy left, the worker's turn end, a failed verify), or ended
      (done or blocked), or a still_alive event todo status raised. Prints the
      event with its event_id, the actions it takes and its evidence
      (questions, diff stat, commits, the verify).
  herdr todo resume <run-id> --event EVENT_ID
                    --action approve|retry|answer|verify|force-stop|
                             retry-install|skip-install|retry-todo|skip-todo|
                             retry-push|abort
                    [--task FILE] [--note FILE | --close FILE]
                    [--request REQUEST_ID] [--message TEXT]
                    [allow|deny|<choice>...]
      Answers the pending event; any other EVENT_ID is refused as stale.
      approve stops the worker, verifies its commit with the checks,
      cherry-picks it onto master, runs the [install] of .herdr/checks.toml,
      appends --note FILE's lines to the item in TODO.md (or, with --close
      FILE, removes the item and adds FILE as a section of DECISIONS.md)
      with scripts/todo_edit.py and commits that by path, pushes master to
      origin only as a fast-forward, and deletes the run's merged branches;
      retry starts the next attempt with FILE's text (at most 3 attempts,
      then the run is blocked); answer sends the worker allow, deny or one
      choice per question; verify runs the verify again; force-stop SIGKILLs
      a worker still alive after its stop. After install_failed, todo_failed
      or push_failed: retry-install, skip-install, retry-todo, skip-todo,
      retry-push, or abort (the run ends blocked, the commit stays on
      master). Every resume sends this shell's environment again, which the
      checks, the install and the push run with (the server never stores
      it); without it a check is unavailable and an install or push fails.
  herdr todo status <run-id>
      Prints the run. Asked while its worker has not exited since the stop,
      it raises a still_alive event first, which takes force-stop.
  herdr todo runs [--repo DIR]";

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
    let id = format!("cli:todo:{}", args[0]);
    super::print_response(&super::send_request(&Request { id, method })?)
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

/// `Ok(None)` asks for help.
fn parse(args: &[String]) -> Result<Option<Method>, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(String::new());
    };
    let rest = &args[1..];
    Ok(Some(match subcommand {
        "run" => {
            let (paths, rest) = take_list(rest, "--paths")?;
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
                owner_session_id: pane
                    .as_ref()
                    .and(std::env::var("CLAUDE_CODE_SESSION_ID").ok())
                    .filter(|session| !session.trim().is_empty()),
                owner_pane_id: pane,
                env: super::worker::caller_env(),
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
            let (event, rest) = take_string_option(rest, "--event")?;
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
            if action != TodoAction::Approve && (note.is_some() || close.is_some()) {
                return Err("only --action approve takes --note or --close".into());
            }
            if note.is_some() && close.is_some() {
                return Err("--note and --close exclude each other".into());
            }
            let note = note.map(|path| read_text("--note", &path)).transpose()?;
            let close = close.map(|path| read_text("--close", &path)).transpose()?;
            let task = task.map(|path| read_task(&path)).transpose()?;
            if action == TodoAction::Retry && task.is_none() {
                return Err("--action retry needs --task FILE, the next attempt's task".into());
            }
            let (decision, answers) = match answer {
                [] if action == TodoAction::Answer => {
                    return Err("--action answer takes allow, deny or the chosen options".into())
                }
                [word] if word == "allow" => (Some(WorkerDecision::Allow), Vec::new()),
                [word] if word == "deny" => (Some(WorkerDecision::Deny), Vec::new()),
                answers => (None, answers.to_vec()),
            };
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
            })
        }
        "status" => Method::TodoStatus(TodoRunTarget {
            run_id: one_run_id("status", rest)?,
        }),
        "runs" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            if !rest.is_empty() {
                return Err("runs takes only --repo".into());
            }
            let repo = repo
                .map(|dir| {
                    std::path::absolute(&dir)
                        .map(|dir| dir.display().to_string())
                        .map_err(|error| format!("--repo {dir}: {error}"))
                })
                .transpose()?;
            Method::TodoRuns(TodoRunsParams { repo })
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
        assert!(parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "1",
            "--action",
            "retry"
        ]))
        .is_err());
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
}
