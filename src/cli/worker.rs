use crate::api::schema::{
    EmptyParams, Method, Request, WorkerAckParams, WorkerAnswerParams, WorkerCommandTarget,
    WorkerDecision, WorkerInterruptParams, WorkerKillParams, WorkerObligationsParams,
    WorkerPromptParams, WorkerStartParams, WorkerTarget, WorkerWaitParams, WorkerWaitUntil,
};

const USAGE: &str =
    "usage: herdr worker <start|status|list|wait|ack|obligations|prompt|interrupt|stop|kill|answer|log|take-over> ...
  herdr worker start [--name TASK] [--cwd DIR] [--model MODEL] [--workspace ID]
                     [--folder-slot NAME --branch BRANCH [--base REF] [--fresh-build]]
                     (--prompt TEXT | <prompt>)
    --name names the worker's line in the sidebar (default: the prompt's first line);
    --workspace is the space it is listed under (default: the caller's space).
    --folder-slot runs it in the persistent worktree ../herdr-worktrees/NAME of
    --cwd's repository, on the new branch BRANCH from REF (default: master),
    one worker at a time, keeping target/ warm; --fresh-build removes the
    slot's target/ and Zig cache first.
    The caller's pane (HERDR_PANE_ID) and agent session (CLAUDE_CODE_SESSION_ID)
    become the worker's owner.
  herdr worker status <worker_id>
  herdr worker list
  herdr worker wait <worker_id> [--exit | --attention [--after SEQ]]
    Returns at the end of the turn, or with --exit when the process ended.
    --attention returns at once or at the first of a pending question, a
    turn's end or the worker's end, with the reason, the questions and seq;
    --after SEQ (the seq it returned) skips the state that seq already showed.
  herdr worker ack <worker_id> <seq>
    The owner handled the worker's events up to SEQ (the seq a wait or
    obligations returned); acknowledge after handling, not before.
  herdr worker obligations [--pane PANE_ID]
    The workers owned by PANE_ID (default: the caller's pane; all owned
    workers outside a pane) with a question, a turn end or an end not
    acknowledged yet, each with its reason and seq.
  herdr worker prompt <worker_id> <text>
    Its reply's turn_seq is the seq of the message it sent.
  herdr worker interrupt <worker_id> [--turn SEQ]
    --turn interrupts only that turn (its turn_seq); refused once it ended.
  herdr worker stop <worker_id>
  herdr worker kill <worker_id> [--force]
    SIGKILL to the worker and to its recorded tool sessions whose leader is
    still the recorded process; an exited or lost worker needs --force.
  herdr worker answer <worker_id> --request REQUEST_ID [--message TEXT] allow|deny|<choice>...
    --request names the question (its request_id in worker status or the ? list).
    A choice is an option's label or 1-based number, or free text; give one per
    question, and several options of a multi-select question separated by commas.
  herdr worker log [--follow] <worker_id>
    The worker's journal as text; --follow keeps printing new events until q.
  herdr worker take-over <worker_id>
    Interrupts and stops the worker, then resumes its session in a new tab.
  start, prompt, interrupt, stop, kill and answer take --command-id ID: the
  command runs once per ID; repeating it returns the first outcome (the same
  reply or refusal) without doing it again, and reusing ID for another
  command is refused (worker_command_conflict).";

