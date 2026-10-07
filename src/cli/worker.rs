use crate::api::schema::{
    EmptyParams, Method, Request, WorkerAnswerParams, WorkerDecision, WorkerPromptParams,
    WorkerStartParams, WorkerTarget, WorkerWaitParams, WorkerWaitUntil,
};

const USAGE: &str =
    "usage: herdr worker <start|status|list|wait|prompt|interrupt|stop|kill|answer> ...
  herdr worker start [--cwd DIR] [--model MODEL] <prompt>
  herdr worker status <worker_id>
  herdr worker list
  herdr worker wait <worker_id> [--exit]
  herdr worker prompt <worker_id> <text>
  herdr worker interrupt <worker_id>
  herdr worker stop <worker_id>
  herdr worker kill <worker_id>
  herdr worker answer <worker_id> [--request REQUEST_ID] [--message TEXT] allow|deny|<choice>...
    A choice is an option's label or 1-based number, or free text; give one per
    question, and several options of a multi-select question separated by commas.";

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
        "answer" => Method::WorkerAnswer(parse_answer(rest)?),
        "help" | "--help" | "-h" => return Ok(None),
        _ => return Err(format!("unknown worker command: {subcommand}")),
    }))
}

/// `allow` or `deny` alone is a decision; anything else are the answers to
/// an `AskUserQuestion`, one per question.
fn parse_answer(args: &[String]) -> Result<WorkerAnswerParams, String> {
    let mut request_id = None;
    let mut message = None;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let value = || {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("missing value for {}", args[index]))
        };
        match args[index].as_str() {
            "--request" => {
                request_id = Some(value()?);
                index += 2;
            }
            "--message" => {
                message = Some(value()?);
                index += 2;
            }
            other => {
                positional.push(other.to_owned());
                index += 1;
            }
        }
    }
    let Some((worker_id, rest)) = positional.split_first() else {
        return Err("answer takes a worker id and an answer".into());
    };
    let (decision, answers) = match rest {
        [] => return Err("answer takes allow, deny or the chosen options".into()),
        [word] if word == "allow" => (Some(WorkerDecision::Allow), Vec::new()),
        [word] if word == "deny" => (Some(WorkerDecision::Deny), Vec::new()),
        answers => (None, answers.to_vec()),
    };
    Ok(WorkerAnswerParams {
        worker_id: worker_id.clone(),
        request_id,
        decision,
        answers,
        message,
    })
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

    #[test]
    fn parses_answers_as_a_decision_or_choices() {
        let Ok(Some(Method::WorkerAnswer(params))) =
            parse_worker_args(&args(&["answer", "w1", "deny", "--message", "no"]))
        else {
            panic!("answer must parse");
        };
        assert_eq!(params.decision, Some(WorkerDecision::Deny));
        assert_eq!(params.message.as_deref(), Some("no"));
        assert!(params.answers.is_empty());

        let Ok(Some(Method::WorkerAnswer(params))) = parse_worker_args(&args(&[
            "answer",
            "--request",
            "r1",
            "w1",
            "blue.txt",
            "1,3",
        ])) else {
            panic!("answer must parse");
        };
        assert_eq!(params.request_id.as_deref(), Some("r1"));
        assert_eq!(params.decision, None);
        assert_eq!(params.answers, vec!["blue.txt", "1,3"]);

        assert!(parse_worker_args(&args(&["answer", "w1"])).is_err());
        assert!(parse_worker_args(&args(&["answer"])).is_err());
    }
}
