use std::collections::HashMap;

use crate::api::schema::{
    EmptyParams, Method, Request, WorkerAckParams, WorkerAnswerAsParams,
    WorkerAnswerFromPaneParams, WorkerAnswerParams, WorkerCommandTarget, WorkerDecision,
    WorkerDrainAction, WorkerDrainParams, WorkerEscalateParams, WorkerEventsParams,
    WorkerGatedRunParams, WorkerGeneratedFile, WorkerInterruptParams, WorkerKillParams,
    WorkerObligationsParams, WorkerPromptParams, WorkerRunsParams, WorkerStartChoiceParams,
    WorkerStartParams, WorkerTarget, WorkerVerifyParams, WorkerWaitDrainedParams, WorkerWaitParams,
    WorkerWaitUntil,
};

const USAGE: &str =
    "usage: herdr worker <start|run|status|list|runs|wait|events|ack|obligations|escalate|verify|prompt|interrupt|stop|kill|answer|log|take-over|drain|wait-drained> ...
  herdr worker start [--name TASK] [--cwd DIR] [--model MODEL] [--agent claude|pi|pi-anthropic]
                     [--workspace ID] [--item ID]
                     [--folder-slot NAME --branch BRANCH [--base REF] [--fresh-build]]
                     (--prompt TEXT | <prompt>)
    --agent picks the agent: claude (Claude Code on the subscription), pi
    (headless pi on the ChatGPT plan) or pi-anthropic (headless pi on the
    Anthropic API credits; a refused credit path is an error). Without it
    the worker uses [workers] default_agent (default claude), except that a
    --model alone names a Claude model and keeps Claude; --model does not
    combine with pi-anthropic, whose model is [workers] anthropic_api_model.
    --name names the worker's line in the sidebar (default: the prompt's first line);
    --workspace is the space it is listed under (default: the caller's space);
    --item is the TODO item it works on (its id, t-abcd2345), which `runs` lists it under.
    --folder-slot runs it in the persistent worktree
    ../herdr-worktrees/<repository>/NAME of --cwd's repository (or an earlier
    slot of that repository at ../herdr-worktrees/NAME), on the new branch
    BRANCH from REF (default: the repository's default branch), one worker at
    a time, keeping target/ warm; --fresh-build removes the slot's target/ and Zig cache first.
    The caller's pane (HERDR_PANE_ID) and agent session (CLAUDE_CODE_SESSION_ID)
    become the worker's owner.
  herdr worker run [--cwd DIR] [--agent claude|pi] [--model MODEL] [--name TASK]
                   [--workspace ID] [--item ID] [--ignore-usage] [--prompt-file FILE]
    A gated, blocking headless run for an orchestrator outside herdr's TODO
    driver (todo run). The server admits it through the same usage gate as
    todo run, for the repository of DIR (default: the current directory) and
    the agent's provider; --ignore-usage starts anyway, with the same meaning
    as todo run's (on the user's word; a refusal it overrides leaves the gate's
    state as it was, and the decision is recorded in the worker's journal).
    Then it starts the worker as worker start does (the same sandbox: writes
    only to DIR, its temp dir and caches) but in DIR itself, a git worktree
    the caller owns outside herdr's folder slots (refused by the slot's path,
    whether or not a worker ran there), with no branch, verify, landing or
    TODO edit. It sends the
    prompt (FILE's text, else standard input) as the one turn, waits for that
    turn's end on the worker's own events, stops the worker and waits for its
    exit, then prints one JSON object: worker_id, outcome (ended, failed,
    stopped, or policy_refused when the worker asked a question its policy
    leaves to the user, which nothing answers), message, text (the final
    assistant text), usage (per model: tokens and cost_usd, as the worker's
    journal records them), total_cost_usd, usage_gate (the gate's decision)
    and question (for policy_refused). Never retried: ending this command, or
    losing its connection, kills the worker.
    Exits 0 when the outcome is ended, 3 for any other outcome (the JSON is
    printed all the same), 4 when the usage gate refuses (its reason on
    stderr; nothing starts), 2 for a usage error and 1 for any other refusal
    or error (a directory outside a git repository, in a folder slot or one
    worker start refuses).
  herdr worker status <worker_id>
  herdr worker list
  herdr worker runs [--item ID] [--repo DIR]
    Every worker's run (one worker process; prompts add turns to it) grouped
    by its TODO item and repository: start and end, outcome, turns, the
    commits its WORKER-DONE lines named, questions, journal and its latest
    verification. Runs started without --item are listed as unassigned. --repo DIR keeps the runs of
    DIR's repository. The run that landed a commit is found with
    herdr todo runs --commit SHA.
  herdr worker wait <worker_id> [--exit | --attention [--after SEQ]]
    Returns at the end of the turn, or with --exit when the process ended.
    --attention returns at once or at the first of a pending question, a
    turn's end or the worker's end, with the reason, the questions and seq;
    --after SEQ (the seq it returned) skips the state that seq already showed.
    Without --after, the worker's owner (the caller's pane, or its tenure)
    waits after the seq it acknowledged with ack: wait, handle, ack, the same
    wait again, with no cursor in the shell. Options take --name=VALUE too.
  herdr worker events [--owner COORDINATOR_ID] [--after CURSOR] [--wait] [--limit N] [--snapshot]
    One coordinator's inbox over all of its workers (default owner: the
    tenure bound to the caller's pane): every question, turn end, exit,
    joined, left and reowned after CURSOR, in order, at most N (100) per
    reply, with next_cursor to pass as --after next and more when more
    follow. --wait blocks while there is none. Without --after, or with a
    cursor the server cannot serve (resync_required), the reply is a
    snapshot of the owner's workers and their pending questions instead;
    --snapshot adds it to a reply after CURSOR. Reading answers nothing: a
    question stays pending until it is answered. One wait per coordinator:
    drain the batch, handle it, then wait again from its next_cursor.
  herdr worker ack <worker_id> <seq>
    The owner handled the worker's events up to SEQ (the seq a wait or
    obligations returned); acknowledge after handling, not before.
  herdr worker obligations [--pane PANE_ID]
    The workers owned by PANE_ID (default: the caller's pane; all owned
    workers outside a pane) with a question, a turn end or an end not
    acknowledged yet, each with its reason and seq.
  herdr worker escalate <worker_id> --request REQUEST_ID
    The owner hands a question it will not answer to the user. An owned
    worker's question waits quietly in the ? list (awaiting the coordinator)
    until then, or until its owner's pane closes, its agent exits, hits a
    limit, ends its turn or is blocked on its own question.
  herdr worker verify <worker_id> --base SHA --message SUBJECT --paths GLOB...
                      [--cmd COMMAND] [--generated PATH=COMMAND]...
    Herdr, not the worker, decides whether its commit is ready, in its
    directory and outside its sandbox: exactly one commit since SHA, whose
    message is SUBJECT alone (no body, no trailers), changing only paths
    matching the git globs; the worktree clean (ignored files aside) and no
    process of the worker left (stop it first); each generated PATH
    regenerated by its COMMAND equal to the committed bytes; and COMMAND
    (sh -c, e.g. the tests) exiting 0, however long it takes. The commands
    run with this shell's environment (HERDR_* dropped), not the server's.
    The verdict, verified, failed or unavailable (a check could not run, or
    a command failed on its build environment, such as a missing tool or a
    wrong Zig version; never a pass), with each check's evidence, is stored
    with the run. Exits 0 only when verified, 1 when failed, 3 when
    unavailable.
  herdr worker prompt <worker_id> <text>
    Its reply's turn_seq is the seq of the message it sent.
  herdr worker interrupt <worker_id> [--turn SEQ]
    --turn interrupts only that turn (its turn_seq); refused once it ended.
  herdr worker stop <worker_id>
  herdr worker kill <worker_id> [--force]
    SIGKILL to the worker and to its recorded tool sessions whose leader is
    still the recorded process; an exited or lost worker needs --force.
  herdr worker answer <worker_id> --request REQUEST_ID [--message TEXT] [--coordinator ID]
                      allow|deny|<choice>...
    --request names the question (its request_id in worker status or the ? list).
    A choice is an option's label or 1-based number, or free text; give one per
    question, and several options of a multi-select question separated by commas.
    --coordinator ID answers as that coordination tenure (c-..., as coordinator
    start or worker events named it): once the worker belongs to another tenure
    the answer is refused (ownership_transferred, naming it), never applied, and
    passed to that tenure's inbox as a late_answer.
  herdr worker log [--follow|--takeover] <worker_id>
    The worker's journal as text; --follow keeps printing new events until q.
    --takeover ends it with the mark a takeover's tab shows before the
    session it resumes.
  herdr worker take-over [--force] <worker_id>
    Interrupts and stops the worker, then resumes its session in a new tab.
    A takeover an earlier server left unfinished is adopted when its tab or a
    process resuming the session is found; otherwise only --force retries it,
    and a tab that earlier attempt opened unseen would be a second writer of
    the session.
  herdr worker drain <start [--reason TEXT]|status|cancel>
    start stops admitting new turns before an install: prompt and start are
    refused (workers_draining, naming TEXT) while running turns go on; it
    lists the workers still in a turn. cancel admits turns again; a handoff
    ends the drain too (the new server does not drain).
  herdr worker wait-drained
    Blocks until no worker is in a turn, woken by their turn-end events,
    printing each turn as it ends. Exits 1 when the drain it saw is
    cancelled before that.
  start, prompt, interrupt, stop, kill and answer take --command-id ID: the
  command runs once per ID; repeating it returns the first outcome (the same
  reply or refusal) without doing it again, and reusing ID for another
  command is refused (worker_command_conflict).";