pub(super) fn run_worker_command(args: &[String]) -> std::io::Result<i32> {
    if args.first().map(String::as_str) == Some("log") {
        return run_log(&args[1..]);
    }
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
    let (command_id, rest) = take_string_option(&args[1..], "--command-id")?;
    let rest = rest.as_slice();
    if command_id.is_some()
        && !matches!(
            subcommand,
            "start" | "prompt" | "interrupt" | "stop" | "kill" | "answer"
        )
    {
        return Err(format!("{subcommand} takes no --command-id"));
    }
    let target = |rest: &[String]| -> Result<WorkerTarget, String> {
        match rest {
            [worker_id] => Ok(WorkerTarget {
                worker_id: worker_id.clone(),
            }),
            _ => Err(format!("{subcommand} takes one worker id")),
        }
    };
    Ok(Some(match subcommand {
        "start" => Method::WorkerStart(WorkerStartParams {
            command_id,
            ..parse_start(rest)?
        }),
        "status" => Method::WorkerStatus(target(rest)?),
        "list" if rest.is_empty() => Method::WorkerList(EmptyParams::default()),
        "wait" => Method::WorkerWait(parse_wait(rest)?),
        "ack" => match rest {
            [worker_id, seq] => Method::WorkerAck(WorkerAckParams {
                worker_id: worker_id.clone(),
                seq: seq
                    .parse()
                    .map_err(|_| format!("ack takes a seq number, not {seq}"))?,
            }),
            _ => return Err("ack takes a worker id and a seq".into()),
        },
        "obligations" => {
            let (pane, rest) = take_string_option(rest, "--pane")?;
            if !rest.is_empty() {
                return Err("obligations takes only --pane".into());
            }
            Method::WorkerObligations(WorkerObligationsParams {
                owner_pane_id: pane.or_else(super::target::caller_pane_id),
            })
        }
        "prompt" => match rest {
            [worker_id, text] => Method::WorkerPrompt(WorkerPromptParams {
                worker_id: worker_id.clone(),
                text: text.clone(),
                command_id,
            }),
            _ => return Err("prompt takes a worker id and one text argument".into()),
        },
        "interrupt" => {
            let (turn, ids) = take_seq_option(rest, "--turn")?;
            let [worker_id] = ids.as_slice() else {
                return Err("interrupt takes one worker id".into());
            };
            Method::WorkerInterrupt(WorkerInterruptParams {
                worker_id: worker_id.clone(),
                turn,
                command_id,
            })
        }
        "stop" => Method::WorkerStop(WorkerCommandTarget {
            worker_id: target(rest)?.worker_id,
            command_id,
        }),
        "kill" => {
            let (force, ids): (Vec<&String>, Vec<&String>) =
                rest.iter().partition(|arg| arg.as_str() == "--force");
            let [worker_id] = ids.as_slice() else {
                return Err("kill takes one worker id".into());
            };
            Method::WorkerKill(WorkerKillParams {
                worker_id: (*worker_id).clone(),
                force: !force.is_empty(),
                command_id,
            })
        }
        "answer" => Method::WorkerAnswer(WorkerAnswerParams {
            command_id,
            ..parse_answer(rest)?
        }),
        "take-over" => Method::WorkerTakeOver(target(rest)?),
        "help" | "--help" | "-h" => return Ok(None),
        _ => return Err(format!("unknown worker command: {subcommand}")),
    }))
}

fn parse_wait(args: &[String]) -> Result<WorkerWaitParams, String> {
    let (after, rest) = take_seq_option(args, "--after")?;
    let mut until = None;
    let mut ids = Vec::new();
    for arg in rest {
        let flag = match arg.as_str() {
            "--exit" => WorkerWaitUntil::Exit,
            "--attention" => WorkerWaitUntil::Attention,
            _ => {
                ids.push(arg);
                continue;
            }
        };
        if until.replace(flag).is_some_and(|before| before != flag) {
            return Err("wait takes --exit or --attention, not both".into());
        }
    }
    let [worker_id] = ids.as_slice() else {
        return Err("wait takes one worker id".into());
    };
    let until = until.unwrap_or(WorkerWaitUntil::TurnEnd);
    if after.is_some() && until != WorkerWaitUntil::Attention {
        return Err("--after needs --attention".into());
    }
    Ok(WorkerWaitParams {
        worker_id: worker_id.clone(),
        until: Some(until),
        after,
    })
}

