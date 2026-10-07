use crate::api::schema::{
    EmptyParams, Method, Request, WorkerPromptParams, WorkerStartParams, WorkerTarget,
    WorkerWaitParams, WorkerWaitUntil,
};

const USAGE: &str = "usage: herdr worker <start|status|list|wait|prompt|interrupt|stop|kill> ...
  herdr worker start [--cwd DIR] [--model MODEL] <prompt>
  herdr worker status <worker_id>
  herdr worker list
  herdr worker wait <worker_id> [--exit]
  herdr worker prompt <worker_id> <text>
  herdr worker interrupt <worker_id>
  herdr worker stop <worker_id>
  herdr worker kill <worker_id>";

pub(super) fn run_worker_command(args: &[String]) -> std::io::Result<i32> {
    let method = match parse_worker_args(args) {
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
    let id = format!("cli:worker:{}", args[0]);
    super::print_response(&super::send_request(&Request { id, method })?)
}

/// `Ok(None)` asks for help.
fn parse_worker_args(args: &[String]) -> Result<Option<Method>, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(String::new());
    };
    let rest = &args[1..];
    let target = |rest: &[String]| -> Result<WorkerTarget, String> {
        match rest {
            [worker_id] => Ok(WorkerTarget {
                worker_id: worker_id.clone(),
            }),
            _ => Err(format!("{subcommand} takes one worker id")),
        }
    };
    Ok(Some(match subcommand {
        "start" => Method::WorkerStart(parse_start(rest)?),
        "status" => Method::WorkerStatus(target(rest)?),
        "list" if rest.is_empty() => Method::WorkerList(EmptyParams::default()),
        "wait" => {
            let (exit, ids): (Vec<&String>, Vec<&String>) =
                rest.iter().partition(|arg| arg.as_str() == "--exit");
            let [worker_id] = ids.as_slice() else {
                return Err("wait takes one worker id".into());
            };
            Method::WorkerWait(WorkerWaitParams {
                worker_id: (*worker_id).clone(),
                until: Some(if exit.is_empty() {
                    WorkerWaitUntil::TurnEnd
                } else {
                    WorkerWaitUntil::Exit
                }),
            })
        }
        "prompt" => match rest {
            [worker_id, text] => Method::WorkerPrompt(WorkerPromptParams {
                worker_id: worker_id.clone(),
                text: text.clone(),
            }),
            _ => return Err("prompt takes a worker id and one text argument".into()),
        },
        "interrupt" => Method::WorkerInterrupt(target(rest)?),
        "stop" => Method::WorkerStop(target(rest)?),
        "kill" => Method::WorkerKill(target(rest)?),
        "help" | "--help" | "-h" => return Ok(None),
        _ => return Err(format!("unknown worker command: {subcommand}")),
    }))
}

fn parse_start(args: &[String]) -> Result<WorkerStartParams, String> {
    let mut cwd = None;
    let mut model = None;
    let mut prompt = None;
    let mut index = 0;
    while index < args.len() {
        let value = || {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("missing value for {}", args[index]))
        };
        match args[index].as_str() {
            "--cwd" => {
                cwd = Some(value()?);
                index += 2;
            }
            "--model" => {
                model = Some(value()?);
                index += 2;
            }
            other if prompt.is_none() => {
                prompt = Some(other.to_owned());
                index += 1;
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let prompt = prompt.ok_or("start needs a prompt")?;
    let cwd = match cwd {
        Some(cwd) => std::path::PathBuf::from(cwd),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let cwd = std::path::absolute(&cwd).map_err(|error| error.to_string())?;
    Ok(WorkerStartParams {
        cwd: cwd.display().to_string(),
        prompt,
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn parses_start_with_options() {
        let Ok(Some(Method::WorkerStart(params))) = parse_worker_args(&args(&[
            "start",
            "--cwd",
            "/tmp/repo",
            "--model",
            "sonnet",
            "do it",
        ])) else {
            panic!("start must parse");
        };
        assert_eq!(params.cwd, "/tmp/repo");
        assert_eq!(params.model.as_deref(), Some("sonnet"));
        assert_eq!(params.prompt, "do it");
    }

    #[test]
    fn parses_wait_until_exit_and_rejects_extra_ids() {
        assert!(matches!(
            parse_worker_args(&args(&["wait", "w1", "--exit"])),
            Ok(Some(Method::WorkerWait(WorkerWaitParams {
                until: Some(WorkerWaitUntil::Exit),
                ..
            })))
        ));
        assert!(parse_worker_args(&args(&["wait", "w1", "w2"])).is_err());
        assert!(parse_worker_args(&args(&["stop"])).is_err());
        assert!(parse_worker_args(&args(&["start"])).is_err());
    }
}