pub(super) fn run_worker_command(args: &[String]) -> std::io::Result<i32> {
    if let Some(help) = subcommand_help(args) {
        println!("{help}");
        return Ok(0);
    }
    match args.first().map(String::as_str) {
        Some("log") => return run_log(&args[1..]),
        Some("wait-drained") => return run_wait_drained(&args[1..]),
        Some("run") => return run_gated(&args[1..]),
        _ => {}
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
    let verifying = matches!(method, Method::WorkerVerify(_));
    let hints = matches!(method, Method::WorkerVerify(_) | Method::WorkerWait(_));
    let response = match &method {
        Method::WorkerEvents(params) if params.wait => {
            let what = match &params.owner {
                Some(owner) => format!("coordinator {owner}'s events"),
                None => "the caller's coordinator's events".to_owned(),
            };
            let reply = super::reconnect::wait("worker events", &what, |_| Request {
                id: id.clone(),
                method: method.clone(),
            })?;
            super::reconnect::report_gave_up("worker events", &what, &reply);
            reply.response
        }
        Method::WorkerWait(params) => {
            let what = format!("worker {}", params.worker_id);
            let reply = super::reconnect::wait("worker wait", &what, |_| Request {
                id: id.clone(),
                method: method.clone(),
            })?;
            super::reconnect::report_gave_up("worker wait", &what, &reply);
            reply.response
        }
        _ => super::send_request(&Request { id, method })?,
    };
    let code = super::print_response(&response)?;
    if hints {
        super::report::hint_reply(
            &response,
            &super::report::command_line(&["herdr", "worker"], args),
        );
    }
    if !verifying || code != 0 {
        return Ok(code);
    }
    // A verdict other than `verified` is not a success.
    Ok(
        match response["result"]["verification"]["verdict"].as_str() {
            Some("verified") => 0,
            Some("failed") => 1,
            _ => 3,
        },
    )
}

/// `herdr worker run`'s exit status for an outcome other than `ended`.
const GATED_RUN_NOT_ENDED: i32 = 3;
/// `herdr worker run`'s exit status when the usage gate refuses the run.
const GATED_RUN_USAGE_GATE: i32 = 4;

/// `herdr worker run`: one gated, blocking run ([`USAGE`]). Never resent:
/// the server ends the worker when the connection drops, so a second send
/// would be a second run.
fn run_gated(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_gated_run(args, &mut std::io::stdin().lock()) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            if let Some(help) = subcommand_help(&["run".to_owned(), "--help".to_owned()]) {
                eprintln!("{help}");
            }
            return Ok(2);
        }
    };
    let response = super::send_request(&Request {
        id: "cli:worker:run".into(),
        method: Method::WorkerGatedRun(params),
    })?;
    Ok(gated_run_exit(&response))
}