/// Takes `flag SEQ` out of `args`; returns the seq and the other arguments.
fn take_seq_option(args: &[String], flag: &str) -> Result<(Option<i64>, Vec<String>), String> {
    let (value, rest) = take_string_option(args, flag)?;
    let seq = value
        .map(|value| {
            value
                .parse()
                .map_err(|_| format!("{flag} takes a seq number, not {value}"))
        })
        .transpose()?;
    Ok((seq, rest))
}

/// Takes `flag VALUE` out of `args`; returns the value and the other
/// arguments.
fn take_string_option(
    args: &[String],
    flag: &str,
) -> Result<(Option<String>, Vec<String>), String> {
    let mut found = None;
    let mut rest = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg != flag {
            rest.push(arg.clone());
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        if found.replace(value.clone()).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    Ok((found, rest))
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
    // Naming the question keeps an answer from reaching one that replaced
    // the question the user read.
    if request_id.is_none() {
        return Err("answer takes --request REQUEST_ID, the question it answers".into());
    }
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
        command_id: None,
    })
}

fn parse_start(args: &[String]) -> Result<WorkerStartParams, String> {
    let mut cwd = None;
    let mut model = None;
    let mut name = None;
    let mut workspace_id = None;
    let mut folder_slot = None;
    let mut branch = None;
    let mut base = None;
    let mut fresh_build = false;
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
            "--name" => {
                name = Some(value()?);
                index += 2;
            }
            "--workspace" => {
                workspace_id = Some(value()?);
                index += 2;
            }
            "--folder-slot" => {
                folder_slot = Some(value()?);
                index += 2;
            }
            "--branch" => {
                branch = Some(value()?);
                index += 2;
            }
            "--base" => {
                base = Some(value()?);
                index += 2;
            }
            "--fresh-build" => {
                fresh_build = true;
                index += 1;
            }
            "--prompt" if prompt.is_none() => {
                prompt = Some(value()?);
                index += 2;
            }
            other if prompt.is_none() && !other.starts_with("--") => {
                prompt = Some(other.to_owned());
                index += 1;
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let prompt = prompt.ok_or("start needs a prompt")?;
    if folder_slot.is_some() != branch.is_some() {
        return Err("--folder-slot and --branch go together".into());
    }
    if folder_slot.is_none() && (base.is_some() || fresh_build) {
        return Err("--base and --fresh-build need --folder-slot".into());
    }
    let cwd = match cwd {
        Some(cwd) => std::path::PathBuf::from(cwd),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let cwd = std::path::absolute(&cwd).map_err(|error| error.to_string())?;
    Ok(WorkerStartParams {
        cwd: cwd.display().to_string(),
        prompt,
        model,
        name,
        workspace_id: workspace_id.or_else(super::target::caller_workspace_id),
        owner_pane_id: super::target::caller_pane_id(),
        owner_session_id: super::target::caller_pane_id()
            .and(std::env::var("CLAUDE_CODE_SESSION_ID").ok())
            .filter(|session| !session.trim().is_empty()),
        folder_slot,
        branch,
        base,
        fresh_build,
        command_id: None,
    })
}

/// `herdr worker log [--follow] <worker_id>`: the worker's journal as text
/// (`crate::workers::log_lines`). With `--follow`, as in the sidebar's log
/// popup, it keeps printing new events until `q`, Esc or Ctrl-C.
fn run_log(args: &[String]) -> std::io::Result<i32> {
    let follow = args.iter().any(|arg| arg == "--follow");
    let ids: Vec<&String> = args.iter().filter(|arg| *arg != "--follow").collect();
    let [worker_id] = ids.as_slice() else {
        eprintln!("log takes one worker id");
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let response = super::send_request(&Request {
        id: "cli:worker:log".into(),
        method: Method::WorkerStatus(WorkerTarget {
            worker_id: (*worker_id).clone(),
        }),
    })?;
    let worker = &response["result"]["worker"];
    let Some(path) = worker["journal_path"].as_str() else {
        return super::print_response(&response);
    };
    let title = format!(
        "worker {worker_id} · {}",
        worker["name"].as_str().unwrap_or("")
    );
    let file = std::fs::File::open(path)?;
    if !follow {
        println!("{title}");
        for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
            for shown in crate::workers::log_lines(&line?) {
                println!("{shown}");
            }
        }
        return Ok(0);
    }
    follow_log(&title, file)
}

/// Prints the journal and then what is appended to it, until a key closes
/// the view. Without a terminal on stdin it follows until killed.
fn follow_log(title: &str, file: std::fs::File) -> std::io::Result<i32> {
    use std::io::{BufRead, IsTerminal, Write};
    use std::sync::mpsc::RecvTimeoutError;

    // The journal is a file the server appends to, which gives no change
    // event without a file-watching dependency: like `tail -f`, look again
    // at this interval (external polling).
    const POLL: std::time::Duration = std::time::Duration::from_millis(250);

    struct RawMode;
    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }

    let (quit_tx, quit_rx) = std::sync::mpsc::channel::<()>();
    let _raw = if std::io::stdin().is_terminal() {
        crossterm::terminal::enable_raw_mode()?;
        std::thread::spawn(move || {
            use crossterm::event::{Event, KeyCode, KeyModifiers};
            while let Ok(event) = crossterm::event::read() {
                let Event::Key(key) = event else { continue };
                let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL));
                if quit {
                    let _ = quit_tx.send(());
                    return;
                }
            }
        });
        Some(RawMode)
    } else {
        // Nothing ever closes it but a signal.
        std::mem::forget(quit_tx);
        None
    };
    let mut out = std::io::stdout().lock();
    // Raw mode does not turn `\n` into a new line at the left edge.
    write!(out, "{title} — q closes\r\n\r\n")?;
    let mut reader = std::io::BufReader::new(file);
    let mut pending = String::new();
    loop {
        // The server writes a record and its newline in one write; a read
        // can still see half of it, which waits for the rest.
        while reader.read_line(&mut pending)? > 0 {
            if !pending.ends_with('\n') {
                break;
            }
            for shown in crate::workers::log_lines(pending.trim_end()) {
                write!(out, "{shown}\r\n")?;
            }
            pending.clear();
        }
        out.flush()?;
        match quit_rx.recv_timeout(POLL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(0),
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
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

        let Ok(Some(Method::WorkerStart(params))) = parse_worker_args(&args(&[
            "start",
            "--name",
            "fix login",
            "--cwd",
            "/tmp/repo",
            "--workspace",
            "ws-1",
            "--prompt",
            "do it",
        ])) else {
            panic!("start with --prompt must parse");
        };
        assert_eq!(params.name.as_deref(), Some("fix login"));
        assert_eq!(params.workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(params.prompt, "do it");
    }

    #[test]
    fn parses_start_in_a_folder_slot() {
        let Ok(Some(Method::WorkerStart(params))) = parse_worker_args(&args(&[
            "start",
            "--cwd",
            "/tmp/repo",
            "--folder-slot",
            "worker",
            "--branch",
            "w/fix",
            "--base",
            "main",
            "--fresh-build",
            "do it",
        ])) else {
            panic!("start with a folder slot must parse");
        };
        assert_eq!(params.folder_slot.as_deref(), Some("worker"));
        assert_eq!(params.branch.as_deref(), Some("w/fix"));
        assert_eq!(params.base.as_deref(), Some("main"));
        assert!(params.fresh_build);
        for bad in [
            &["start", "--folder-slot", "worker", "do it"][..],
            &["start", "--branch", "w/fix", "do it"],
            &["start", "--fresh-build", "do it"],
            &["start", "--base", "main", "do it"],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
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
    fn parses_wait_for_attention_after_a_seq_and_interrupt_of_a_turn() {
        assert!(matches!(
            parse_worker_args(&args(&["wait", "w1", "--attention", "--after", "42"])),
            Ok(Some(Method::WorkerWait(WorkerWaitParams {
                until: Some(WorkerWaitUntil::Attention),
                after: Some(42),
                ..
            })))
        ));
        assert!(matches!(
            parse_worker_args(&args(&["wait", "--attention", "w1"])),
            Ok(Some(Method::WorkerWait(WorkerWaitParams {
                until: Some(WorkerWaitUntil::Attention),
                after: None,
                ..
            })))
        ));
        for bad in [
            &["wait", "w1", "--after", "42"][..],
            &["wait", "w1", "--attention", "--exit"],
            &["wait", "w1", "--attention", "--after"],
            &["wait", "w1", "--attention", "--after", "x"],
            &["interrupt", "w1", "--turn"],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
        assert!(matches!(
            parse_worker_args(&args(&["interrupt", "w1", "--turn", "7"])),
            Ok(Some(Method::WorkerInterrupt(WorkerInterruptParams {
                turn: Some(7),
                ..
            })))
        ));
        assert!(matches!(
            parse_worker_args(&args(&["interrupt", "w1"])),
            Ok(Some(Method::WorkerInterrupt(WorkerInterruptParams {
                turn: None,
                ..
            })))
        ));
    }

    #[test]
    fn parses_kill_with_and_without_force() {
        assert!(matches!(
            parse_worker_args(&args(&["kill", "w1"])),
            Ok(Some(Method::WorkerKill(WorkerKillParams {
                force: false,
                ..
            })))
        ));
        assert!(matches!(
            parse_worker_args(&args(&["kill", "--force", "w1"])),
            Ok(Some(Method::WorkerKill(WorkerKillParams {
                force: true,
                ..
            })))
        ));
        assert!(parse_worker_args(&args(&["kill", "--force"])).is_err());
    }

    #[test]
    fn parses_answers_as_a_decision_or_choices() {
        let Ok(Some(Method::WorkerAnswer(params))) = parse_worker_args(&args(&[
            "answer",
            "w1",
            "--request",
            "r1",
            "deny",
            "--message",
            "no",
        ])) else {
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

        let Ok(Some(Method::WorkerAnswer(params))) = parse_worker_args(&args(&[
            "answer",
            "w1",
            "--command-id",
            "item-3:answer:r1",
            "--request",
            "r1",
            "allow",
        ])) else {
            panic!("answer with a command id must parse");
        };
        assert_eq!(params.command_id.as_deref(), Some("item-3:answer:r1"));
        assert_eq!(params.decision, Some(WorkerDecision::Allow));
        assert!(matches!(
            parse_worker_args(&args(&["stop", "--command-id", "c1", "w1"])),
            Ok(Some(Method::WorkerStop(WorkerCommandTarget { command_id: Some(id), .. })))
                if id == "c1"
        ));
        assert!(parse_worker_args(&args(&["status", "w1", "--command-id", "c1"])).is_err());
        assert!(parse_worker_args(&args(&["stop", "w1", "--command-id"])).is_err());

        assert!(parse_worker_args(&args(&["answer", "w1"])).is_err());
        assert!(parse_worker_args(&args(&["answer", "w1", "allow"])).is_err());
        assert!(parse_worker_args(&args(&["answer"])).is_err());
    }

    #[test]
    fn parses_ack_and_obligations() {
        assert_eq!(
            parse_worker_args(&args(&["ack", "w12", "40"])),
            Ok(Some(Method::WorkerAck(WorkerAckParams {
                worker_id: "w12".into(),
                seq: 40,
            })))
        );
        assert_eq!(
            parse_worker_args(&args(&["obligations", "--pane", "w1-2"])),
            Ok(Some(Method::WorkerObligations(WorkerObligationsParams {
                owner_pane_id: Some("w1-2".into()),
            })))
        );
        for bad in [
            &["ack", "w12"][..],
            &["ack", "w12", "x"],
            &["ack", "w12", "4", "5"],
            &["ack", "--command-id", "c", "w12", "4"],
            &["obligations", "w12"],
            &["obligations", "--pane"],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
