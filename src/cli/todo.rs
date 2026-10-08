//! `herdr todo`: drives a TODO item from preflight to a cherry-pick onto
//! `master` (`todo.run`), and lets the coordinator wait on and answer the
//! run's events.

use crate::api::schema::{
    Method, Request, TodoAction, TodoResumeParams, TodoRunParams, TodoRunTarget, TodoRunsParams,
    TodoWaitParams, WorkerDecision,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr todo run <item-id> --task FILE --message SUBJECT --paths GLOB... --check NAME
      Preflight (the item in TODO.md, the folder slot free and clean, the disk
      above the guard threshold, SUBJECT a lowercase conventional subject, the
      paths relative git globs, NAME registered in .herdr/checks.toml), then
      start a headless worker in the folder slot ../herdr-worktrees/worker on
      the branch todo/<item-id>-<attempt> from master with FILE's text as its
      task, owned by this pane. Prints the run (its id r-...). The run then
      waits for you on its events (todo wait).
  herdr todo wait <run-id> [--after EVENT_ID]
      Blocks until the run waits on an event after EVENT_ID (a question the
      worker policy left, the worker's turn end, a failed verify), or ended
      (done or blocked). Prints the event with its event_id, the actions it
      takes and its evidence (questions, diff stat, commits, the verify).
  herdr todo resume <run-id> --event EVENT_ID --action approve|retry|answer
                    [--task FILE] [--request REQUEST_ID] [--message TEXT]
                    [allow|deny|<choice>...]
      Answers the pending event; any other EVENT_ID is refused as stale.
      approve stops the worker, verifies its commit with the check and
      cherry-picks it onto master; retry starts the next attempt with FILE's
      text (at most 3 attempts, then the run is blocked); answer sends the
      worker allow, deny or one choice per question.
  herdr todo status <run-id>
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
    let text = std::fs::read_to_string(path).map_err(|error| format!("--task {path}: {error}"))?;
    if text.trim().is_empty() {
        return Err(format!("--task {path} is empty"));
    }
    Ok(text)
}

/// Takes `--paths GLOB...` (every argument up to the next option) out of
/// `args`.
fn take_paths(args: &[String]) -> Result<(Option<Vec<String>>, Vec<String>), String> {
    let Some(at) = args.iter().position(|arg| arg == "--paths") else {
        return Ok((None, args.to_vec()));
    };
    let globs: Vec<String> = args[at + 1..]
        .iter()
        .take_while(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    if globs.is_empty() {
        return Err("missing value for --paths".into());
    }
    let mut rest = args[..at].to_vec();
    rest.extend_from_slice(&args[at + 1 + globs.len()..]);
    if rest.iter().any(|arg| arg == "--paths") {
        return Err("--paths given twice".into());
    }
    Ok((Some(globs), rest))
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
            let (paths, rest) = take_paths(rest)?;
            let (task, rest) = take_string_option(&rest, "--task")?;
            let (message, rest) = take_string_option(&rest, "--message")?;
            let (check, rest) = take_string_option(&rest, "--check")?;
            let item = match rest.as_slice() {
                [item] if !item.starts_with("--") => item.clone(),
                _ => {
                    return Err(
                        "run takes one item id, --task, --message, --paths and --check".into(),
                    )
                }
            };
            let (Some(task), Some(message), Some(paths), Some(check)) =
                (task, message, paths, check)
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
                check,
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
            let event = event
                .ok_or("resume takes --event EVENT_ID, the event it answers")?
                .parse::<i64>()
                .map_err(|_| "--event takes an event id".to_owned())?;
            let action = match action.as_deref() {
                Some("approve") => TodoAction::Approve,
                Some("retry") => TodoAction::Retry,
                Some("answer") => TodoAction::Answer,
                _ => return Err("resume takes --action approve, retry or answer".into()),
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
        ])) else {
            panic!("run did not parse");
        };
        assert_eq!(params.item, "t-abcd2345");
        assert_eq!(params.paths, ["src/**", "AGENTS.md"]);
        assert_eq!(params.task, "Do the thing\n");
        assert_eq!(params.check, "workers");
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