/// Prints `worker.run`'s reply (the run's object on stdout, or the gate's
/// reason or the error on stderr) and returns the exit status.
fn gated_run_exit(response: &serde_json::Value) -> i32 {
    if let Some(error) = response.get("error") {
        if error["code"] == "usage_gate" {
            eprintln!(
                "{}",
                error["message"]
                    .as_str()
                    .unwrap_or("the usage gate refused the run")
            );
            return GATED_RUN_USAGE_GATE;
        }
        eprintln!("{response}");
        return 1;
    }
    let run = &response["result"]["run"];
    println!("{run}");
    if run["outcome"] == "ended" {
        0
    } else {
        GATED_RUN_NOT_ENDED
    }
}

/// `worker run`'s options; the prompt from `--prompt-file`, else from
/// `stdin`.
fn parse_gated_run(
    args: &[String],
    stdin: &mut dyn std::io::Read,
) -> Result<WorkerGatedRunParams, String> {
    let (cwd, rest) = take_string_option(args, "--cwd")?;
    let (agent, rest) = take_string_option(&rest, "--agent")?;
    let (model, rest) = take_string_option(&rest, "--model")?;
    let (name, rest) = take_string_option(&rest, "--name")?;
    let (workspace_id, rest) = take_string_option(&rest, "--workspace")?;
    let (item, rest) = take_string_option(&rest, "--item")?;
    let (prompt_file, rest) = take_string_option(&rest, "--prompt-file")?;
    let ignore_usage = match rest.as_slice() {
        [] => false,
        [flag] if flag == "--ignore-usage" => true,
        _ => {
            return Err(format!(
                "unexpected argument for worker run: {}",
                rest.join(" ")
            ))
        }
    };
    let agent = match agent.as_deref() {
        None => None,
        Some("claude") => Some(crate::api::schema::WorkerAgent::Claude),
        Some("pi") => Some(crate::api::schema::WorkerAgent::Pi),
        Some(other) => return Err(format!("--agent takes claude or pi, not {other}")),
    };
    let prompt = match prompt_file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read --prompt-file {path}: {error}"))?,
        None => {
            let mut text = String::new();
            stdin
                .read_to_string(&mut text)
                .map_err(|error| format!("cannot read the prompt from standard input: {error}"))?;
            text
        }
    };
    if prompt.trim().is_empty() {
        return Err("worker run needs a prompt (--prompt-file FILE or standard input)".into());
    }
    let cwd = match cwd {
        Some(cwd) => std::path::PathBuf::from(cwd),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let cwd = std::path::absolute(&cwd).map_err(|error| error.to_string())?;
    Ok(WorkerGatedRunParams {
        cwd: cwd.display().to_string(),
        prompt,
        agent,
        model,
        name,
        workspace_id: workspace_id.or_else(super::target::caller_workspace_id),
        item,
        ignore_usage,
    })
}

