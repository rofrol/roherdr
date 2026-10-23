//! `herdr todo`: drives a TODO item from preflight through the cherry-pick
//! onto the checked-out branch, the install, the TODO update, the push and the cleanup
//! (`todo.run`), and lets the coordinator wait on and answer the run's
//! events.

use crate::api::schema::{
    Method, Request, TodoAction, TodoDraftRunParams, TodoGrantParams, TodoGrantsParams,
    TodoNextParams, TodoNextRun, TodoQueueMode, TodoQueueSetParams, TodoQueueTarget,
    TodoResumeParams, TodoReviewParams, TodoRevokeParams, TodoRunParams, TodoRunTarget,
    TodoRunsParams, TodoStopParams, TodoWaitParams, WorkerAgent, WorkerDecision,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr todo run <item-id> --task FILE --message SUBJECT --paths GLOB... --check NAME...
                 [--ignore-usage] [--auto-review] [--auto-answer] [--agent claude|pi|pi-anthropic]
                 [--model MODEL] [--new-api]
      --agent names the agent of the run's workers: claude (Claude Code on
      the subscription), pi (headless pi on the ChatGPT plan) or
      pi-anthropic (headless pi on the Anthropic API credits). Without it
      the run uses [workers] default_agent (default claude); an explicit
      --agent wins. --model MODEL runs a pi worker on MODEL, a
      provider/model id as pi names it (e.g. deepseek/deepseek-flash)
      instead of openai-codex/gpt-6.1-sol; the run pins it for all its
      attempts and its events record it. The usage gate reads the provider
      MODEL belongs to (the ChatGPT plan for openai-codex/*, DeepSeek's
      prepaid balance for deepseek/*), so a run on another provider is
      admitted while the ChatGPT plan is at its limit; a provider herdr has
      no live usage read for is refused and --ignore-usage does not
      override that, and a Claude run keeps Claude Code's own model. A
      pi-anthropic run the credit proxy or the credit cap
      refuses falls back to pi once; its events record the refusal, and
      todo status shows the agent and model that ran.
      Preflight (the item in TODO.md, the folder slot free and clean, the disk
      above the guard threshold, SUBJECT a lowercase conventional subject, the
      paths relative git globs, each NAME registered in .herdr/checks.toml;
      the verify runs them in order and every one must pass; the usage gate:
      usage is read from the worker's provider now. High usage waits until
      every window is below [usage] gate_reopen_percent (default 80).
      Unknown usage waits for a successful read below gate_close_percent
      (default 90); failure never clears a high latch. A refused start prints
      a durable run id; todo wait waits through measurements;
      --ignore-usage, on the user's word, starts anyway; an orchestrator
      outside this driver runs its agents with `herdr worker run`, which
      passes this same gate), then
      start a headless worker in the repository's folder slot
      ../herdr-worktrees/<repository>/worker (see `herdr worker start`) on
      the branch todo/<item-id>-<attempt> from the branch checked out where
      you run this (a detached HEAD refuses; the run stores it and that
      worktree, lands there and pushes it where git's push configuration
      sends it: branch.B.pushRemote, remote.pushDefault or the upstream,
      fast-forward only, never creating a remote branch, and not at all
      when git rev-parse B@{push} names no single destination) with FILE's
      text as its task (followed by SUBJECT, the GLOBs and the WORKER-DONE line it must
      keep), owned by this pane. Prints the run (its id r-...). The run then
      waits for you on its events (todo wait).
      With --agent pi-anthropic the run spends configured Anthropic API credits
      through herdr's authenticated loopback proxy (todo.run_anthropic).
      With --agent pi the run's workers run headless pi (pi --mode rpc) with
      openai-codex/gpt-6.1-sol on your ChatGPT plan instead of Claude, in the
      same folder slot, contract, checks and verify; herdr's own pi extension
      sends every tool call to the worker policy and runs bash in herdr's
      shell sandbox (macOS sandbox-exec: writes only to the slot, its temp
      dir and caches, no network, no credential reads), and a pi without
      that extension is ended before its prompt. The usage gate then reads
      the ChatGPT plan's usage (as Codex reads it) with the same thresholds
      and a gate state of its own. The run's events record the agent and
      model.
      With --auto-review the server reviews each review event itself: one
      claude -p call (structured JSON output, no tools, no session) with
      the item, the task, the worker's last reply, the diff and the checks,
      whose typed decision it applies: approve (bound to the event's commit
      and base), retry (its review as the next attempt's) or escalate (a new
      review event whose error holds the question and its options, and a
      notice to the user). A failed call or invalid output is retried once,
      then escalated. todo wait does not return a review event before the
      server decided it; todo resume still answers it and wins when it
      comes first. The decisions are recorded in the run's events. Before
      the call the server stops the worker and runs the verify (the
      registered checks) on the event's commit, so the model judges real
      results; an approval of that verified commit needs no second verify,
      and one of a commit whose verify failed is escalated.
      With --auto-answer the server answers each question the worker policy
      leaves the same way: one call with the task, the question (or the tool
      call and why the policy left it) and the run's policy, which returns
      allow, deny (a message), answer (an AskUserQuestion's answers) or
      escalate. A request the policy leaves to the user (a classifier
      escalation, a path outside the worker's folders, a tool herdr does not
      know) is only escalated, without a call. Every escalation, of a review
      or of a question, also enters the user's ? list with the run, the item
      and the question, answerable from its dialog. Both flags pass to a run
      a close starts with --next. While a decision cannot be recorded (the
      worker store refuses the write), todo wait and todo status show the run
      blocked with that error, and the server writes it again once the store
      takes a write.
      With --new-api the run records that its commit adds an API the base
      cannot build against: when the repository configures [counterfactual]
      in .herdr/checks.toml, the verify's counterfactual check accepts a base
      that does not build with the commit's tests (base_unbuildable) instead
      of failing, and records it as not proven. It relaxes nothing else: a
      test that passes on the base still fails the verify.
  herdr todo run <item-id> --draft [--ignore-usage] [--agent claude|pi|pi-anthropic]
                 [--model MODEL]
      The server drafts the task, SUBJECT, the GLOBs and the check NAMEs
      itself (todo.draft_run): one claude -p call (structured JSON output, no
      tools, no session) with the item's text, the DECISIONS.md sections it
      names, AGENTS.md's code, test and commit rules, .herdr/checks.toml, git
      log -20 of the default branch and the item's history. The server checks the draft
      as todo run checks those flags (only registered checks), records it and
      starts the run with it; an invalid draft or a failed call is asked once
      more, then escalated: a draft event whose error holds the question,
      also in the user's ? list. The user's answer there, or todo resume
      --action retry --task FILE with the answer, drafts again with it.
      Implies --auto-review and --auto-answer. Preflight checks the rest
      before the call. todo review shows the draft.
  herdr todo wait <run-id> [--after EVENT_ID]
      Blocks until the run waits on an event after EVENT_ID (a question the
      worker policy left, the worker's turn end, a failed verify), or ended
      (done, blocked or aborted), or a still_alive event todo status raised.
      Prints the event with its event_id, the actions it takes and its
      evidence (questions, diff stat, commits, the verify). When the server
      hands off meanwhile (a live handoff), it answers server_handed_off once
      the new server accepts, and the wait goes on with the new server on the
      same run after EVENT_ID; it gives up when the new server does not know
      the run. A server that stops or restarts ends the wait.
  herdr todo resume <run-id> --event EVENT_ID
                    --action approve|retry|answer|verify|force-stop|
                             retry-install|skip-install|retry-todo|skip-todo|
                             retry-push|approve-landing|abort
                    [--task FILE [--ignore-usage]]
                    [--keep-open | (--note FILE | --close FILE)
                     [--next ITEM --next-task FILE
                       --next-message SUBJECT --next-paths GLOB...
                       [--next-check NAME...] [--next-agent claude|pi|pi-anthropic]
                       [--next-model MODEL]
                       | --stop-reason TEXT]]
                    [--request REQUEST_ID] [--message TEXT]
                    [allow|deny|<choice>...]
      Answers the pending event; any other EVENT_ID is refused as stale.
      --close needs --next or --stop-reason, unless queue mode is on (todo
      queue), where it takes neither: the queue starts the next item.
      --note takes either of them the same way, or neither (a noted item
      stays open), and in queue mode neither. In
      queue mode (a run the queue started, or any run while the queue is on)
      approve without --note or --close closes the item as the queue's own
      review does (a section of DECISIONS.md), so the queue never runs it
      again; --keep-open leaves it open. A commit that changes a guarded
      path (the checks of .herdr/checks.toml, the review's
      .herdr/review-checklist.md or a case of .herdr/review-golden/, and
      whatever the repository's [review] guarded_paths adds) is approved
      only with --accept-guarded-paths, which says you read those files;
      the verify reports them (guarded_paths) and never fails for them, a
      guard that cannot be read refuses the approval, and no automatic
      approval passes one. With --next, once this run is
      done the driver starts ITEM's run itself (as todo run with those
      parameters, this run's checks unless --next-check names others, owned
      by this run's owner); a refused start (preflight, usage gate) is a
      next_refused event on this run, and todo status shows next_run_id or
      next_refusal. todo wait on the run returns only once the next run
      started or was refused (then next_refused is its result); a refusal
      also notifies the user, and one for a short disk first runs the
      folder slot's target sweep and preflight once more. With --stop-reason, TEXT goes into the item's history
      when the run is done and to the user as a notification.
      approve stops the worker, verifies its commit with the checks,
      cherry-picks it onto the run's branch with the trailers Herdr-Item: <item-id> and
      Herdr-Run: <run-id>/<attempt> (only when the branch is still at the
      commit the event showed; otherwise a new review event), runs the
      [install] of .herdr/checks.toml,
      appends --note FILE's lines to the item in TODO.md (or, with --close
      FILE, removes the item and adds FILE as a section of DECISIONS.md)
      with herdr's own TODO editor (todo edit) and commits that by path,
      pushes that branch to
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
      retry-push. In the repository rooted at the home directory the
      cherry-pick first waits on a landing_approval event with the whole
      patch and a backup and rollback note: its owner answers
      approve-landing from a pane that coordinates no repository (not the
      coordinator's), and only that commit lands and installs.
      abort answers any event the run waits on, or the blocked
      event of a blocked run: the run stops its worker when it still runs,
      waits for its exit and ends aborted, with --message TEXT as the reason;
      its branches and commits (a commit already on its branch too) stay as they
      are and the aborted event names them. Every resume sends this shell's environment again, which the
      checks, the install and the push run with (the server never stores
      it); without it a check is unavailable and an install or push fails.
  herdr todo status <run-id>
      Prints the run. Asked while its worker has not exited since the stop,
      it raises a still_alive event first, which takes force-stop.
      step_started_ms, step_command and step_log say what the driver does
      at its step now: when it entered it, the command it runs there while
      it runs, and that command's log.
  herdr todo log [--follow] <run-id>
      Prints the log of the run's current step (step_log); --follow keeps
      printing what is appended until q, Esc or Ctrl-C. Read-only.
  herdr todo ack-stop <run-id>
      Acknowledges the stop reason of a run done with --stop-reason: the
      sidebar no longer lists the run under its coordinator's space, and
      no notification of it follows. Prints the run.
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
      item, and no next one starts.
  herdr todo queue on|pause|status [--repo DIR] [--reason TEXT]
                   [--agent claude|pi|pi-anthropic] [--model PROVIDER/MODEL]
      Queue mode: herdr's server drives the queue of DIR's (default: the
      current directory's) repository. on (kept across restarts): whenever
      no run of the repository is active, no escalation is pending and the
      usage gate admits, the server starts the top item of \"Next, in
      order\" (TODO.md as the default branch has it) that is not blocked as todo run
      --draft would, owned by this pane, its checks, install and push with
      this shell's environment (kept in memory only; after a cold restart the
      queue pauses with \"environment needed\" and a notification until on is
      run again from a shell), its agent and pi model from on's --agent and
      --model as todo run --draft takes them (without them [workers]
      default_agent and its defaults; on again replaces them). It looks
      again only on its own events: a run ended, an escalation was answered,
      the server started, the mode was set, and, while the usage gate stops
      it, the usage poller published a new reading; never on a timer. An approval of a queue run (its own
      review's, or todo resume's without --keep-open) closes its item. An
      item the queue ran 2 times with the
      same text is blocked until its text changes; 3 consecutive queue runs
      that ended blocked or aborted pause the queue (the circuit breaker),
      with a notification. on also clears such a pause. pause [--reason
      TEXT] lets the run in progress finish and starts nothing more. status
      prints the mode and the status with its reason: running,
      waiting_on_user, escalation_pending, usage_gate, blocked or empty.
  herdr todo grant --operation prepare --hash HASH [--repo DIR]
      Grant the [prepare] operation that the default branch of DIR's (default: the
      current directory's) repository declares in .herdr/operations.toml:
      its argv and requested capabilities, exactly the definition whose
      hash todo run's grant_required refusal showed (refused when it changed
      since). The grant is the user's decision: it asks for `yes` on a
      terminal and is refused without one. A changed definition has another
      hash and needs its own grant. A refused run also lists the definition
      as a question in the user's ? list (the operation, its capabilities,
      its hash and the repository), answered only with a click in the
      user's own client, never from an agent pane; once granted, the
      coordinator starts the run again (in queue mode the queue does).
  herdr todo cancel-pending <run-id>
      Cancel a start still waiting for usage (also available in its ? entry).
      Repeating cancellation is idempotent; wait observes the aborted event.
  herdr todo grant --revoke --operation prepare [--hash HASH] [--repo DIR]
      Remove the stored grant of that definition hash (every grant of the
      operation without --hash), recorded in the worker store; a run that
      has not run its prepare yet refuses it.
  herdr todo grants [--repo DIR]
      List the stored grants (of DIR's repository with --repo).
  herdr todo review-eval [--case NAME]... [--without-context-pack]
                         [--without-checklist] [--credits] [--repo DIR] [--out FILE]
      Replay the repository's golden set (.herdr/review-golden/, catches
      made after the checklist's cut-off) blind against the automatic
      review: each case's patch in a temporary worktree at its base, the
      review's input without a verify, one model call through claude -p (or
      the credit proxy with the review tools, --credits) with [review]
      model, findings scored mechanically against the case's expected
      entries (the same file, within 15 lines). The flags add ablation arms
      beside the full input. Prints the summary on stderr and the report,
      every call recorded, to FILE or stdout. Spends model calls; needs no
      server.
  herdr todo edit [--file FILE] <command> [ARGS...]
      Herdr's own TODO editor, the scripts/todo_edit.py herdr was built
      with, embedded in the binary: verified edits of TODO.md (or FILE, such
      as DECISIONS.md) in the current directory (find, ids, add, append-to,
      insert-after, remove, move, add-section; todo edit --help lists them).
      It passes the arguments and stdin through and exits with the editor's
      status. todo run's TODO step uses the same editor, so it works in any
      repository, with or without a scripts/todo_edit.py of its own. Needs
      python3, not the server.";

pub(super) fn run_todo_command(args: &[String]) -> std::io::Result<i32> {
    if args.first().map(String::as_str) == Some("edit") {
        return Ok(run_todo_edit(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("review-eval") {
        return Ok(run_review_eval(&args[1..]));
    }
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
    if let Method::TodoGrant(params) = &method {
        if let Err(refusal) = confirm_grant(params) {
            eprintln!("{refusal}");
            return Ok(1);
        }
    }
    let request = Request {
        id: format!("cli:todo:{}", args[0]),
        method,
    };
    if let Method::TodoReview(_) = &request.method {
        let json = args.iter().any(|arg| arg == "--json");
        return super::todo_review::print_review(&super::send_request(&request)?, json);
    }
    if args.first().map(String::as_str) == Some("log") {
        let follow = args.iter().any(|arg| arg == "--follow");
        return print_step_log(&super::send_request(&request)?, follow);
    }
    let command = super::report::command_line(&["herdr", "todo"], args);
    if matches!(request.method, Method::TodoWait(_)) {
        return wait_across_handoffs(&request, &command);
    }
    let response = super::send_request(&request)?;
    let code = super::print_response(&response)?;
    super::report::hint_reply(&response, &command);
    if let Method::TodoRunWait(params) = &request.method {
        if let Some(refusal) = auto_review_ignored(params, &response) {
            eprintln!("{refusal}");
            return Ok(1);
        }
    }
    Ok(code)
}

/// The options of `herdr todo review-eval`, and the repository and output
/// file it names.
fn parse_review_eval(
    args: &[String],
) -> Result<(crate::workers::EvalOptions, Option<String>, Option<String>), String> {
    let mut options = crate::workers::EvalOptions::default();
    let (repo, rest) = take_string_option(args, "--repo")?;
    let (out, rest) = take_string_option(&rest, "--out")?;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--case" => options
                .cases
                .push(words.next().ok_or("missing value for --case")?.to_owned()),
            "--without-context-pack" => options.without_context_pack = true,
            "--without-checklist" => options.without_checklist = true,
            "--credits" => options.credits = true,
            other => match other.strip_prefix("--case=") {
                Some(name) => options.cases.push(name.to_owned()),
                None => return Err(format!("review-eval does not take {other}")),
            },
        }
    }
    Ok((options, repo, out))
}

/// `herdr todo review-eval`: replays the repository's golden set of past
/// review catches with the automatic review's own model call, prints the
/// summary on stderr and the report (every call) to `--out` or stdout.
/// Needs no server.
fn run_review_eval(args: &[String]) -> i32 {
    let (options, repo, out) = match parse_review_eval(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let dir = repo.map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
    let toplevel = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    let repo = match toplevel {
        Ok(output) if output.status.success() => {
            std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        }
        _ => {
            eprintln!("{} is not in a git repository", dir.display());
            return 1;
        }
    };
    let (report, summary) = match crate::workers::review_eval(&repo, &options) {
        Ok(done) => done,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    eprintln!("{summary}");
    let text = match serde_json::to_string_pretty(&report) {
        Ok(text) => text + "\n",
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    match out {
        Some(path) => {
            if let Err(error) = std::fs::write(&path, text) {
                eprintln!("cannot write {path}: {error}");
                return 1;
            }
        }
        None => print!("{text}"),
    }
    0
}

/// `herdr todo edit`: runs herdr's own `todo_edit.py` (embedded in the
/// binary) in the current directory with `args`, stdin and stdout passed
/// through, and returns its exit status. Needs no server.
fn run_todo_edit(args: &[String]) -> i32 {
    let status = with_todo_editor(|script| {
        std::process::Command::new("python3")
            .arg(script)
            .args(args)
            .status()
            .map_err(|error| format!("cannot run python3: {error}"))
    });
    match status {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// Calls `run` with the path of herdr's `todo_edit.py`, written for it into
/// a fresh directory of its own (Python puts a script's directory first on
/// its import path), and removes that directory afterwards.
fn with_todo_editor<T>(
    run: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("herdr-todo-edit-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let result = crate::workers::todo_editor::write_script(&dir).and_then(|script| run(&script));
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Prints the log `todo.status` names for the run's current step, and
/// with `follow` what is appended to it, its control characters shown as
/// spaces (a log is not a terminal).
fn print_step_log(response: &serde_json::Value, follow: bool) -> std::io::Result<i32> {
    let run = &response["result"]["run"];
    let Some(path) = run["step_log"].as_str() else {
        if response.get("error").is_some() {
            return super::print_response(response);
        }
        eprintln!(
            "run {} has no log at its step ({})",
            run["run_id"].as_str().unwrap_or("?"),
            run["step"].as_str().unwrap_or("?")
        );
        return Ok(1);
    };
    let title = format!(
        "run {} · {} · {}",
        run["run_id"].as_str().unwrap_or("?"),
        run["step"].as_str().unwrap_or("?"),
        run["step_command"].as_str().unwrap_or(path)
    );
    let file = std::fs::File::open(path)?;
    if follow {
        return super::worker::follow_lines(&title, file, |line| vec![log_line(line)]);
    }
    println!("{title}");
    for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
        println!("{}", log_line(&line?));
    }
    Ok(0)
}

/// A log line as the log view shows it: control characters (escape
/// sequences' starts, carriage returns) as spaces.
fn log_line(line: &str) -> String {
    line.chars()
        .map(|character| {
            if character.is_control() && character != '\t' {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// The user's own confirmation of a grant, on a terminal: an agent's
/// command has none, so it cannot grant by running the command.
fn confirm_grant(params: &TodoGrantParams) -> Result<(), String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(
            "a grant is the user's decision: run `herdr todo grant` in your own \
                    terminal, where it asks for confirmation"
                .into(),
        );
    }
    print!(
        "Grant [{}] of {} with definition hash {}? Type yes: ",
        params.operation, params.cwd, params.hash
    );
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err("not granted".into())
    }
}

/// A server older than `--auto-review` or `--auto-answer` ignores it and
/// starts the run anyway: its reply has no `auto_review` or `auto_answer`.
fn auto_review_ignored(params: &TodoRunParams, response: &serde_json::Value) -> Option<String> {
    let run = &response["result"]["run"];
    if !run.is_object() {
        return None;
    }
    let run_id = run["run_id"].as_str().unwrap_or("?");
    if params.auto_review && run["auto_review"] != true {
        return Some(format!(
            "this server ignored --auto-review (it predates it): run {run_id} waits for your \
             review of each event"
        ));
    }
    (params.auto_answer && run["auto_answer"] != true).then(|| {
        format!(
            "this server ignored --auto-answer (it predates it): run {run_id} waits for your \
             answer to each question"
        )
    })
}

/// `todo wait`, which a live handoff does not end: the handed-off server
/// tells the wait so, and it goes on with the new one ([`super::reconnect`]).
/// When the new server's answer is an error, the wait gave up: it prints
/// the `herdr report` command for that, as for an event without a next step.
fn wait_across_handoffs(request: &Request, command: &str) -> std::io::Result<i32> {
    let target = match &request.method {
        Method::TodoWait(params) => format!("run {}", params.run_id),
        _ => "the run".into(),
    };
    let reply = super::reconnect::wait("todo wait", &target, |_| request.clone())?;
    let response = reply.response;
    let code = super::print_response(&response)?;
    match &response["error"] {
        serde_json::Value::Null => super::report::hint_reply(&response, command),
        error if reply.reconnected => super::report::print_hint(
            "todo-wait-gave-up",
            &format!(
                "todo wait gave up after a server handoff: the new server answered {}: {}",
                error["code"].as_str().unwrap_or("?"),
                error["message"].as_str().unwrap_or_default()
            ),
            command,
        ),
        _ => {}
    }
    Ok(code)
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
/// `--agent`'s value: the agent CLI a run's workers run.
fn parse_agent(value: &str) -> Result<WorkerAgent, String> {
    match value {
        "claude" => Ok(WorkerAgent::Claude),
        "pi" => Ok(WorkerAgent::Pi),
        other => Err(format!("--agent takes claude or pi, not {other}")),
    }
}

/// An agent choice by name, for `option` (`--agent`, `--next-agent`).
pub(super) fn parse_agent_choice(
    option: &str,
    value: &str,
) -> Result<crate::api::schema::WorkerAgentChoice, String> {
    crate::api::schema::WorkerAgentChoice::parse(value)
        .ok_or_else(|| format!("{option} takes claude, pi or pi-anthropic, not {value}"))
}

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

/// What `resume --close` or `--note` names after the approval: the next
/// item's run (`--next ITEM` with `--next-task`, `--next-message` and
/// `--next-paths`, and optionally `--next-check`, else this run's checks)
/// or `--stop-reason`; at most one of them, only with `--close` or
/// `--note`. A close with neither is the server's to refuse unless the
/// repository's queue mode is on; a note with neither is valid.
fn next_run(
    close_or_note: bool,
    item: Option<String>,
    task: Option<String>,
    message: Option<String>,
    paths: Option<Vec<String>>,
    checks: Option<Vec<String>>,
    agent: Option<crate::api::schema::WorkerAgentChoice>,
    model: Option<String>,
    stop_reason: Option<&str>,
) -> Result<Option<TodoNextRun>, String> {
    let any_next = item.is_some()
        || task.is_some()
        || message.is_some()
        || paths.is_some()
        || checks.is_some()
        || agent.is_some()
        || model.is_some();
    if !close_or_note && (any_next || stop_reason.is_some()) {
        return Err("only --close or --note takes --next and --stop-reason".into());
    }
    if any_next && stop_reason.is_some() {
        return Err("--next and --stop-reason exclude each other".into());
    }
    if !any_next {
        return Ok(None);
    }
    let (Some(item), Some(task), Some(message), Some(paths)) = (item, task, message, paths) else {
        return Err(
            "--next takes the item id with --next-task FILE, --next-message SUBJECT \
                    and --next-paths GLOB... (--next-check NAME..., --next-agent and \
                    --next-model are optional)"
                .into(),
        );
    };
    if model.is_some()
        && matches!(
            agent,
            Some(
                crate::api::schema::WorkerAgentChoice::Claude
                    | crate::api::schema::WorkerAgentChoice::PiAnthropic
            )
        )
    {
        return Err(
            "--next-model is for a pi chain; a Claude or pi-anthropic next run takes its own \
             model"
                .into(),
        );
    }
    Ok(Some(TodoNextRun {
        item,
        task: read_text("--next-task", &task)?,
        message,
        paths,
        checks: checks.unwrap_or_default(),
        agent,
        model,
    }))
}

/// The agent, the model and the other args [`take_agent_and_model`] gives.
type AgentAndModel = (
    Option<crate::api::schema::WorkerAgentChoice>,
    Option<String>,
    Vec<String>,
);

/// `--agent <claude|pi|pi-anthropic>` and `--model <provider/model>` (a pi
/// model, refused with an agent that takes its own) out of `args`, for
/// `run --draft` and `queue on`.
fn take_agent_and_model(args: &[String]) -> Result<AgentAndModel, String> {
    let (agent, rest) = take_string_option(args, "--agent")?;
    let (model, rest) = take_string_option(&rest, "--model")?;
    let agent = agent
        .as_deref()
        .map(|value| parse_agent_choice("--agent", value))
        .transpose()?;
    if model.is_some() && agent == Some(crate::api::schema::WorkerAgentChoice::PiAnthropic) {
        return Err(
            "--model is for a gated pi run; --agent pi-anthropic takes its model from \
             [workers] anthropic_api_model"
                .into(),
        );
    }
    if model.is_some() && agent == Some(crate::api::schema::WorkerAgentChoice::Claude) {
        return Err(
            "--model names the pi worker's model; a Claude run keeps Claude Code's own model"
                .into(),
        );
    }
    Ok((agent, model, rest))
}

/// `todo run <item> --draft`: the item only (`--auto-review` and
/// `--auto-answer` are implied, and allowed); the server drafts the rest.
fn parse_draft_run(rest: &[String], ignore_usage: bool) -> Result<Method, String> {
    let (_, rest) = take_switch(rest, "--auto-review")?;
    let (_, rest) = take_switch(&rest, "--auto-answer")?;
    let (agent, model, rest) = take_agent_and_model(&rest)?;
    let item = match rest.as_slice() {
        [item] if !item.starts_with("--") => item.clone(),
        _ => {
            return Err(
                "run --draft takes one item id and no --task, --message, --paths or --check: \
                 the server drafts them"
                    .into(),
            )
        }
    };
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let pane = super::target::caller_pane_id();
    Ok(Method::TodoDraftRun(TodoDraftRunParams {
        cwd: std::path::absolute(&cwd)
            .map_err(|error| error.to_string())?
            .display()
            .to_string(),
        item,
        owner_session_id: caller_session(pane.as_deref()),
        owner_pane_id: pane,
        workspace_id: super::target::caller_workspace_id(),
        env: super::worker::caller_env(),
        ignore_usage,
        agent,
        model,
    }))
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
            let (draft, rest) = take_switch(&rest, "--draft")?;
            if draft {
                return parse_draft_run(&rest, ignore_usage).map(Some);
            }
            let (new_api, rest) = take_switch(&rest, "--new-api")?;
            let (auto_review, rest) = take_switch(&rest, "--auto-review")?;
            let (auto_answer, rest) = take_switch(&rest, "--auto-answer")?;
            let (agent, rest) = take_string_option(&rest, "--agent")?;
            let (model, rest) = take_string_option(&rest, "--model")?;
            let credits = agent.as_deref() == Some("pi-anthropic");
            let agent = if credits {
                Some(WorkerAgent::Pi)
            } else {
                agent.as_deref().map(parse_agent).transpose()?
            };
            if credits && model.is_some() {
                return Err(
                    "--model is for a gated pi run; --agent pi-anthropic takes its model from \
                     [workers] anthropic_api_model"
                        .into(),
                );
            }
            if model.is_some() && agent == Some(WorkerAgent::Claude) {
                return Err(
                    "--model names the pi worker's model; a Claude run keeps Claude Code's own \
                     model"
                        .into(),
                );
            }
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
            let params = TodoRunParams {
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
                agent,
                model,
                auto_review,
                auto_answer,
                new_api,
            };
            if credits {
                Method::TodoRunAnthropic(params)
            } else {
                Method::TodoRunWait(params)
            }
        }
        "cancel-pending" => {
            Method::TodoCancelPending(crate::api::schema::TodoCancelPendingParams {
                run_id: one_run_id("cancel-pending", rest)?,
                caller_pane_id: super::target::caller_pane_id(),
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
                waiter_pane: super::target::caller_pane_id(),
            })
        }
        "resume" => {
            let (ignore_usage, rest) = take_switch(rest, "--ignore-usage")?;
            let (keep_open, rest) = take_switch(&rest, "--keep-open")?;
            let (accept_guarded_paths, rest) = take_switch(&rest, "--accept-guarded-paths")?;
            let (event, rest) = take_string_option(&rest, "--event")?;
            let (action, rest) = take_string_option(&rest, "--action")?;
            let (task, rest) = take_string_option(&rest, "--task")?;
            let (request_id, rest) = take_string_option(&rest, "--request")?;
            let (message, rest) = take_string_option(&rest, "--message")?;
            let (note, rest) = take_string_option(&rest, "--note")?;
            let (close, rest) = take_string_option(&rest, "--close")?;
            let (next_paths, rest) = take_list(&rest, "--next-paths")?;
            let (next_checks, rest) = take_list(&rest, "--next-check")?;
            let (next_task, rest) = take_string_option(&rest, "--next-task")?;
            let (next_message, rest) = take_string_option(&rest, "--next-message")?;
            let (next_item, rest) = take_string_option(&rest, "--next")?;
            let (next_agent, rest) = take_string_option(&rest, "--next-agent")?;
            let next_agent = next_agent
                .as_deref()
                .map(|value| parse_agent_choice("--next-agent", value))
                .transpose()?;
            let (next_model, rest) = take_string_option(&rest, "--next-model")?;
            let (stop_reason, rest) = take_string_option(&rest, "--stop-reason")?;
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
                Some("approve-landing") => TodoAction::ApproveLanding,
                Some("abort") => TodoAction::Abort,
                _ => {
                    return Err("resume takes --action approve, retry, answer, verify, \
                         force-stop, retry-install, skip-install, retry-todo, skip-todo, \
                         retry-push, approve-landing or abort"
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
            if keep_open && action != TodoAction::Approve {
                return Err("only --action approve takes --keep-open".into());
            }
            if keep_open && close.is_some() {
                return Err("--keep-open and --close exclude each other".into());
            }
            if accept_guarded_paths && action != TodoAction::Approve {
                return Err("only --action approve takes --accept-guarded-paths".into());
            }
            let next = next_run(
                close.is_some() || note.is_some(),
                next_item,
                next_task,
                next_message,
                next_paths,
                next_checks,
                next_agent,
                next_model,
                stop_reason.as_deref(),
            )?;
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
                next,
                stop_reason,
                caller_session_id: caller_session(pane.as_deref()),
                caller_pane_id: pane,
                caller_workspace_id: super::target::caller_workspace_id(),
                ignore_usage,
                keep_open,
                accept_guarded_paths,
            })
        }
        "status" => Method::TodoStatus(TodoRunTarget {
            run_id: one_run_id("status", rest)?,
        }),
        "ack-stop" => Method::TodoAckStop(TodoRunTarget {
            run_id: one_run_id("ack-stop", rest)?,
        }),
        // The status names the step's log, which the CLI reads itself.
        "log" => {
            let (_, rest) = take_switch(rest, "--follow")?;
            Method::TodoStatus(TodoRunTarget {
                run_id: one_run_id("log", &rest)?,
            })
        }
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
                Method::TodoStop(TodoStopParams {
                    caller_pane_id: super::target::caller_pane_id(),
                    cwd,
                })
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
        "queue" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            let (reason, rest) = take_string_option(&rest, "--reason")?;
            let (agent, model, rest) = take_agent_and_model(&rest)?;
            let action = match rest.as_slice() {
                [action] => action.as_str(),
                _ => return Err("queue takes on, pause or status, and --repo".into()),
            };
            if reason.is_some() && action != "pause" {
                return Err("only queue pause takes --reason".into());
            }
            if (agent.is_some() || model.is_some()) && action != "on" {
                return Err("only queue on takes --agent and --model".into());
            }
            let dir = match repo {
                Some(dir) => std::path::PathBuf::from(dir),
                None => std::env::current_dir().map_err(|error| error.to_string())?,
            };
            let cwd = std::path::absolute(&dir)
                .map_err(|error| format!("--repo {}: {error}", dir.display()))?
                .display()
                .to_string();
            let set = |mode, env, reason| {
                let pane = super::target::caller_pane_id();
                Method::TodoQueueSet(TodoQueueSetParams {
                    cwd: cwd.clone(),
                    mode,
                    reason,
                    owner_session_id: caller_session(pane.as_deref()),
                    owner_pane_id: pane,
                    workspace_id: super::target::caller_workspace_id(),
                    env,
                    agent,
                    model: model.clone(),
                })
            };
            match action {
                "on" => set(TodoQueueMode::On, super::worker::caller_env(), None),
                "pause" => set(TodoQueueMode::Paused, None, reason),
                "status" => Method::TodoQueueStatus(TodoQueueTarget { cwd }),
                other => return Err(format!("queue takes on, pause or status, not {other}")),
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
        "grant" | "grants" => {
            let (repo, rest) = take_string_option(rest, "--repo")?;
            let dir = match repo {
                Some(dir) => std::path::PathBuf::from(dir),
                None => std::env::current_dir().map_err(|error| error.to_string())?,
            };
            let cwd = std::path::absolute(&dir)
                .map_err(|error| format!("--repo {}: {error}", dir.display()))?
                .display()
                .to_string();
            if subcommand == "grants" {
                if !rest.is_empty() {
                    return Err("grants takes only --repo".into());
                }
                Method::TodoGrants(TodoGrantsParams { cwd: Some(cwd) })
            } else {
                let (revoke, rest) = take_switch(&rest, "--revoke")?;
                let (operation, rest) = take_string_option(&rest, "--operation")?;
                let (hash, rest) = take_string_option(&rest, "--hash")?;
                if revoke {
                    let (Some(operation), true) = (operation, rest.is_empty()) else {
                        return Err("grant --revoke takes --operation, --hash and --repo".into());
                    };
                    Method::TodoRevoke(TodoRevokeParams {
                        caller_pane_id: super::target::caller_pane_id(),
                        cwd,
                        operation,
                        hash,
                    })
                } else {
                    let (Some(operation), Some(hash), true) = (operation, hash, rest.is_empty())
                    else {
                        return Err("grant takes --operation, --hash and --repo".into());
                    };
                    Method::TodoGrant(TodoGrantParams {
                        caller_pane_id: super::target::caller_pane_id(),
                        cwd,
                        operation,
                        hash,
                    })
                }
            }
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

    /// `todo edit`'s editor run in `dir` with `words`.
    fn todo_edit_in(dir: &std::path::Path, words: &[&str]) -> std::process::Output {
        with_todo_editor(|script| {
            std::process::Command::new("python3")
                .arg(script)
                .args(words)
                .current_dir(dir)
                .output()
                .map_err(|error| error.to_string())
        })
        .unwrap()
    }

    /// `todo edit` finds and edits items in a repository that has no
    /// `scripts/` of its own, and removes the editor's directory after.
    #[test]
    fn todo_edit_runs_herdrs_editor_without_the_repository_script() {
        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!dir.join("scripts").exists());
        std::fs::write(
            dir.join("TODO.md"),
            "# TODO\n\n## Next, in order\n\n- [ ] The item [t-abcd2345]\n  Its text.\n",
        )
        .unwrap();

        let found = todo_edit_in(&dir, &["find", "The item"]);
        assert!(found.status.success(), "{found:?}");
        assert!(
            String::from_utf8_lossy(&found.stdout).contains("t-abcd2345"),
            "{found:?}"
        );

        std::fs::write(dir.join("note.md"), "  A note.\n").unwrap();
        let appended = todo_edit_in(&dir, &["append-to", "t-abcd2345", "--text-file", "note.md"]);
        assert!(appended.status.success(), "{appended:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("TODO.md")).unwrap(),
            "# TODO\n\n## Next, in order\n\n- [ ] The item [t-abcd2345]\n  Its text.\n  A note.\n"
        );

        let missing = todo_edit_in(&dir, &["find", "No such item"]);
        assert!(!missing.status.success(), "{missing:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn todo_edit_passes_its_exit_status_through() {
        assert_eq!(
            run_todo_edit(&args(&["--file", "/nonexistent/TODO.md", "find", "x"])),
            1
        );
    }

    #[test]
    fn review_eval_takes_cases_ablations_the_repository_and_the_output() {
        let (options, repo, out) = parse_review_eval(&args(&[
            "--case",
            "r-a-a1",
            "--case=r-b-a2",
            "--without-context-pack",
            "--without-checklist",
            "--credits",
            "--repo",
            "/repo",
            "--out=report.json",
        ]))
        .unwrap();
        assert_eq!(options.cases, ["r-a-a1", "r-b-a2"]);
        assert!(options.without_context_pack && options.without_checklist && options.credits);
        assert_eq!(repo.as_deref(), Some("/repo"));
        assert_eq!(out.as_deref(), Some("report.json"));
        let (options, repo, out) = parse_review_eval(&[]).unwrap();
        assert!(options.cases.is_empty() && !options.credits && repo.is_none() && out.is_none());
        assert!(parse_review_eval(&args(&["--case"])).is_err());
        assert!(parse_review_eval(&args(&["--diff"])).is_err());
    }

    #[test]
    fn grant_takes_the_operation_and_its_hash() {
        let Ok(Some(Method::TodoGrant(grant))) = parse(&args(&[
            "grant",
            "--operation",
            "prepare",
            "--hash",
            "abc",
            "--repo",
            "/repo",
        ])) else {
            panic!("grant did not parse");
        };
        assert_eq!(
            (
                grant.cwd.as_str(),
                grant.operation.as_str(),
                grant.hash.as_str()
            ),
            ("/repo", "prepare", "abc")
        );
        assert!(parse(&args(&["grant", "--operation", "prepare"])).is_err());
        let Ok(Some(Method::TodoGrants(grants))) = parse(&args(&["grants", "--repo", "/repo"]))
        else {
            panic!("grants did not parse");
        };
        assert_eq!(grants.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn grant_revoke_takes_the_operation_and_an_optional_hash() {
        let Ok(Some(Method::TodoRevoke(revoke))) = parse(&args(&[
            "grant",
            "--revoke",
            "--operation",
            "prepare",
            "--repo",
            "/repo",
        ])) else {
            panic!("grant --revoke did not parse");
        };
        assert_eq!(
            (revoke.cwd.as_str(), revoke.operation.as_str(), revoke.hash),
            ("/repo", "prepare", None)
        );
        let Ok(Some(Method::TodoRevoke(revoke))) = parse(&args(&[
            "grant",
            "--operation",
            "prepare",
            "--hash",
            "abc",
            "--revoke",
            "--repo",
            "/repo",
        ])) else {
            panic!("grant --revoke --hash did not parse");
        };
        assert_eq!(revoke.hash.as_deref(), Some("abc"));
        assert!(parse(&args(&["grant", "--revoke", "--repo", "/repo"])).is_err());
    }

    #[test]
    fn credit_agent_selects_a_new_method_without_changing_the_pi_wire_value() {
        let task = concat!(env!("CARGO_MANIFEST_DIR"), "/src/workers/pi.rs");
        let Ok(Some(Method::TodoRunAnthropic(params))) = parse(&args(&[
            "run",
            "t-example",
            "--agent",
            "pi-anthropic",
            "--task",
            task,
            "--message",
            "feat: test",
            "--paths",
            "src/**",
            "--check",
            "tests",
        ])) else {
            panic!("credit worker command did not parse");
        };
        assert_eq!(params.agent, Some(WorkerAgent::Pi));
        let wire = serde_json::to_value(Method::TodoRunAnthropic(params)).unwrap();
        assert_eq!(wire["method"], "todo.run_anthropic");
        assert_eq!(wire["params"]["agent"], "pi");
    }

    #[test]
    fn run_draft_takes_the_item_only() {
        let Ok(Some(Method::TodoDraftRun(params))) =
            parse(&args(&["run", "t-abcd2345", "--draft", "--auto-review"]))
        else {
            panic!("run --draft did not parse");
        };
        assert_eq!(params.item, "t-abcd2345");
        assert!(!params.ignore_usage);
        let Ok(Some(Method::TodoDraftRun(params))) =
            parse(&args(&["run", "--draft", "--ignore-usage", "t-abcd2345"]))
        else {
            panic!("run --draft --ignore-usage did not parse");
        };
        assert!(params.ignore_usage);
        for extra in [
            &["--message", "feat: x"][..],
            &["--paths", "src/**"],
            &["--check", "workers"],
            &["--task", "task.md"],
        ] {
            let mut words = vec!["run", "t-abcd2345", "--draft"];
            words.extend_from_slice(extra);
            let refused = parse(&args(&words)).unwrap_err();
            assert!(refused.contains("the server drafts them"), "{refused}");
        }
    }

    #[test]
    fn run_takes_the_paths_up_to_the_next_option_and_reads_the_task() {
        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let task = dir.join("task.md");
        std::fs::write(&task, "Do the thing\n").unwrap();
        let task = task.display().to_string();
        let Ok(Some(Method::TodoRunWait(params))) = parse(&args(&[
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
        let Ok(Some(Method::TodoRunWait(params))) = parse(&args(&[
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
        assert!(!params.auto_review);
        assert_eq!(params.paths, ["src/**"]);
        assert_eq!(params.agent, None);
        let with_agent = |agent: &str| {
            parse(&args(&[
                "run",
                "t-abcd2345",
                "--agent",
                agent,
                "--task",
                &task,
                "--message",
                "feat: x",
                "--paths",
                "src/**",
                "--check",
                "workers",
            ]))
        };
        let Ok(Some(Method::TodoRunWait(params))) = with_agent("pi") else {
            panic!("run --agent pi did not parse");
        };
        assert_eq!(params.agent, Some(WorkerAgent::Pi));
        assert_eq!(params.paths, ["src/**"]);
        let Ok(Some(Method::TodoRunAnthropic(params))) = with_agent("pi-anthropic") else {
            panic!("credit worker did not select its independent method");
        };
        assert_eq!(params.agent, Some(WorkerAgent::Pi));
        let wire = serde_json::to_value(Method::TodoRunAnthropic(params)).unwrap();
        assert_eq!(wire["method"], "todo.run_anthropic");

        let Ok(Some(Method::TodoRunWait(params))) = with_agent("claude") else {
            panic!("run --agent claude did not parse");
        };
        assert_eq!(params.agent, Some(WorkerAgent::Claude));
        let Err(error) = with_agent("codex") else {
            panic!("run --agent codex parsed");
        };
        assert!(error.contains("claude or pi"), "{error}");
        let Ok(Some(Method::TodoRunWait(params))) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--auto-review",
            "--task",
            &task,
            "--message",
            "feat: x",
            "--paths",
            "src/**",
            "--check",
            "workers",
        ])) else {
            panic!("run --auto-review did not parse");
        };
        assert!(params.auto_review);
        let started = serde_json::json!({"result": {"type": "todo_run", "run": {
            "run_id": "r-abcd2345", "auto_review": true}}});
        assert_eq!(auto_review_ignored(&params, &started), None);
        let old = serde_json::json!({"result": {"type": "todo_run", "run": {
            "run_id": "r-abcd2345"}}});
        assert!(auto_review_ignored(&params, &old)
            .unwrap()
            .contains("r-abcd2345"));
        let Ok(Some(Method::TodoRunWait(params))) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--auto-answer",
            "--task",
            &task,
            "--message",
            "feat: x",
            "--paths",
            "src/**",
            "--check",
            "workers",
        ])) else {
            panic!("run --auto-answer did not parse");
        };
        assert!(params.auto_answer && !params.auto_review);
        let answering = serde_json::json!({"result": {"type": "todo_run", "run": {
            "run_id": "r-abcd2345", "auto_answer": true}}});
        assert_eq!(auto_review_ignored(&params, &answering), None);
        assert!(auto_review_ignored(&params, &old)
            .unwrap()
            .contains("--auto-answer"));
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
        assert!(!params.keep_open);
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "9",
            "--action",
            "approve",
            "--keep-open",
        ])) else {
            panic!("approve --keep-open did not parse");
        };
        assert!(params.keep_open);
        // The guarded paths are the approval's own word, and only its own.
        let Ok(Some(Method::TodoResume(params))) = parse(&args(&[
            "resume",
            "r-abcd2345",
            "--event",
            "9",
            "--action",
            "approve",
            "--accept-guarded-paths",
        ])) else {
            panic!("approve --accept-guarded-paths did not parse");
        };
        assert!(params.accept_guarded_paths);
        for bad in [
            &["--action", "approve", "--note", &note, "--close", &note][..],
            &["--action", "abort", "--note", &note][..],
            &["--action", "abort", "--keep-open"][..],
            &["--action", "approve", "--keep-open", "--close", &note][..],
            &["--action", "abort", "--accept-guarded-paths"][..],
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
    fn log_reads_the_runs_status_and_shows_control_characters_as_spaces() {
        let Ok(Some(Method::TodoStatus(target))) = parse(&args(&["log", "--follow", "r-abcd2345"]))
        else {
            panic!("log did not parse");
        };
        assert_eq!(target.run_id, "r-abcd2345");
        assert!(parse(&args(&["log"])).is_err());
        assert_eq!(log_line("\u{1b}[1mok\r"), " [1mok ");
        assert_eq!(log_line("a\tb"), "a\tb");
    }

    #[test]
    fn ack_stop_takes_one_run_id() {
        let Ok(Some(Method::TodoAckStop(target))) = parse(&args(&["ack-stop", "r-abcd2345"]))
        else {
            panic!("ack-stop did not parse");
        };
        assert_eq!(target.run_id, "r-abcd2345");
        assert!(parse(&args(&["ack-stop"])).is_err());
        assert!(parse(&args(&["ack-stop", "r-a", "r-b"])).is_err());
    }

    #[test]
    fn the_help_names_every_option_run_parses_and_the_agent_values() {
        for option in [
            "--task",
            "--message",
            "--paths",
            "--check",
            "--ignore-usage",
            "--auto-review",
            "--auto-answer",
            "--draft",
            "--agent claude|pi|pi-anthropic",
            "--next-agent claude|pi|pi-anthropic",
            "--model MODEL",
            "--next-model MODEL",
            "default_agent",
        ] {
            assert!(USAGE.contains(option), "{option} missing from the help");
        }
    }

    #[test]
    fn a_draft_and_a_chain_take_an_optional_agent() {
        use crate::api::schema::WorkerAgentChoice;
        let Ok(Some(Method::TodoDraftRun(params))) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--draft",
            "--agent",
            "pi-anthropic",
        ])) else {
            panic!("draft --agent did not parse");
        };
        assert_eq!(params.agent, Some(WorkerAgentChoice::PiAnthropic));
        let Ok(Some(Method::TodoDraftRun(params))) =
            parse(&args(&["run", "t-abcd2345", "--draft"]))
        else {
            panic!("draft did not parse");
        };
        assert_eq!(params.agent, None);
        assert!(parse(&args(&["run", "t-abcd2345", "--draft", "--agent", "gpt"])).is_err());

        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-next-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let task = dir.join("task.md");
        std::fs::write(&task, "Do the next thing\n").unwrap();
        let task = task.display().to_string();
        let resume = |agent: &[&str]| {
            let mut words = vec![
                "resume",
                "r-abcd2345",
                "--event",
                "9",
                "--action",
                "approve",
                "--note",
                &task,
                "--next",
                "t-bcde3456",
                "--next-task",
                &task,
                "--next-message",
                "feat: y",
                "--next-paths",
                "src/**",
            ];
            words.extend_from_slice(agent);
            parse(&args(&words))
        };
        let Ok(Some(Method::TodoResume(params))) = resume(&["--next-agent", "pi"]) else {
            panic!("--next-agent did not parse");
        };
        assert_eq!(params.next.unwrap().agent, Some(WorkerAgentChoice::Pi));
        let Ok(Some(Method::TodoResume(params))) = resume(&[
            "--next-agent",
            "pi",
            "--next-model",
            "deepseek/deepseek-flash",
        ]) else {
            panic!("--next-model did not parse");
        };
        let next = params.next.unwrap();
        assert_eq!(next.agent, Some(WorkerAgentChoice::Pi));
        assert_eq!(next.model.as_deref(), Some("deepseek/deepseek-flash"));
        let Ok(Some(Method::TodoResume(params))) = resume(&[]) else {
            panic!("--next did not parse");
        };
        assert_eq!(params.next.unwrap().agent, None);
        assert!(resume(&["--next-agent", "gpt"]).is_err());
        // A chain's model is a pi model: a Claude or credit next run keeps
        // its own.
        for agent in ["claude", "pi-anthropic"] {
            let Err(error) = resume(&[
                "--next-agent",
                agent,
                "--next-model",
                "deepseek/deepseek-flash",
            ]) else {
                panic!("a {agent} next run took a pi model");
            };
            assert!(error.contains("--next-model is for a pi chain"), "{error}");
        }
    }

    /// `--model` chooses a gated pi worker's model; a Claude or credit run
    /// refuses it, and the value travels in the run's parameters.
    #[test]
    fn run_takes_a_pi_model_and_refuses_it_for_a_claude_or_credit_run() {
        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-model-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let task = dir.join("task.md");
        std::fs::write(&task, "Do the thing\n").unwrap();
        let task = task.display().to_string();
        let with = |agent: Option<&str>| {
            let mut words = vec!["run", "t-abcd2345"];
            if let Some(agent) = agent {
                words.extend_from_slice(&["--agent", agent]);
            }
            words.extend_from_slice(&[
                "--model",
                "deepseek/deepseek-flash",
                "--task",
                &task,
                "--message",
                "feat: x",
                "--paths",
                "src/**",
                "--check",
                "workers",
            ]);
            parse(&args(&words))
        };
        let Ok(Some(Method::TodoRunWait(params))) = with(Some("pi")) else {
            panic!("run --agent pi --model did not parse");
        };
        assert_eq!(params.model.as_deref(), Some("deepseek/deepseek-flash"));
        let wire = serde_json::to_value(Method::TodoRunWait(params)).unwrap();
        assert_eq!(wire["params"]["model"], "deepseek/deepseek-flash");
        // Without --agent the server resolves [workers] default_agent.
        let Ok(Some(Method::TodoRunWait(params))) = with(None) else {
            panic!("run --model without --agent did not parse");
        };
        assert_eq!(params.agent, None);
        assert_eq!(params.model.as_deref(), Some("deepseek/deepseek-flash"));
        let Err(error) = with(Some("claude")) else {
            panic!("a Claude run took a pi model");
        };
        assert!(error.contains("Claude Code's own model"), "{error}");
        let Err(error) = with(Some("pi-anthropic")) else {
            panic!("a credit run took a pi model");
        };
        assert!(error.contains("anthropic_api_model"), "{error}");
        assert!(parse(&args(&["run", "t-abcd2345", "--model"])).is_err());

        let Ok(Some(Method::TodoDraftRun(params))) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--draft",
            "--agent",
            "pi",
            "--model",
            "deepseek/deepseek-flash",
        ])) else {
            panic!("draft --model did not parse");
        };
        assert_eq!(params.model.as_deref(), Some("deepseek/deepseek-flash"));
        let Err(error) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--draft",
            "--agent",
            "pi-anthropic",
            "--model",
            "deepseek/deepseek-flash",
        ])) else {
            panic!("a credit draft took a pi model");
        };
        assert!(error.contains("anthropic_api_model"), "{error}");
        let Err(error) = parse(&args(&[
            "run",
            "t-abcd2345",
            "--draft",
            "--agent",
            "claude",
            "--model",
            "deepseek/deepseek-flash",
        ])) else {
            panic!("a Claude draft took a pi model");
        };
        assert!(error.contains("Claude Code's own model"), "{error}");
    }

    #[test]
    fn a_close_takes_the_next_run_or_a_stop_reason() {
        let dir = std::env::temp_dir().join(format!("herdr-cli-todo-close-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let close = dir.join("close.md");
        std::fs::write(&close, "## Closed\n").unwrap();
        let close = close.display().to_string();
        let task = dir.join("task.md");
        std::fs::write(&task, "Do the next thing\n").unwrap();
        let task = task.display().to_string();
        let resume = |extra: &[&str]| {
            let mut words = vec![
                "resume",
                "r-abcd2345",
                "--event",
                "9",
                "--action",
                "approve",
            ];
            words.extend_from_slice(extra);
            parse(&args(&words))
        };
        let Ok(Some(Method::TodoResume(params))) = resume(&[
            "--close",
            &close,
            "--next",
            "t-bcde3456",
            "--next-task",
            &task,
            "--next-message",
            "feat: y",
            "--next-paths",
            "src/**",
            "docs/**",
        ]) else {
            panic!("--close --next did not parse");
        };
        assert_eq!(
            params.next,
            Some(TodoNextRun {
                agent: None,
                model: None,
                item: "t-bcde3456".into(),
                task: "Do the next thing\n".into(),
                message: "feat: y".into(),
                paths: vec!["src/**".into(), "docs/**".into()],
                checks: Vec::new(),
            })
        );
        assert_eq!(params.stop_reason, None);
        let Ok(Some(Method::TodoResume(params))) = resume(&[
            "--next-check",
            "workers",
            "tests",
            "--close",
            &close,
            "--next",
            "t-bcde3456",
            "--next-task",
            &task,
            "--next-message",
            "feat: y",
            "--next-paths",
            "src/**",
        ]) else {
            panic!("--next-check did not parse");
        };
        assert_eq!(params.next.unwrap().checks, ["workers", "tests"]);
        let Ok(Some(Method::TodoResume(params))) = resume(&[
            "--close",
            &close,
            "--stop-reason",
            "the rest waits on the user",
        ]) else {
            panic!("--close --stop-reason did not parse");
        };
        assert_eq!(
            (params.next, params.stop_reason.as_deref()),
            (None, Some("the rest waits on the user"))
        );
        // A close alone is the server's to judge: refused unless queue mode
        // is on.
        let Ok(Some(Method::TodoResume(params))) = resume(&["--close", &close]) else {
            panic!("--close alone did not parse");
        };
        assert_eq!((params.next, params.stop_reason), (None, None));
        for bad in [
            // Not both.
            &[
                "--close",
                &close,
                "--stop-reason",
                "x",
                "--next",
                "t-bcde3456",
                "--next-task",
                &task,
                "--next-message",
                "feat: y",
                "--next-paths",
                "src/**",
            ][..],
            // The next run needs its task, message and paths.
            &["--close", &close, "--next", "t-bcde3456"][..],
            // Neither without a close or a note.
            &["--stop-reason", "x"][..],
            &["--keep-open", "--stop-reason", "x"][..],
            // Not both after a note either.
            &[
                "--note",
                &close,
                "--stop-reason",
                "x",
                "--next",
                "t-bcde3456",
                "--next-task",
                &task,
                "--next-message",
                "feat: y",
                "--next-paths",
                "src/**",
            ][..],
        ] {
            assert!(resume(bad).is_err(), "{bad:?}");
        }
        // A note takes them too, or neither: the noted item stays open.
        let Ok(Some(Method::TodoResume(params))) = resume(&[
            "--note",
            &close,
            "--next",
            "t-bcde3456",
            "--next-task",
            &task,
            "--next-message",
            "feat: y",
            "--next-paths",
            "src/**",
        ]) else {
            panic!("--note --next did not parse");
        };
        assert_eq!(params.next.unwrap().item, "t-bcde3456");
        assert_eq!(params.note.as_deref(), Some("## Closed\n"));
        let Ok(Some(Method::TodoResume(params))) =
            resume(&["--note", &close, "--stop-reason", "the rest waits"])
        else {
            panic!("--note --stop-reason did not parse");
        };
        assert_eq!(
            (params.next, params.stop_reason.as_deref()),
            (None, Some("the rest waits"))
        );
        let Ok(Some(Method::TodoResume(params))) = resume(&["--note", &close]) else {
            panic!("--note alone did not parse");
        };
        assert_eq!((params.next, params.stop_reason), (None, None));
        let _ = std::fs::remove_dir_all(dir);
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
    fn queue_takes_on_pause_or_status_and_the_repository() {
        let Ok(Some(Method::TodoQueueSet(on))) = parse(&args(&["queue", "on", "--repo", "/repo"]))
        else {
            panic!("queue on did not parse");
        };
        assert_eq!((on.cwd.as_str(), on.mode), ("/repo", TodoQueueMode::On));
        assert!(on
            .env
            .is_some_and(|env| env.keys().all(|key| !key.starts_with("HERDR_"))));
        let Ok(Some(Method::TodoQueueSet(pause))) =
            parse(&args(&["queue", "pause", "--reason", "lunch"]))
        else {
            panic!("queue pause did not parse");
        };
        assert_eq!(pause.mode, TodoQueueMode::Paused);
        assert_eq!(pause.reason.as_deref(), Some("lunch"));
        assert!(pause.env.is_none());
        assert!(std::path::Path::new(&pause.cwd).is_absolute());
        let Ok(Some(Method::TodoQueueStatus(status))) =
            parse(&args(&["queue", "status", "--repo", "/repo"]))
        else {
            panic!("queue status did not parse");
        };
        assert_eq!(status.cwd, "/repo");
        for bad in [
            &["queue"][..],
            &["queue", "off"],
            &["queue", "on", "pause"],
            &["queue", "on", "--reason", "x"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn queue_on_takes_the_agent_and_pi_model_of_its_runs() {
        use crate::api::schema::WorkerAgentChoice;
        let Ok(Some(Method::TodoQueueSet(on))) = parse(&args(&[
            "queue",
            "on",
            "--agent",
            "pi",
            "--model",
            "deepseek/deepseek-flash",
        ])) else {
            panic!("queue on with an agent did not parse");
        };
        assert_eq!(on.mode, TodoQueueMode::On);
        assert_eq!(on.agent, Some(WorkerAgentChoice::Pi));
        assert_eq!(on.model.as_deref(), Some("deepseek/deepseek-flash"));
        let Ok(Some(Method::TodoQueueSet(credits))) =
            parse(&args(&["queue", "on", "--agent", "pi-anthropic"]))
        else {
            panic!("queue on --agent pi-anthropic did not parse");
        };
        assert_eq!(
            (credits.agent, credits.model),
            (Some(WorkerAgentChoice::PiAnthropic), None)
        );
        let Ok(Some(Method::TodoQueueSet(plain))) = parse(&args(&["queue", "on"])) else {
            panic!("queue on did not parse");
        };
        assert_eq!((plain.agent, plain.model), (None, None));
        for (bad, why) in [
            (
                &["queue", "on", "--agent", "gpt"][..],
                "claude, pi or pi-anthropic",
            ),
            (
                &[
                    "queue",
                    "on",
                    "--agent",
                    "claude",
                    "--model",
                    "deepseek/deepseek-flash",
                ],
                "Claude Code's own model",
            ),
            (
                &[
                    "queue",
                    "on",
                    "--agent",
                    "pi-anthropic",
                    "--model",
                    "deepseek/deepseek-flash",
                ],
                "anthropic_api_model",
            ),
            (&["queue", "pause", "--agent", "pi"], "only queue on"),
            (
                &["queue", "status", "--model", "deepseek/deepseek-flash"],
                "only queue on",
            ),
            (&["queue", "on", "--model"], "--model"),
        ] {
            let error = parse(&args(bad)).expect_err(&format!("{bad:?}"));
            assert!(error.contains(why), "{bad:?}: {error}");
        }
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
}