/// The usage of `herdr worker <subcommand> --help` (or `-h`): that subcommand's part of
/// [`USAGE`], or all of it for a subcommand it does not describe.
fn subcommand_help(args: &[String]) -> Option<String> {
    let (subcommand, rest) = args.split_first()?;
    if !rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        return None;
    }
    let heading = format!("  herdr worker {subcommand}");
    let mut section = Vec::new();
    for line in USAGE.lines().skip(1) {
        let starts_entry = line.starts_with("  herdr worker ");
        if starts_entry && !section.is_empty() {
            break;
        }
        if !section.is_empty()
            || starts_entry
                && line
                    .strip_prefix(&heading)
                    .is_some_and(|tail| tail.is_empty() || tail.starts_with(' '))
        {
            section.push(line);
        }
    }
    // The closing paragraph on --command-id is not part of the last entry.
    let section: Vec<&str> = section
        .into_iter()
        .take_while(|line| !line.starts_with("  start, prompt"))
        .collect();
    Some(if section.is_empty() {
        USAGE.to_string()
    } else {
        format!("usage:\n{}", section.join("\n"))
    })
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
        "start" => {
            let (start, agent_choice) = parse_start(rest)?;
            Method::WorkerStartChoice(WorkerStartChoiceParams {
                start: WorkerStartParams {
                    command_id,
                    ..start
                },
                agent_choice,
            })
        }
        "status" => Method::WorkerStatus(target(rest)?),
        "list" if rest.is_empty() => Method::WorkerList(EmptyParams::default()),
        "wait" => Method::WorkerWait(parse_wait(rest)?),
        "events" => Method::WorkerEvents(parse_events(rest)?),
        "ack" => match rest {
            [worker_id, seq] => Method::WorkerAck(WorkerAckParams {
                caller_pane_id: super::target::caller_pane_id(),
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
        "runs" => {
            let (item, rest) = take_string_option(rest, "--item")?;
            let (repo, rest) = take_string_option(&rest, "--repo")?;
            if !rest.is_empty() {
                return Err("runs takes only --item and --repo".into());
            }
            let repo = repo
                .map(|dir| {
                    std::path::absolute(&dir)
                        .map(|dir| dir.display().to_string())
                        .map_err(|error| error.to_string())
                })
                .transpose()?;
            Method::WorkerRuns(WorkerRunsParams { item, repo })
        }
        "prompt" => match rest {
            [worker_id, text] => Method::WorkerPrompt(WorkerPromptParams {
                caller_pane_id: super::target::caller_pane_id(),
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
                caller_pane_id: super::target::caller_pane_id(),
                worker_id: worker_id.clone(),
                turn,
                command_id,
            })
        }
        "stop" => Method::WorkerStop(WorkerCommandTarget {
            worker_id: target(rest)?.worker_id,
            caller_pane_id: super::target::caller_pane_id(),
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
                caller_pane_id: super::target::caller_pane_id(),
                command_id,
            })
        }
        "answer" => {
            let (coordinator, rest) = take_string_option(rest, "--coordinator")?;
            let answer = WorkerAnswerParams {
                command_id,
                ..parse_answer(&rest)?
            };
            match coordinator {
                Some(coordinator_id) => Method::WorkerAnswerAs(WorkerAnswerAsParams {
                    coordinator_id,
                    answer,
                }),
                None => match super::target::caller_pane_id() {
                    Some(caller_pane_id) => {
                        Method::WorkerAnswerFromPane(WorkerAnswerFromPaneParams {
                            caller_pane_id,
                            answer,
                        })
                    }
                    None => Method::WorkerAnswer(answer),
                },
            }
        }
        "take-over" => {
            let (force, ids): (Vec<String>, Vec<String>) =
                rest.iter().cloned().partition(|arg| arg == "--force");
            let worker = target(&ids)?;
            if force.is_empty() {
                Method::WorkerTakeOver(worker)
            } else {
                Method::WorkerForceTakeOver(worker)
            }
        }
        "escalate" => {
            let (request, rest) = take_string_option(rest, "--request")?;
            match (rest.as_slice(), request) {
                ([worker_id], Some(request_id)) => Method::WorkerEscalate(WorkerEscalateParams {
                    caller_pane_id: super::target::caller_pane_id(),
                    worker_id: worker_id.clone(),
                    request_id,
                }),
                _ => return Err("escalate takes a worker id and --request REQUEST_ID".into()),
            }
        }
        "drain" => {
            let (reason, rest) = take_string_option(rest, "--reason")?;
            let action = match rest.as_slice() {
                [action] if action == "start" => WorkerDrainAction::Start,
                [action] if action == "status" => WorkerDrainAction::Status,
                [action] if action == "cancel" => WorkerDrainAction::Cancel,
                _ => return Err("drain takes start, status or cancel".into()),
            };
            if reason.is_some() && action != WorkerDrainAction::Start {
                return Err("--reason goes with drain start".into());
            }
            Method::WorkerDrain(WorkerDrainParams { action, reason })
        }
        "verify" => Method::WorkerVerify(WorkerVerifyParams {
            env: caller_env(),
            ..parse_verify(rest)?
        }),
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
            // Named whole, so an option a shell passed as one argument
            // (`"--after 42"`) shows as such instead of as a second id.
            _ if arg.starts_with('-') => return Err(format!("wait does not take {arg:?}")),
            _ => {
                ids.push(arg);
                continue;
            }
        };
        if until.replace(flag).is_some_and(|before| before != flag) {
            return Err("wait takes --exit or --attention, not both".into());
        }
    }
    let worker_id = match ids.as_slice() {
        [worker_id] => worker_id.clone(),
        [] => return Err("wait takes one worker id".into()),
        [_, extra @ ..] => {
            return Err(format!(
                "wait takes one worker id, not also {}",
                extra
                    .iter()
                    .map(|arg| format!("{arg:?}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    };
    let until = until.unwrap_or(WorkerWaitUntil::TurnEnd);
    if after.is_some() && until != WorkerWaitUntil::Attention {
        return Err("--after needs --attention".into());
    }
    Ok(WorkerWaitParams {
        worker_id,
        until: Some(until),
        after,
        // Lets the owner's wait without --after start after what it acked.
        caller_pane_id: (until == WorkerWaitUntil::Attention && after.is_none())
            .then(super::target::caller_pane_id)
            .flatten(),
    })
}

fn parse_events(args: &[String]) -> Result<WorkerEventsParams, String> {
    let (owner, rest) = take_string_option(args, "--owner")?;
    let (after, rest) = take_string_option(&rest, "--after")?;
    let (limit, rest) = take_string_option(&rest, "--limit")?;
    let limit = limit
        .map(|limit| {
            limit
                .parse()
                .ok()
                .filter(|limit| *limit > 0)
                .ok_or_else(|| format!("--limit takes a positive number, not {limit}"))
        })
        .transpose()?;
    let mut params = WorkerEventsParams {
        owner_pane_id: owner
            .is_none()
            .then(super::target::caller_pane_id)
            .flatten(),
        owner,
        after,
        limit,
        ..WorkerEventsParams::default()
    };
    for arg in rest {
        match arg.as_str() {
            "--wait" => params.wait = true,
            "--snapshot" => params.snapshot = true,
            _ => return Err(format!("events does not take {arg}")),
        }
    }
    if params.owner.is_none() && params.owner_pane_id.is_none() {
        return Err("events needs --owner outside a coordinator's pane".into());
    }
    Ok(params)
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

/// Takes `flag VALUE` or `flag=VALUE` out of `args`; returns the value and
/// the other arguments.
pub(super) fn take_string_option(
    args: &[String],
    flag: &str,
) -> Result<(Option<String>, Vec<String>), String> {
    let mut found = None;
    let mut rest = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let value = if arg == flag {
            args.next()
                .ok_or_else(|| format!("missing value for {flag}"))?
                .clone()
        } else if let Some(value) = arg
            .strip_prefix(flag)
            .and_then(|value| value.strip_prefix('='))
        {
            value.to_owned()
        } else {
            rest.push(arg.clone());
            continue;
        };
        if found.replace(value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    Ok((found, rest))
}

/// `verify <id> --base SHA --message SUBJECT --paths GLOB... [--cmd COMMAND]
/// [--generated PATH=COMMAND]...`: `--paths` takes every argument up to
/// the next option.
fn parse_verify(args: &[String]) -> Result<WorkerVerifyParams, String> {
    let mut worker_id = None;
    let mut base = None;
    let mut message = None;
    let mut paths: Option<Vec<String>> = None;
    let mut command = None;
    let mut generated = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = || {
            args.get(index + 1)
                .cloned()
                .ok_or_else(|| format!("missing value for {flag}"))
        };
        let once = |slot: &mut Option<String>, value: String| {
            if slot.replace(value).is_some() {
                Err(format!("{flag} given twice"))
            } else {
                Ok(())
            }
        };
        match flag {
            "--base" => once(&mut base, value()?)?,
            "--message" => once(&mut message, value()?)?,
            "--cmd" => once(&mut command, value()?)?,
            "--generated" => {
                let value = value()?;
                let Some((path, regenerate)) = value.split_once('=') else {
                    return Err(format!("--generated takes PATH=COMMAND, not {value}"));
                };
                if path.is_empty() || regenerate.is_empty() {
                    return Err(format!("--generated takes PATH=COMMAND, not {value}"));
                }
                generated.push(WorkerGeneratedFile {
                    path: path.to_owned(),
                    command: regenerate.to_owned(),
                });
            }
            "--paths" => {
                if paths.is_some() {
                    return Err("--paths given twice".into());
                }
                let globs: Vec<String> = args[index + 1..]
                    .iter()
                    .take_while(|arg| !arg.starts_with("--"))
                    .cloned()
                    .collect();
                if globs.is_empty() {
                    return Err("missing value for --paths".into());
                }
                index += 1 + globs.len();
                paths = Some(globs);
                continue;
            }
            _ if flag.starts_with("--") => return Err(format!("verify takes no {flag}")),
            _ => {
                if worker_id.replace(flag.to_owned()).is_some() {
                    return Err("verify takes one worker id".into());
                }
                index += 1;
                continue;
            }
        }
        index += 2;
    }
    match (worker_id, base, message, paths) {
        (Some(worker_id), Some(base), Some(expected_message), Some(allowed_paths)) => {
            Ok(WorkerVerifyParams {
                worker_id,
                base,
                expected_message,
                allowed_paths,
                command,
                generated,
                env: None,
            })
        }
        _ => Err("verify takes a worker id, --base, --message and --paths".into()),
    }
}

/// The environment `verify`'s commands run with: the caller's, whose
/// `ZIG`, `PATH` and the like the herdr server, started elsewhere, may
/// lack. Without the `HERDR_*` variables, which name the caller's pane and
/// session, and without variables that are not UTF-8. None when the server
/// is on another machine, where this one's environment means nothing.
pub(super) fn caller_env() -> Option<HashMap<String, String>> {
    if super::target::is_remote() {
        return None;
    }
    Some(
        std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(key, _)| !key.starts_with("HERDR_"))
            .collect(),
    )
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

fn parse_start(
    args: &[String],
) -> Result<
    (
        WorkerStartParams,
        Option<crate::api::schema::WorkerAgentChoice>,
    ),
    String,
> {
    let mut agent_choice = None;
    let mut cwd = None;
    let mut model = None;
    let mut name = None;
    let mut workspace_id = None;
    let mut folder_slot = None;
    let mut branch = None;
    let mut base = None;
    let mut fresh_build = false;
    let mut item = None;
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
            "--agent" => {
                agent_choice = Some(super::todo::parse_agent_choice("--agent", &value()?)?);
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
            "--item" => {
                item = Some(value()?);
                index += 2;
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
    Ok((
        WorkerStartParams {
            cwd: cwd.display().to_string(),
            prompt,
            model,
            agent: None,
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
            item,
            command_id: None,
        },
        agent_choice,
    ))
}

/// `herdr worker wait-drained`: waits until no worker is in a turn, one
/// `worker.wait_drained` per change, each passing the workers the last reply
/// showed in a turn, so the server answers at the next turn's end. Prints
/// what it waits for and each turn as it ends.
fn run_wait_drained(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("wait-drained takes no arguments");
        eprintln!("{USAGE}");
        return Ok(2);
    }
    let mut in_turn: Vec<String> = Vec::new();
    let mut draining = None;
    loop {
        let response = super::send_request(&Request {
            id: "cli:worker:wait-drained".into(),
            method: Method::WorkerWaitDrained(WorkerWaitDrainedParams {
                in_turn: in_turn.clone(),
                draining,
            }),
        })?;
        if response.get("error").is_some() {
            return super::print_response(&response);
        }
        let drain = &response["result"]["drain"];
        let now_draining = drain["draining"].as_bool().unwrap_or(false);
        if draining == Some(true) && !now_draining {
            eprintln!("the drain was cancelled while turns still run");
            return Ok(1);
        }
        draining = Some(now_draining);
        let describe = |worker: &serde_json::Value| {
            format!(
                "{} ({}{})",
                worker["worker_id"].as_str().unwrap_or("?"),
                worker["state"].as_str().unwrap_or("?"),
                worker["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(|name| format!(", {name}"))
                    .unwrap_or_default()
            )
        };
        for worker in drain["ended"].as_array().into_iter().flatten() {
            println!("turn ended: {}", describe(worker));
        }
        let now: Vec<&serde_json::Value> =
            drain["in_turn"].as_array().into_iter().flatten().collect();
        for worker in &now {
            let id = worker["worker_id"].as_str().unwrap_or("");
            if !in_turn.iter().any(|known| known == id) {
                println!("waiting for the turn of {}", describe(worker));
            }
        }
        let starting = drain["starting"].as_u64().unwrap_or(0);
        if now.is_empty() && starting == 0 {
            println!("no worker is in a turn");
            return Ok(0);
        }
        if now.is_empty() {
            println!("waiting for {starting} worker start(s) to register");
        }
        in_turn = now
            .iter()
            .filter_map(|worker| worker["worker_id"].as_str().map(str::to_owned))
            .collect();
    }
}

/// The line a takeover's tab shows between the worker's transcript and the
/// session it resumes.
pub(crate) const TAKEOVER_MARK: &str =
    "──── taken over: its session goes on here, interactively ────";

/// `herdr worker log [--follow|--takeover] <worker_id>`: the worker's
/// journal as text (`crate::workers::log_lines`, the lines its tab shows).
/// With `--follow`, as in an older server's log popup, it keeps printing new
/// events until `q`, Esc or Ctrl-C. With `--takeover`, a takeover's tab
/// prints it before resuming the session: the transcript, then
/// [`TAKEOVER_MARK`].
fn run_log(args: &[String]) -> std::io::Result<i32> {
    let follow = args.iter().any(|arg| arg == "--follow");
    let takeover = args.iter().any(|arg| arg == "--takeover");
    let ids: Vec<&String> = args
        .iter()
        .filter(|arg| *arg != "--follow" && *arg != "--takeover")
        .collect();
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
        if takeover {
            println!("{TAKEOVER_MARK}");
        }
        return Ok(0);
    }
    follow_lines(&title, file, crate::workers::log_lines)
}

/// Prints the file's lines (each as `shown` turns it into lines) and then
/// what is appended to it, until a key closes the view. Without a terminal
/// on stdin it follows until killed.
pub(super) fn follow_lines(
    title: &str,
    file: std::fs::File,
    shown: impl Fn(&str) -> Vec<String>,
) -> std::io::Result<i32> {
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
            for line in shown(pending.trim_end()) {
                write!(out, "{line}\r\n")?;
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

    /// The pane id the parser reads from the environment: set when the tests
    /// run inside a Herdr pane, absent elsewhere. Tests expect it, so they pass
    /// in both.
    fn caller() -> Option<String> {
        super::super::target::caller_pane_id()
    }

    /// `worker answer` parses to `worker.answer`, or to
    /// `worker.answer_from_pane` carrying the caller's pane inside a pane.
    fn parsed_answer(items: &[&str]) -> Option<WorkerAnswerParams> {
        match parse_worker_args(&args(items)) {
            Ok(Some(Method::WorkerAnswer(answer))) if caller().is_none() => Some(answer),
            Ok(Some(Method::WorkerAnswerFromPane(params)))
                if Some(&params.caller_pane_id) == caller().as_ref() =>
            {
                Some(params.answer)
            }
            _ => None,
        }
    }

    #[test]
    fn every_subcommand_answers_help() {
        let first = USAGE.lines().next().unwrap_or_default();
        let list = first
            .split_once('<')
            .and_then(|(_, rest)| rest.split_once('>'))
            .map(|(list, _)| list)
            .unwrap_or_default();
        assert!(list.contains("verify"));
        for subcommand in list.split('|') {
            for flag in ["--help", "-h"] {
                let help = subcommand_help(&args(&[subcommand, flag]))
                    .unwrap_or_else(|| panic!("{subcommand} {flag} shows no help"));
                let heading = format!("  herdr worker {subcommand}");
                assert!(
                    help.lines().nth(1).is_some_and(|line| line
                        .strip_prefix(&heading)
                        .is_some_and(|tail| tail.is_empty() || tail.starts_with(' '))),
                    "{subcommand}: {help}"
                );
                assert!(!help.contains("  start, prompt"), "{subcommand}: {help}");
            }
        }
        let verify = subcommand_help(&args(&["verify", "w1", "--help"])).unwrap_or_default();
        assert!(verify.contains("--generated PATH=COMMAND"));
        assert!(!verify.contains("herdr worker prompt"));
        assert_eq!(subcommand_help(&args(&["status", "w1"])), None);
    }

    #[test]
    fn parses_worker_run_with_the_prompt_from_stdin_or_a_file() {
        let dir = std::env::temp_dir().join(format!("herdr-worker-run-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut stdin = "do the task\n".as_bytes();
        let params = parse_gated_run(
            &args(&[
                "--cwd",
                &dir.display().to_string(),
                "--agent",
                "pi",
                "--model",
                "m",
                "--item",
                "t-abcd2345",
                "--ignore-usage",
            ]),
            &mut stdin,
        )
        .unwrap();
        assert_eq!(params.prompt, "do the task\n");
        assert_eq!(params.agent, Some(crate::api::schema::WorkerAgent::Pi));
        assert_eq!(params.model.as_deref(), Some("m"));
        assert_eq!(params.item.as_deref(), Some("t-abcd2345"));
        assert!(params.ignore_usage);
        assert_eq!(params.cwd, dir.display().to_string());

        let file = dir.join("prompt.txt");
        std::fs::write(&file, "from the file").unwrap();
        let params = parse_gated_run(
            &args(&["--prompt-file", &file.display().to_string()]),
            &mut "ignored".as_bytes(),
        )
        .unwrap();
        assert_eq!(params.prompt, "from the file");
        assert_eq!(params.agent, None);
        assert!(!params.ignore_usage);

        for (bad, why) in [
            (vec!["--agent", "pi-anthropic"], "claude or pi"),
            (vec!["--bogus"], "unexpected argument"),
        ] {
            let error = parse_gated_run(&args(&bad), &mut "task".as_bytes()).unwrap_err();
            assert!(error.contains(why), "{error}");
        }
        let error = parse_gated_run(&args(&[]), &mut " \n".as_bytes()).unwrap_err();
        assert!(error.contains("needs a prompt"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn worker_run_exits_by_outcome_and_with_its_own_status_for_the_usage_gate() {
        let refused = serde_json::json!({"id": "x", "error": {"code": "usage_gate",
            "message": "Claude usage is high"}});
        assert_eq!(gated_run_exit(&refused), GATED_RUN_USAGE_GATE);
        let invalid = serde_json::json!({"id": "x", "error": {"code": "invalid",
            "message": "not in a git repository"}});
        assert_eq!(gated_run_exit(&invalid), 1);
        let run = |outcome: &str| {
            serde_json::json!({"id": "x", "result": {"type": "worker_gated_run",
                "run": {"worker_id": "w1", "outcome": outcome}}})
        };
        assert_eq!(gated_run_exit(&run("ended")), 0);
        for outcome in ["failed", "stopped", "policy_refused"] {
            assert_eq!(gated_run_exit(&run(outcome)), GATED_RUN_NOT_ENDED);
        }
        assert_ne!(GATED_RUN_USAGE_GATE, GATED_RUN_NOT_ENDED);
    }

    #[test]
    fn parses_start_with_options() {
        let Ok(Some(Method::WorkerStartChoice(WorkerStartChoiceParams { start: params, .. }))) =
            parse_worker_args(&args(&[
                "start",
                "--cwd",
                "/tmp/repo",
                "--model",
                "sonnet",
                "do it",
            ]))
        else {
            panic!("start must parse");
        };
        assert_eq!(params.cwd, "/tmp/repo");
        assert_eq!(params.model.as_deref(), Some("sonnet"));
        assert_eq!(params.prompt, "do it");

        let Ok(Some(Method::WorkerStartChoice(WorkerStartChoiceParams { start: params, .. }))) =
            parse_worker_args(&args(&[
                "start",
                "--name",
                "fix login",
                "--cwd",
                "/tmp/repo",
                "--workspace",
                "ws-1",
                "--prompt",
                "do it",
            ]))
        else {
            panic!("start with --prompt must parse");
        };
        assert_eq!(params.name.as_deref(), Some("fix login"));
        assert_eq!(params.workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(params.prompt, "do it");
    }

    #[test]
    fn start_takes_an_agent_by_name() {
        use crate::api::schema::WorkerAgentChoice;
        let choice = |extra: &[&str]| {
            let mut words = vec!["start"];
            words.extend_from_slice(extra);
            words.push("do it");
            let Ok(Some(Method::WorkerStartChoice(params))) = parse_worker_args(&args(&words))
            else {
                panic!("start must parse");
            };
            params.agent_choice
        };
        assert_eq!(choice(&[]), None);
        assert_eq!(
            choice(&["--agent", "pi-anthropic"]),
            Some(WorkerAgentChoice::PiAnthropic)
        );
        assert_eq!(choice(&["--agent", "pi"]), Some(WorkerAgentChoice::Pi));
        assert_eq!(
            choice(&["--agent", "claude", "--model", "sonnet"]),
            Some(WorkerAgentChoice::Claude)
        );
        assert!(parse_worker_args(&args(&["start", "--agent", "gpt", "x"])).is_err());
    }

    #[test]
    fn parses_start_in_a_folder_slot() {
        let Ok(Some(Method::WorkerStartChoice(WorkerStartChoiceParams { start: params, .. }))) =
            parse_worker_args(&args(&[
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
            ]))
        else {
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
    fn parses_events_of_an_owner_after_a_cursor() {
        let Ok(Some(Method::WorkerEvents(params))) = parse_worker_args(&args(&[
            "events",
            "--owner",
            "c-abcd2345",
            "--after",
            "ab12-7",
            "--wait",
            "--limit",
            "5",
            "--snapshot",
        ])) else {
            panic!("events must parse");
        };
        assert_eq!(
            params,
            WorkerEventsParams {
                owner: Some("c-abcd2345".into()),
                owner_pane_id: None,
                after: Some("ab12-7".into()),
                wait: true,
                limit: Some(5),
                snapshot: true,
            }
        );
        for bad in [
            &["events", "--owner", "c-abcd2345", "--limit", "0"][..],
            &["events", "--owner", "c-abcd2345", "--after"],
            &["events", "--owner", "c-abcd2345", "w1"],
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
        // A wait after a cursor is an observer's, so it never names the caller.
        assert!(matches!(
            parse_worker_args(&args(&["wait", "w1", "--attention", "--after=42"])),
            Ok(Some(Method::WorkerWait(WorkerWaitParams {
                after: Some(42),
                caller_pane_id: None,
                ..
            })))
        ));
        // An option a shell passed as one argument, and a second id, are
        // named in the error.
        assert_eq!(
            parse_wait(&args(&["w1", "--attention", "--after 42"])),
            Err("wait does not take \"--after 42\"".into())
        );
        assert_eq!(
            parse_wait(&args(&["w1", "--attention", "42"])),
            Err("wait takes one worker id, not also \"42\"".into())
        );
        for bad in [
            &["wait", "w1", "--after", "42"][..],
            &["wait", "w1", "--attention", "--after="],
            &["wait", "w1", "--attention", "--after=42", "--after", "43"],
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
                caller_pane_id,
                turn: Some(7),
                ..
            }))) if caller_pane_id == caller()
        ));
        assert!(matches!(
            parse_worker_args(&args(&["interrupt", "w1"])),
            Ok(Some(Method::WorkerInterrupt(WorkerInterruptParams {
                caller_pane_id,
                turn: None,
                ..
            }))) if caller_pane_id == caller()
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
        let Some(params) =
            parsed_answer(&["answer", "w1", "--request", "r1", "deny", "--message", "no"])
        else {
            panic!("answer must parse");
        };
        assert_eq!(params.decision, Some(WorkerDecision::Deny));
        assert_eq!(params.message.as_deref(), Some("no"));
        assert!(params.answers.is_empty());

        let Some(params) = parsed_answer(&["answer", "--request", "r1", "w1", "blue.txt", "1,3"])
        else {
            panic!("answer must parse");
        };
        assert_eq!(params.request_id.as_deref(), Some("r1"));
        assert_eq!(params.decision, None);
        assert_eq!(params.answers, vec!["blue.txt", "1,3"]);

        let Some(params) = parsed_answer(&[
            "answer",
            "w1",
            "--command-id",
            "item-3:answer:r1",
            "--request",
            "r1",
            "allow",
        ]) else {
            panic!("answer with a command id must parse");
        };
        assert_eq!(params.command_id.as_deref(), Some("item-3:answer:r1"));
        assert_eq!(params.decision, Some(WorkerDecision::Allow));

        // As a tenure: worker.answer_as, so a server that does not fence
        // refuses the method instead of applying the answer.
        let Ok(Some(Method::WorkerAnswerAs(params))) = parse_worker_args(&args(&[
            "answer",
            "w1",
            "--coordinator",
            "c-abcd2345",
            "--request",
            "r1",
            "allow",
        ])) else {
            panic!("answer as a tenure must parse");
        };
        assert_eq!(params.coordinator_id, "c-abcd2345");
        assert_eq!(params.answer.request_id.as_deref(), Some("r1"));
        assert_eq!(params.answer.decision, Some(WorkerDecision::Allow));
        assert_eq!(
            serde_json::to_value(&params).unwrap(),
            serde_json::json!({
                "coordinator_id": "c-abcd2345",
                "worker_id": "w1",
                "request_id": "r1",
                "decision": "allow",
            })
        );
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
                caller_pane_id: caller(),
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

    #[test]
    fn parses_start_with_an_item_and_runs() {
        let Ok(Some(Method::WorkerStartChoice(WorkerStartChoiceParams { start: params, .. }))) =
            parse_worker_args(&args(&["start", "--item", "t-abcd2345", "do it"]))
        else {
            panic!("start with --item must parse");
        };
        assert_eq!(params.item.as_deref(), Some("t-abcd2345"));
        assert_eq!(
            parse_worker_args(&args(&["runs", "--item", "t-abcd2345", "--repo", "/repo"])),
            Ok(Some(Method::WorkerRuns(WorkerRunsParams {
                item: Some("t-abcd2345".into()),
                repo: Some("/repo".into()),
            })))
        );
        assert_eq!(
            parse_worker_args(&args(&["runs"])),
            Ok(Some(Method::WorkerRuns(WorkerRunsParams::default())))
        );
        for bad in [
            &["runs", "t-abcd2345"][..],
            &["runs", "--item"],
            &["start", "--item"],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_verify() {
        let mut parsed = parse_worker_args(&args(&[
            "verify",
            "w7",
            "--base",
            "abc123",
            "--paths",
            "src/**",
            "AGENTS.md",
            "--message",
            "feat: x",
            "--cmd",
            "cargo nextest run",
            "--generated",
            "docs/a.json=make a",
            "--generated",
            "b=c=d",
        ]));
        // The caller's environment goes along, without HERDR_* variables.
        let Ok(Some(Method::WorkerVerify(params))) = &mut parsed else {
            panic!("{parsed:?}");
        };
        let env = params.env.take().expect("the caller's environment");
        assert_eq!(env.get("PATH"), std::env::var("PATH").ok().as_ref());
        assert!(!env.keys().any(|key| key.starts_with("HERDR_")), "{env:?}");
        assert_eq!(
            parsed,
            Ok(Some(Method::WorkerVerify(WorkerVerifyParams {
                worker_id: "w7".into(),
                base: "abc123".into(),
                expected_message: "feat: x".into(),
                allowed_paths: vec!["src/**".into(), "AGENTS.md".into()],
                command: Some("cargo nextest run".into()),
                generated: vec![
                    WorkerGeneratedFile {
                        path: "docs/a.json".into(),
                        command: "make a".into(),
                    },
                    WorkerGeneratedFile {
                        path: "b".into(),
                        command: "c=d".into(),
                    },
                ],
                env: None,
            })))
        );
        for bad in [
            &["verify", "w7", "--base", "a", "--message", "m"][..],
            &["verify", "w7", "--base", "a", "--message", "m", "--paths"],
            &["verify", "--base", "a", "--message", "m", "--paths", "x"],
            &[
                "verify",
                "w7",
                "--base",
                "a",
                "--message",
                "m",
                "--paths",
                "x",
                "--generated",
                "nope",
            ],
            &[
                "verify",
                "w7",
                "--base",
                "a",
                "--base",
                "b",
                "--message",
                "m",
                "--paths",
                "x",
            ],
            &[
                "verify",
                "w7",
                "--base",
                "a",
                "--message",
                "m",
                "--paths",
                "x",
                "--other",
            ],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_drain() {
        assert_eq!(
            parse_worker_args(&args(&["drain", "start", "--reason", "an install"])),
            Ok(Some(Method::WorkerDrain(WorkerDrainParams {
                action: WorkerDrainAction::Start,
                reason: Some("an install".into()),
            })))
        );
        assert_eq!(
            parse_worker_args(&args(&["drain", "cancel"])),
            Ok(Some(Method::WorkerDrain(WorkerDrainParams {
                action: WorkerDrainAction::Cancel,
                reason: None,
            })))
        );
        for bad in [
            &["drain"][..],
            &["drain", "stop"],
            &["drain", "status", "--reason", "x"],
            &["drain", "start", "cancel"],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_escalate() {
        assert_eq!(
            parse_worker_args(&args(&["escalate", "w12", "--request", "perm-1"])),
            Ok(Some(Method::WorkerEscalate(WorkerEscalateParams {
                caller_pane_id: caller(),
                worker_id: "w12".into(),
                request_id: "perm-1".into(),
            })))
        );
        for bad in [
            &["escalate", "w12"][..],
            &["escalate", "--request", "perm-1"],
            &["escalate", "w12", "w13", "--request", "perm-1"],
            &[
                "escalate",
                "--command-id",
                "c",
                "w12",
                "--request",
                "perm-1",
            ],
        ] {
            assert!(parse_worker_args(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
