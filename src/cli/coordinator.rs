//! `herdr coordinator`: claims, ends and lists coordination tenures (one
//! active coordinator per repository), and opens a space's interactive TODO
//! coordinator.

use std::collections::HashMap;

use crate::api::schema::{
    AgentPromptConfirmedParams, AgentStartParams, AgentStatus, AgentTarget, AgentWaitChangeParams,
    AgentWaitStartParams, CoordinatorAckParams, CoordinatorAcquireParams, CoordinatorEndParams,
    CoordinatorHandoffAbortParams, CoordinatorHandoffParams, CoordinatorHandoffSuccessorParams,
    CoordinatorReadyParams, CoordinatorStartParams, CoordinatorStatusParams, EmptyParams, Method,
    PaneListParams, Request, TabCreateParams, TabRole, TabSetRoleParams, TabTarget,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr coordinator start [--repo DIR] [--token]
      Claim the repository's coordination for this pane ($HERDR_PANE_ID):
      refused (coordinator_active, naming the holder) while another pane or
      process holds it. DIR is a directory in the repository; this pane's
      directory by default. Setting a tab's role to coordinator does the
      same. Opening a new coordinator tab in another space is coordinator
      open, not this. The repository rooted at the home directory (dotfiles)
      is claimed only explicitly, with --repo ~: a pane's directory there is
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
      closed the reply says so and that session stays fenced as superseded.
  herdr coordinator open SPACE [--repo DIR] [--agent claude|pi] [--model MODEL]
                             [--name NAME] [--prompt TEXT] [--dry-run]
                             [-- AGENT_ARG...]
      Open a tab in the space labelled SPACE and start its TODO coordinator
      there: the interactive coordinator a person watches, not coordinator
      start (the tenure of the pane you are in) and not todo next (a
      headless item coordinator). The repository is --repo, else the space's
      worktree, else the one git root its panes share. The agent, named
      todo-<space> unless --name, starts in the tab's only pane and gets the
      TODO prompt (--prompt, else \"Rób TODO po kolei.\") once it takes
      prompts. claude starts with Remote Control (--remote-control \"TODO
      SPACE\"); pi with --model openai-codex/gpt-6.1-sol (the ChatGPT plan
      the usage gate knows), --model replacing that model (a claude launch
      passes no model, and says so); the arguments after -- replace those
      defaults. A repository that already has a
      coordinator is left as it is: its holder is printed and nothing
      starts. A startup dialog (a folder-trust prompt) is the user's to
      answer in the new tab: the command names the pane and waits for the
      agent to take prompts, giving up after the answer window and leaving
      the tab and its tenure. --dry-run prints the plan and starts nothing.";

pub(super) fn run_coordinator_command(args: &[String]) -> std::io::Result<i32> {
    // `open` composes several advertised methods; the other subcommands are
    // one request each, parsed below.
    if args.first().map(String::as_str) == Some("open") {
        return run_open(&args[1..]);
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
    let id = format!("cli:coordinator:{}", args[0]);
    let token_for_pane = match &method {
        Method::CoordinatorStart(params) if args.iter().any(|arg| arg == "--token") => {
            params.pane_id.clone()
        }
        _ => None,
    };
    let ending = matches!(&method, Method::CoordinatorEnd(_));
    let response = super::send_request(&Request { id, method })?;
    let Some(pane_id) = token_for_pane.filter(|_| response.get("error").is_none()) else {
        // An end a landing step of the tenure's run holds back still ended
        // nothing: the reply shows the tenure as it is, and this says why.
        if ending && response.get("error").is_none() {
            let coordinator = &response["coordinator"];
            if coordinator["ended_ms"].is_null() {
                eprintln!(
                    "coordinator {} is ending: a landing step of its run (cherry-pick, \
                     install, TODO edit or push) is in flight, and it ends once that step \
                     has landed",
                    coordinator["coordinator_id"]
                        .as_str()
                        .unwrap_or("the tenure")
                );
            }
        }
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
        "open" => {
            return Err(
                "open is a composition of requests: run_coordinator_command answers it".to_string(),
            )
        }
        other => return Err(format!("unknown coordinator command: {other}")),
    }))
}

/// The default model of a pi coordinator: GPT-6.1 Sol on the ChatGPT plan,
/// the provider the run's usage gate reads as `openai-codex`
/// (`src/workers/runs/usage_gate.rs`).
const PI_COORDINATOR_MODEL: &str = "openai-codex/gpt-6.1-sol";

/// What an agent is asked to do: the user's "Working through TODO.md" rule.
const TODO_PROMPT: &str = "Rób TODO po kolei.";

/// How long `open` waits for the user to answer a startup dialog. The bound
/// is the user answering the trust dialog, never a race with the agent: past
/// it nothing of the tab or its tenure is undone, the command says where the
/// agent waits and leaves that answer to the user. A platform whose sockets
/// have no read timeout (Windows named pipes) waits only as long as the agent.
const DIALOG_ANSWER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600);

/// The agent `coordinator open` starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAgent {
    Claude,
    Pi,
}

impl OpenAgent {
    /// The label `agent start --kind` takes.
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Pi => "pi",
        }
    }
}

/// `coordinator open`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenOptions {
    /// The workspace label of the space the tab opens in.
    space: String,
    /// `--repo`, made absolute.
    repo: Option<String>,
    agent: OpenAgent,
    /// `--model`: the pi model, overriding the default one.
    model: Option<String>,
    /// `--name`: the agent's name, else `todo-<space>`.
    name: Option<String>,
    /// `--prompt`: what the agent is asked to do, else [`TODO_PROMPT`].
    prompt: Option<String>,
    /// The arguments after `--`, which replace the agent's default ones.
    agent_args: Vec<String>,
    dry_run: bool,
}

/// Opens SPACE's interactive TODO coordinator.
fn run_open(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_open(args) {
        Ok(Some(options)) => options,
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
    // The coordinator is a tab of this machine's server: outside a herdr
    // session there is no workspace to open it in.
    if super::target::caller_workspace_id().is_none() {
        eprintln!(
            "herdr coordinator open: not inside a herdr session (HERDR_WORKSPACE_ID is not set)"
        );
        return Ok(1);
    }
    match open_with(&Live, &Disk, &options) {
        Ok(outcome) => {
            if !outcome.text.is_empty() {
                println!("{}", outcome.text);
            }
            Ok(outcome.code)
        }
        Err(Failure::Refused(message)) => {
            eprintln!("herdr coordinator open: {message}");
            Ok(1)
        }
        // A server refusal names the pane or the agent; the space it was
        // asked for is added, so every line says where.
        Err(Failure::Server { message, .. }) => {
            eprintln!("herdr coordinator open: space {}: {message}", options.space);
            Ok(1)
        }
        Err(Failure::Io(error)) => Err(error),
    }
}

fn parse_open(args: &[String]) -> Result<Option<OpenOptions>, String> {
    let separator = args.iter().position(|arg| arg == "--");
    let (before, agent_args) = match separator {
        Some(index) => (&args[..index], args[index + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    // Help anywhere before `--`; after it the arguments are the agent's.
    if before
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        return Ok(None);
    }
    let (repo, rest) = take_string_option(before, "--repo")?;
    let (agent, rest) = take_string_option(&rest, "--agent")?;
    let (model, rest) = take_string_option(&rest, "--model")?;
    let (name, rest) = take_string_option(&rest, "--name")?;
    let (prompt, rest) = take_string_option(&rest, "--prompt")?;
    let mut dry_run = false;
    let mut space = None;
    for argument in &rest {
        match argument.as_str() {
            "--dry-run" if !dry_run => dry_run = true,
            other if space.is_none() && !other.starts_with('-') => space = Some(other.to_owned()),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let space = space.ok_or("open needs SPACE, the label of a workspace")?;
    let agent = match agent.as_deref() {
        None | Some("claude") => OpenAgent::Claude,
        Some("pi") => OpenAgent::Pi,
        Some(other) => return Err(format!("--agent takes claude or pi, not {other}")),
    };
    // `--model` is a pi model (the spec advertises it as one): with claude it
    // is accepted and reported by `open_with`, not refused here.
    if model.as_deref() == Some("") {
        return Err("--model must not be empty".to_string());
    }
    if let Some(name) = name.as_deref() {
        if !valid_agent_name(name) {
            return Err(format!(
                "--name {name} must start with a lowercase letter and contain only lowercase \
                 letters, digits, '-' or '_' (1-32 characters)"
            ));
        }
    }
    if prompt.as_deref() == Some("") {
        return Err("--prompt must not be empty".to_string());
    }
    Ok(Some(OpenOptions {
        space,
        repo: absolute(repo)?,
        agent,
        model,
        name,
        prompt,
        agent_args,
        dry_run,
    }))
}

/// What `open` prints and exits with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    text: String,
    code: i32,
}

impl Outcome {
    fn new(code: i32, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            code,
        }
    }
}

/// Why a step stopped `open`.
#[derive(Debug)]
enum Failure {
    /// This command refuses the request: the message says why.
    Refused(String),
    /// The server refused it: the code decides what `open` answers (a claim
    /// refused `coordinator_active` is another coordinator's repository, a
    /// launch refused `agent_not_ready` waits at a dialog).
    Server { code: String, message: String },
    /// The request could not be made at all: no server, a protocol mismatch,
    /// an unreadable answer.
    Io(std::io::Error),
}

impl Failure {
    /// The failure with what a cleanup could not do, so a cleanup that did not
    /// happen is read together with the step that failed.
    fn with_notes(self, notes: &[String]) -> Self {
        if notes.is_empty() {
            return self;
        }
        let notes = notes.join("; ");
        match self {
            Self::Refused(message) => Self::Refused(format!("{message}; {notes}")),
            Self::Server { code, message } => Self::Server {
                code,
                message: format!("{message}; {notes}"),
            },
            Self::Io(error) => Self::Io(std::io::Error::new(
                error.kind(),
                format!("{error}; {notes}"),
            )),
        }
    }
}

fn unexpected(step: &str) -> Failure {
    Failure::Io(std::io::Error::other(format!(
        "{step} answered a result this command does not know"
    )))
}

/// Whether a step goes on or the command answers with what it has.
enum Step {
    Continue,
    Done(Outcome),
}

/// What `open` asks of herdr, so its steps are testable without a server.
trait Herdr {
    /// Sends one advertised method and answers the reply as the server sent
    /// it.
    fn request(&self, id: &str, method: Method) -> Result<serde_json::Value, Failure>;
    /// Waits until the agent's `state_change_seq` moves past `after`, bounded
    /// by `deadline` (the user answering a startup dialog).
    fn wait_change(
        &self,
        pane_id: &str,
        after: u64,
        deadline: std::time::Duration,
    ) -> Result<Change, Failure>;
    /// Tells the user something now, before a wait.
    fn notice(&self, text: &str);
}

/// How a wait for the agent's state ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// Its state changed: the dialog was answered (or the agent moved on).
    Changed,
    /// The answer window passed with the agent still in that state.
    TimedOut,
}

/// The filesystem `open` resolves its repository against.
trait Paths {
    fn is_dir(&self, path: &str) -> bool;
}

/// The filesystem this command runs on.
struct Disk;

impl Paths for Disk {
    fn is_dir(&self, path: &str) -> bool {
        std::path::Path::new(path).is_dir()
    }
}

/// The server this command runs against.
struct Live;

impl Herdr for Live {
    fn request(&self, id: &str, method: Method) -> Result<serde_json::Value, Failure> {
        super::send_request(&Request {
            id: id.to_owned(),
            method,
        })
        .map_err(Failure::Io)
    }

    fn wait_change(
        &self,
        pane_id: &str,
        after: u64,
        deadline: std::time::Duration,
    ) -> Result<Change, Failure> {
        let request = Request {
            id: "cli:coordinator:open:change".into(),
            method: Method::AgentWaitChange(AgentWaitChangeParams {
                target: pane_id.to_owned(),
                prefer_workspace_id: None,
                state_change_seq: after,
            }),
        };
        match super::send_request_with_deadline(&request, deadline) {
            Ok(reply) => result(&reply, "agent.wait_change").map(|_| Change::Changed),
            // Nothing on the server's side moves until the user answers, so an
            // answer that never came is the deadline, not a change.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(Change::TimedOut)
            }
            Err(error) => Err(Failure::Io(error)),
        }
    }

    fn notice(&self, text: &str) {
        println!("{text}");
    }
}

/// The `result` of an answer, or the failure that stops the step.
fn result(reply: &serde_json::Value, step: &str) -> Result<serde_json::Value, Failure> {
    if let Some(error) = reply.get("error") {
        return Err(Failure::Server {
            code: error["code"].as_str().unwrap_or_default().to_owned(),
            message: error["message"].as_str().unwrap_or_default().to_owned(),
        });
    }
    reply.get("result").cloned().ok_or_else(|| unexpected(step))
}

fn string(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// A workspace, as `workspace.list` describes it.
struct Space {
    workspace_id: String,
    label: String,
    /// The worktree the space was created from or opened as.
    worktree: Option<String>,
}

/// A coordination tenure, as `coordinator.status` describes it.
struct Tenure {
    coordinator_id: String,
    pane_id: Option<String>,
    holder_kind: Option<String>,
    holder_pid: Option<u32>,
}

/// The tab `tab.create` made, and its only pane.
struct CreatedTab {
    tab_id: String,
    pane_id: String,
    label: String,
    number: u64,
}

impl CreatedTab {
    /// The tab as a person reads it: its label, else its number.
    fn label(&self) -> String {
        if self.label.is_empty() {
            self.number.to_string()
        } else {
            self.label.clone()
        }
    }
}

/// The agent as `agent.get` reads it.
struct AgentFacts {
    status: AgentStatus,
    state_change_seq: u64,
}

/// `coordinator open`'s steps.
fn open_with(
    herdr: &dyn Herdr,
    paths: &dyn Paths,
    options: &OpenOptions,
) -> Result<Outcome, Failure> {
    let spaces = spaces(herdr)?;
    let Some(space) = spaces.iter().find(|space| space.label == options.space) else {
        return Err(Failure::Refused(format!(
            "no space named {}",
            options.space
        )));
    };
    // The panes are read only when nothing else names the repository.
    let cwds = if options.repo.is_none() && space.worktree.is_none() {
        pane_cwds(herdr, &space.workspace_id)?
    } else {
        Vec::new()
    };
    let repo = resolve_repository(
        &options.space,
        options.repo.as_deref(),
        space.worktree.as_deref(),
        &cwds,
        git_root,
        |path| paths.is_dir(path),
    )
    .map_err(Failure::Refused)?;

    let name = options
        .name
        .clone()
        .unwrap_or_else(|| derived_name(&options.space));
    let argv = agent_arguments(options);
    let prompt_text = options
        .prompt
        .clone()
        .unwrap_or_else(|| TODO_PROMPT.to_owned());

    // `--model` names a pi provider/model, as the spec advertises it. A claude
    // launch passes none, and this says so instead of dropping it silently.
    if let (OpenAgent::Claude, Some(model)) = (options.agent, options.model.as_deref()) {
        herdr.notice(&format!(
            "claude takes no --model here: {model} names a pi coordinator's provider/model and is \
             not passed"
        ));
    }

    if options.dry_run {
        return Ok(Outcome::new(
            0,
            plan(options, &repo, &name, &argv, &prompt_text),
        ));
    }

    // The tenure is the identity, so this read only avoids opening a tab the
    // claim would refuse; two opens at once are decided by the claim below.
    if let Some(tenure) = tenures(herdr, &repo)?.first() {
        return Ok(Outcome::new(0, already_coordinated(tenure)));
    }

    let tab = create_tab(herdr, &space.workspace_id, &repo)?;
    match claim(herdr, &tab.tab_id) {
        Ok(()) => {}
        Err(Failure::Server { code, .. }) if code == "coordinator_active" => {
            // Another open won the repository: the tab this one made goes
            // again, and the holder is read as `coordinator status` reads it,
            // never as the claim's own wording.
            let problems = close_created_tab(herdr, &tab);
            return Ok(Outcome::new(
                0,
                with_notes(claim_refused(herdr, &repo), &problems),
            ));
        }
        Err(failure) => return Err(abandoned(herdr, &tab, failure)),
    }

    let kind = options.agent.name();
    // A launch that ended at a dialog answers `agent_not_ready` and names it;
    // the state read below decides whether the agent waits at one or is gone.
    let mut launch_refusal = None;
    match start_agent(herdr, &name, kind, &tab.pane_id, &argv) {
        Ok(()) => {}
        Err(Failure::Server { code, message }) if code == "agent_not_ready" => {
            launch_refusal = Some(Failure::Server { code, message });
        }
        Err(failure) => return Err(abandoned(herdr, &tab, failure)),
    }

    let mut facts = match agent_facts(herdr, &tab.pane_id) {
        Ok(facts) => facts,
        // The agent the launch was refused for is not there any more (an
        // exited one reads as none at all): its own refusal is the reason,
        // and no prompt goes into a dead agent.
        Err(failure) => return Err(abandoned(herdr, &tab, launch_refusal.unwrap_or(failure))),
    };
    let detail = launch_refusal.as_ref().map(describe);

    // A dialog that is gone by the time of this read was answered between the
    // launch's answer and it: the state below is what the agent waits for.
    if facts.status == AgentStatus::Blocked {
        match answer_dialog(
            herdr,
            options,
            &tab,
            facts.state_change_seq,
            detail.as_deref(),
        )? {
            Step::Continue => {}
            Step::Done(outcome) => return Ok(outcome),
        }
        // Answered: the state again decides whether a prompt fits, and a
        // second dialog or an agent that left is reported, never prompted.
        facts = match agent_facts(herdr, &tab.pane_id) {
            Ok(facts) => facts,
            Err(failure) => {
                return Ok(Outcome::new(
                    1,
                    format!(
                        "{kind} left pane {} (space {}, tab {}) after its startup dialog ({}); \
                         the TODO was not sent, and the tab and its tenure stay",
                        tab.pane_id,
                        options.space,
                        tab.label(),
                        describe(&failure)
                    ),
                ))
            }
        };
    } else if let Some(refusal) = launch_refusal {
        // A launch refused `agent_not_ready` whose agent reads as anything but
        // blocked waits at no dialog: its own refusal is what is reported.
        return Err(abandoned(herdr, &tab, refusal));
    }

    // Only a state that takes a prompt gets one: an agent blocked again waits
    // for the user's answer, and one whose state is unknown names no agent
    // `agent prompt` would accept. Both are reported, and the tab and its
    // tenure stay, since the prompt was never typed.
    if !takes_prompt(facts.status) {
        let _ = focus(herdr, &tab.pane_id);
        let reason = match facts.status {
            AgentStatus::Blocked => "it is blocked on a dialog again".to_owned(),
            status => format!("its state reads {}", state_name(status)),
        };
        return Ok(Outcome::new(
            1,
            format!(
                "{kind} in pane {} does not take the TODO prompt now (space {}, tab {}): \
                 {reason}; the TODO was not sent, and the tab and its tenure stay",
                tab.pane_id,
                options.space,
                tab.label()
            ),
        ));
    }

    match prompt(herdr, &tab.pane_id, &prompt_text) {
        Ok(()) => Ok(Outcome::new(
            0,
            format!(
                "started {kind} as {name} in space {}, tab {}, pane {}, repository {repo}",
                options.space,
                tab.label(),
                tab.pane_id
            ),
        )),
        Err(Failure::Server { code, message })
            if code == "agent_blocked" || code == "agent_prompt_blocked" =>
        {
            // It waits on a dialog again, this one after typing: it is the
            // user's to answer, so the tab and the tenure stay.
            let _ = focus(herdr, &tab.pane_id);
            Ok(Outcome::new(
                1,
                format!(
                    "blocked: {kind} waits for an answer in pane {} (space {}, tab {}); the \
                     TODO prompt was not confirmed: {message}",
                    tab.pane_id,
                    options.space,
                    tab.label()
                ),
            ))
        }
        // A refusal the server sends before it types anything, and its message
        // names the agent: the launch never happened, so nothing stays.
        Err(failure) if refused_before_typing(&failure) => Err(abandoned(herdr, &tab, failure)),
        // Anything else may have typed the prompt before its answer was lost
        // (a dropped connection, a timeout): it is never sent again, and the
        // tab and the tenure stay for the user to look at.
        Err(failure) => Ok(Outcome::new(
            1,
            format!(
                "the TODO prompt was sent to {kind} in pane {} (space {}, tab {}), but its \
                 answer was lost ({}), so whether it was accepted is unknown; it is not sent \
                 again",
                tab.pane_id,
                options.space,
                tab.label(),
                describe(&failure)
            ),
        )),
    }
}

/// The agent stopped at a startup dialog (a folder-trust prompt): it is the
/// user's to answer in the new tab, so this brings the tab forward, says where
/// and what waits, and waits for the agent to take prompts.
fn answer_dialog(
    herdr: &dyn Herdr,
    options: &OpenOptions,
    tab: &CreatedTab,
    after: u64,
    detail: Option<&str>,
) -> Result<Step, Failure> {
    let kind = options.agent.name();
    let _ = focus(herdr, &tab.pane_id);
    let dialog = detail.unwrap_or("the agent is blocked and takes no prompt yet");
    herdr.notice(&format!(
        "answer {kind}'s startup dialog in pane {}, space {} (tab {}): {dialog}. The TODO starts \
         once it is answered",
        tab.pane_id,
        options.space,
        tab.label()
    ));
    match herdr.wait_change(&tab.pane_id, after, DIALOG_ANSWER_DEADLINE) {
        Ok(Change::Changed) => Ok(Step::Continue),
        Ok(Change::TimedOut) => Ok(Step::Done(Outcome::new(
            1,
            format!(
                "the startup dialog in pane {} was not answered within {} minutes; the TODO was \
                 not sent (space {} and tab {} stay as they are)",
                tab.pane_id,
                DIALOG_ANSWER_DEADLINE.as_secs() / 60,
                options.space,
                tab.label()
            ),
        ))),
        Err(failure) => Err(abandoned(herdr, tab, failure)),
    }
}

/// A failure after this command claimed the tenure: the tenure ends and the
/// tab it created closes, so nothing is left half-started. A cleanup that did
/// not happen is reported with the step that failed, never instead of it.
fn abandoned(herdr: &dyn Herdr, tab: &CreatedTab, failure: Failure) -> Failure {
    let mut problems = Vec::new();
    if let Err(failure) = end_tenure(herdr, &tab.pane_id) {
        problems.push(format!(
            "the tenure of pane {} did not end: {}",
            tab.pane_id,
            describe(&failure)
        ));
    }
    problems.extend(close_created_tab(herdr, tab));
    failure.with_notes(&problems)
}

/// Closes the tab this command created, naming it when it did not close. The
/// tab is found by the id `tab create` gave, so another one is never closed.
fn close_created_tab(herdr: &dyn Herdr, tab: &CreatedTab) -> Vec<String> {
    match close_tab(herdr, &tab.tab_id) {
        Ok(()) => Vec::new(),
        Err(failure) => vec![format!(
            "tab {} is still open: {}",
            tab.label(),
            describe(&failure)
        )],
    }
}

/// The line an outcome prints, with what a cleanup could not do.
fn with_notes(text: String, notes: &[String]) -> String {
    if notes.is_empty() {
        return text;
    }
    format!("{text}; {}", notes.join("; "))
}

/// A failure as one line, for a message that reports it.
fn describe(failure: &Failure) -> String {
    match failure {
        Failure::Refused(message) => message.clone(),
        Failure::Server { code, message } => format!("{code}: {message}"),
        Failure::Io(error) => error.to_string(),
    }
}

/// Whether the server refused a prompt before it typed anything: the agent is
/// gone or is no longer the pane's, so nothing of the tab has to stay.
fn refused_before_typing(failure: &Failure) -> bool {
    matches!(
        failure,
        Failure::Server { code, .. }
            if matches!(code.as_str(), "agent_not_found" | "agent_not_ready")
    )
}

/// Whether a prompt sent now would reach the agent: the states in which it
/// waits for input. `Blocked` waits for an answer to a dialog instead, and
/// `Unknown` names no agent (`agent prompt` answers `agent_not_ready`).
fn takes_prompt(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Idle | AgentStatus::Working | AgentStatus::Done
    )
}

/// The agent's state as `open` names it in what it prints.
fn state_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

/// The plan `--dry-run` prints: everything resolved, nothing started.
fn plan(options: &OpenOptions, repo: &str, name: &str, argv: &[String], prompt: &str) -> String {
    format!(
        "would start {} as {name} in space {}, repository {repo}, argv: {}, prompt: {prompt}",
        options.agent.name(),
        options.space,
        join_argv(argv)
    )
}

/// The holder of a tenure an `open` found, as it is printed.
fn already_coordinated(tenure: &Tenure) -> String {
    let holder = match tenure.holder_kind.as_deref() {
        Some("process") => format!(
            "process {}",
            tenure
                .holder_pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "?".into())
        ),
        Some("headless") => format!("headless coordinator {}", tenure.coordinator_id),
        _ => format!(
            "pane {}",
            tenure.pane_id.as_deref().unwrap_or("an unknown pane")
        ),
    };
    format!("already coordinated: {holder}")
}

/// Another coordinator holds the repository: the claim was refused. The line
/// names the holder as the status read above does, never as the claim's own
/// message; a status read that fails or lists nothing yet names the repository,
/// since the claim is what decided.
fn claim_refused(herdr: &dyn Herdr, repo: &str) -> String {
    tenures(herdr, repo)
        .ok()
        .and_then(|tenures| tenures.into_iter().next())
        .map_or_else(
            || format!("already coordinated: repository {repo} is held by another coordinator"),
            |tenure| already_coordinated(&tenure),
        )
}

/// The repository `open` starts the coordinator in: `--repo`, else the
/// worktree the space records, else the one git root its panes share.
fn resolve_repository(
    space: &str,
    explicit: Option<&str>,
    recorded: Option<&str>,
    pane_cwds: &[String],
    git_root: impl Fn(&str) -> Option<String>,
    is_dir: impl Fn(&str) -> bool,
) -> Result<String, String> {
    if let Some(repo) = explicit {
        return is_dir(repo)
            .then(|| repo.to_owned())
            .ok_or_else(|| format!("--repo {repo} is not a directory"));
    }
    if let Some(recorded) = recorded {
        return is_dir(recorded)
            .then(|| recorded.to_owned())
            .ok_or_else(|| {
                format!("space {space} records repository {recorded}, which is not a directory")
            });
    }
    let mut roots: Vec<String> = Vec::new();
    for cwd in pane_cwds {
        if let Some(root) = git_root(cwd) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    match roots.len() {
        0 => Err(format!("space {space} has no repository")),
        1 => Ok(roots.remove(0)),
        _ => Err(format!(
            "space {space} has panes in several repositories: {}",
            roots.join(", ")
        )),
    }
}

/// The agent's name: `todo-<space>`, lowercased, everything but `[a-z0-9_-]`
/// turned into `-`, at most the 32 characters `agent start` takes. The user's
/// `todo` skill looks for that prefix.
fn derived_name(space: &str) -> String {
    const MAX: usize = 32;
    let mut name = String::from("todo-");
    for character in space.to_lowercase().chars() {
        if name.len() >= MAX {
            break;
        }
        name.push(
            if character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_')
            {
                character
            } else {
                '-'
            },
        );
    }
    name
}

/// The server's rule for an agent name (`agent start` refuses others).
fn valid_agent_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some('a'..='z'))
        && name.len() <= 32
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_')
        })
}

/// The agent's own arguments: the ones after `--`, else the defaults.
fn agent_arguments(options: &OpenOptions) -> Vec<String> {
    if !options.agent_args.is_empty() {
        return options.agent_args.clone();
    }
    default_agent_arguments(options.agent, options.model.as_deref(), &options.space)
}

/// What each agent gets when `--` gives nothing. The skills and the models
/// they run change outside this repository, so this table is the one place
/// to update: `claude` starts with Remote Control (visible in the Claude
/// app), `pi` with GPT-6.1 Sol, whose provider the usage gate reads as the
/// ChatGPT plan.
fn default_agent_arguments(agent: OpenAgent, model: Option<&str>, space: &str) -> Vec<String> {
    match agent {
        OpenAgent::Claude => vec!["--remote-control".to_owned(), format!("TODO {space}")],
        OpenAgent::Pi => vec![
            "--model".to_owned(),
            model.unwrap_or(PI_COORDINATOR_MODEL).to_owned(),
        ],
    }
}

/// Arguments as a person reads them, with those holding whitespace quoted.
fn join_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| {
            if argument.is_empty() || argument.chars().any(char::is_whitespace) {
                format!("\"{argument}\"")
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The git root `cwd` is in, as git in that directory finds it.
fn git_root(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|root| !root.is_empty())
}

fn spaces(herdr: &dyn Herdr) -> Result<Vec<Space>, Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:spaces",
        Method::WorkspaceList(EmptyParams::default()),
    )?;
    let result = result(&reply, "workspace.list")?;
    let Some(workspaces) = result.get("workspaces").and_then(|value| value.as_array()) else {
        return Err(unexpected("workspace.list"));
    };
    Ok(workspaces
        .iter()
        .map(|space| Space {
            workspace_id: string(space, "workspace_id"),
            label: string(space, "label"),
            worktree: space["worktree"]["checkout_path"]
                .as_str()
                .map(str::to_owned),
        })
        .collect())
}

/// The directories the space's panes run in: the process's own when herdr
/// knows it, else the pane's directory.
fn pane_cwds(herdr: &dyn Herdr, workspace_id: &str) -> Result<Vec<String>, Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:panes",
        Method::PaneList(PaneListParams {
            workspace_id: Some(workspace_id.to_owned()),
        }),
    )?;
    let result = result(&reply, "pane.list")?;
    let Some(panes) = result.get("panes").and_then(|value| value.as_array()) else {
        return Err(unexpected("pane.list"));
    };
    Ok(panes
        .iter()
        .filter_map(|pane| {
            pane["foreground_cwd"]
                .as_str()
                .or_else(|| pane["cwd"].as_str())
        })
        .map(str::to_owned)
        .collect())
}

fn tenures(herdr: &dyn Herdr, repo: &str) -> Result<Vec<Tenure>, Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:status",
        Method::CoordinatorStatus(CoordinatorStatusParams {
            repo: Some(repo.to_owned()),
        }),
    )?;
    let result = result(&reply, "coordinator.status")?;
    let Some(coordinators) = result
        .get("coordinators")
        .and_then(|value| value.as_array())
    else {
        return Err(unexpected("coordinator.status"));
    };
    Ok(coordinators
        .iter()
        .map(|tenure| Tenure {
            coordinator_id: string(tenure, "coordinator_id"),
            pane_id: tenure["pane_id"].as_str().map(str::to_owned),
            holder_kind: tenure["holder_kind"].as_str().map(str::to_owned),
            holder_pid: tenure["holder_pid"].as_u64().map(|pid| pid as u32),
        })
        .collect())
}

fn create_tab(herdr: &dyn Herdr, workspace_id: &str, repo: &str) -> Result<CreatedTab, Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:tab",
        Method::TabCreate(TabCreateParams {
            workspace_id: Some(workspace_id.to_owned()),
            cwd: Some(repo.to_owned()),
            focus: false,
            label: None,
            env: HashMap::new(),
        }),
    )?;
    let result = result(&reply, "tab.create")?;
    let (Some(tab_id), Some(pane_id)) = (
        result["tab"]["tab_id"].as_str(),
        result["root_pane"]["pane_id"].as_str(),
    ) else {
        return Err(unexpected("tab.create"));
    };
    Ok(CreatedTab {
        tab_id: tab_id.to_owned(),
        pane_id: pane_id.to_owned(),
        label: string(&result["tab"], "label"),
        number: result["tab"]["number"].as_u64().unwrap_or_default(),
    })
}

/// `tab role <tab> coordinator`: the claim of the repository's tenure, refused
/// `coordinator_active` while another one holds it.
fn claim(herdr: &dyn Herdr, tab_id: &str) -> Result<(), Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:claim",
        Method::TabSetRole(TabSetRoleParams {
            tab_id: tab_id.to_owned(),
            role: Some(TabRole::Coordinator),
        }),
    )?;
    result(&reply, "tab.set_role").map(|_| ())
}

/// `agent start` and the wait for its launch to end: the launch ends on an
/// event of the agent (its session report, a dialog, its exit), and a prompt
/// typed before it ends would be refused (`agent_not_ready`).
fn start_agent(
    herdr: &dyn Herdr,
    name: &str,
    kind: &str,
    pane_id: &str,
    argv: &[String],
) -> Result<(), Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:start",
        Method::AgentStart(AgentStartParams {
            name: name.to_owned(),
            kind: kind.to_owned(),
            pane_id: pane_id.to_owned(),
            args: argv.to_vec(),
            timeout_ms: None,
            prompt: None,
            // The agent coordinates a repository of its own: it is no worker
            // of the pane this command runs in, whose tenure must not own it.
            caller_pane_id: None,
        }),
    )?;
    let started = result(&reply, "agent.start")?;
    let (Some(terminal_id), Some(started_pane), Some(workspace_id)) = (
        started["agent"]["terminal_id"].as_str(),
        started["agent"]["pane_id"].as_str(),
        started["agent"]["workspace_id"].as_str(),
    ) else {
        return Err(unexpected("agent.start"));
    };
    let reply = herdr.request(
        "cli:coordinator:open:launch",
        Method::AgentWaitStart(AgentWaitStartParams {
            name: name.to_owned(),
            terminal_id: terminal_id.to_owned(),
            pane_id: started_pane.to_owned(),
            kind: kind.to_owned(),
            prefer_workspace_id: Some(workspace_id.to_owned()),
            timeout_ms: None,
        }),
    )?;
    result(&reply, "agent.wait_start").map(|_| ())
}

fn agent_facts(herdr: &dyn Herdr, pane_id: &str) -> Result<AgentFacts, Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:agent",
        Method::AgentGet(AgentTarget {
            target: pane_id.to_owned(),
            prefer_workspace_id: None,
        }),
    )?;
    let result = result(&reply, "agent.get")?;
    let agent = &result["agent"];
    Ok(AgentFacts {
        status: serde_json::from_value(agent["agent_status"].clone())
            .unwrap_or(AgentStatus::Unknown),
        state_change_seq: agent["state_change_seq"].as_u64().unwrap_or_default(),
    })
}

fn focus(herdr: &dyn Herdr, pane_id: &str) -> Result<(), Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:focus",
        Method::AgentFocus(AgentTarget {
            target: pane_id.to_owned(),
            prefer_workspace_id: None,
        }),
    )?;
    result(&reply, "agent.focus").map(|_| ())
}

/// `agent prompt`: it answers only once the agent accepted the prompt (or
/// refused it before typing), so its answer is the acknowledgement.
fn prompt(herdr: &dyn Herdr, pane_id: &str, text: &str) -> Result<(), Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:prompt",
        Method::AgentPromptConfirmed(AgentPromptConfirmedParams {
            target: pane_id.to_owned(),
            prefer_workspace_id: None,
            text: text.to_owned(),
            // Its own tenure's coordinator is the agent's, not this command's
            // caller: the prompt is a message, not a worker's assignment.
            caller_pane_id: None,
            note: false,
        }),
    )?;
    result(&reply, "agent.prompt").map(|_| ())
}

/// Ends the tenure this command claimed for its new tab's pane. The pane
/// names it, so a tenure that moved (a handoff, or a successor's ack) is not
/// found and never ended by mistake; nothing of this command's remains then,
/// so that answer is no cleanup failure.
fn end_tenure(herdr: &dyn Herdr, pane_id: &str) -> Result<(), Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:end",
        Method::CoordinatorEnd(CoordinatorEndParams {
            reason: Some("coordinator open stopped before it sent the TODO".to_owned()),
            coordinator_id: None,
            pane_id: Some(pane_id.to_owned()),
        }),
    )?;
    match result(&reply, "coordinator.end") {
        Ok(_) => Ok(()),
        Err(Failure::Server { code, .. }) if code == "coordinator_not_found" => Ok(()),
        Err(failure) => Err(failure),
    }
}

fn close_tab(herdr: &dyn Herdr, tab_id: &str) -> Result<(), Failure> {
    let reply = herdr.request(
        "cli:coordinator:open:close",
        Method::TabClose(TabTarget {
            tab_id: tab_id.to_owned(),
        }),
    )?;
    result(&reply, "tab.close").map(|_| ())
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

    /// A filesystem where every repository a test names is there.
    struct Everywhere;

    const EVERYWHERE: Everywhere = Everywhere;

    impl Paths for Everywhere {
        fn is_dir(&self, _path: &str) -> bool {
            true
        }
    }

    /// The `open` arguments of one test.
    fn options(argv: &[&str]) -> OpenOptions {
        parse_open(&args(argv))
            .expect("open parses")
            .expect("open is a subcommand")
    }

    fn words(argv: &[String]) -> Vec<&str> {
        argv.iter().map(String::as_str).collect()
    }

    #[test]
    fn open_parses_its_arguments_and_defaults() {
        let parsed = options(&["myrepo"]);
        assert_eq!(parsed.space, "myrepo");
        assert_eq!(parsed.repo, None);
        assert_eq!(parsed.agent, OpenAgent::Claude);
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.name, None);
        assert_eq!(parsed.prompt, None);
        assert!(parsed.agent_args.is_empty());
        assert!(!parsed.dry_run);

        let parsed = options(&[
            "myrepo",
            "--repo",
            "/repo",
            "--agent",
            "pi",
            "--model",
            "deepseek/deepseek-flash",
            "--name",
            "w-repo",
            "--prompt",
            "Do the TODO.",
            "--dry-run",
            "--",
            "--verbose",
        ]);
        assert_eq!(parsed.repo.as_deref(), Some("/repo"));
        assert_eq!(parsed.agent, OpenAgent::Pi);
        assert_eq!(parsed.model.as_deref(), Some("deepseek/deepseek-flash"));
        assert_eq!(parsed.name.as_deref(), Some("w-repo"));
        assert_eq!(parsed.prompt.as_deref(), Some("Do the TODO."));
        assert_eq!(words(&parsed.agent_args), ["--verbose"]);
        assert!(parsed.dry_run);

        assert!(matches!(parse_open(&args(&["--help"])), Ok(None)));
        // Help anywhere before `--`, not only as the first argument.
        assert!(matches!(parse_open(&args(&["myrepo", "--help"])), Ok(None)));
        assert!(matches!(
            parse_open(&args(&["myrepo", "--dry-run", "-h"])),
            Ok(None)
        ));
        // After `--` the arguments are the agent's, help included.
        let parsed = options(&["myrepo", "--", "--help"]);
        assert_eq!(words(&parsed.agent_args), ["--help"]);
        // `--model` is advertised for a pi worker but accepted with claude
        // too, which reports it instead of refusing the command.
        assert_eq!(
            options(&["myrepo", "--model", "opus"]).model.as_deref(),
            Some("opus")
        );
        for refused in [
            vec![],
            vec!["myrepo", "--agent", "codex"],
            vec!["myrepo", "--agent", "pi", "--model", ""],
            vec!["myrepo", "--name", "Bad Name"],
            vec!["myrepo", "--prompt", ""],
            vec!["myrepo", "other"],
            vec!["myrepo", "--nope"],
        ] {
            assert!(parse_open(&args(&refused)).is_err(), "{refused:?} parsed");
        }
    }

    #[test]
    fn open_names_the_agent_and_gives_it_the_arguments_of_its_table() {
        // The name the user's `todo` skill looks for: `todo-<space>`.
        assert_eq!(derived_name("My Repo!"), "todo-my-repo-");
        assert_eq!(derived_name("héllo"), "todo-h-llo");
        assert_eq!(
            derived_name(&"a".repeat(27)),
            format!("todo-{}", "a".repeat(27))
        );
        let truncated = derived_name(&"a".repeat(60));
        assert_eq!(truncated, format!("todo-{}", "a".repeat(27)));
        assert_eq!(truncated.len(), 32);
        assert!(valid_agent_name(&derived_name("1 weird space")));

        // The table of defaults the skills and the models live in.
        assert_eq!(
            words(&default_agent_arguments(OpenAgent::Claude, None, "myrepo")),
            ["--remote-control", "TODO myrepo"]
        );
        assert_eq!(
            words(&default_agent_arguments(OpenAgent::Pi, None, "myrepo")),
            ["--model", "openai-codex/gpt-6.1-sol"]
        );
        assert_eq!(
            words(&default_agent_arguments(
                OpenAgent::Pi,
                Some("deepseek/deepseek-flash"),
                "myrepo"
            )),
            ["--model", "deepseek/deepseek-flash"]
        );
        // The arguments after `--` replace the defaults.
        assert_eq!(
            words(&agent_arguments(&options(&[
                "myrepo", "--", "--model", "x"
            ]))),
            ["--model", "x"]
        );
    }

    #[test]
    fn open_resolves_the_repository_of_its_space() {
        let dirs = |_path: &str| true;
        let one = |_cwd: &str| Some("/pane".to_owned());
        // `--repo` wins over the worktree the space records.
        assert_eq!(
            resolve_repository("s", Some("/explicit"), Some("/recorded"), &[], one, dirs),
            Ok("/explicit".to_owned())
        );
        assert_eq!(
            resolve_repository("s", None, Some("/recorded"), &[], one, dirs),
            Ok("/recorded".to_owned())
        );
        // Without either: the one git root its panes share.
        assert_eq!(
            resolve_repository(
                "s",
                None,
                None,
                &["/a".to_owned(), "/b".to_owned()],
                one,
                dirs
            ),
            Ok("/pane".to_owned())
        );
        // Panes in several repositories are refused, and both are listed.
        let per_cwd = |cwd: &str| {
            Some(
                match cwd {
                    "/a" => "/one",
                    "/b" => "/two",
                    _ => "/three",
                }
                .to_owned(),
            )
        };
        let refused = resolve_repository(
            "s",
            None,
            None,
            &["/a".to_owned(), "/b".to_owned()],
            per_cwd,
            dirs,
        )
        .expect_err("several repositories are refused");
        assert!(refused.contains("several repositories"), "{refused}");
        assert!(
            refused.contains("/one") && refused.contains("/two"),
            "{refused}"
        );
        // No pane in a repository at all.
        assert_eq!(
            resolve_repository("s", None, None, &["/a".to_owned()], |_cwd| None, dirs),
            Err("space s has no repository".to_owned())
        );
        // A resolved path that is not a directory is refused.
        let gone = |_path: &str| false;
        assert!(resolve_repository("s", Some("/gone"), None, &[], one, gone).is_err());
        assert!(resolve_repository("s", None, Some("/gone"), &[], one, gone).is_err());
    }

    /// An answer a test scripts for one method.
    enum Answer {
        Reply(serde_json::Value),
        Refused(&'static str, &'static str),
        /// The request was made but its answer could not be read: a dropped
        /// connection or a timeout, after the server may have acted on it.
        Unreadable(&'static str),
    }

    /// A herdr that answers what a test scripts, in the order `open` asks.
    #[derive(Default)]
    struct Fake {
        answers: std::cell::RefCell<Vec<(&'static str, Answer)>>,
        sent: std::cell::RefCell<Vec<&'static str>>,
        /// The requests themselves, for a test of what they carried.
        methods: std::cell::RefCell<Vec<Method>>,
        notices: std::cell::RefCell<Vec<String>>,
        waited: std::cell::RefCell<Vec<(String, u64, std::time::Duration)>>,
        change: Option<Change>,
    }

    impl Fake {
        fn script(&self, method: &'static str, answer: Answer) {
            self.answers.borrow_mut().push((method, answer));
        }

        fn replies(&self, method: &'static str, result: serde_json::Value) {
            self.script(method, Answer::Reply(result));
        }

        fn sent(&self) -> Vec<&'static str> {
            self.sent.borrow().clone()
        }
    }

    /// The method a request names, as the test scripts it.
    fn method_key(method: &Method) -> &'static str {
        match method {
            Method::WorkspaceList(_) => "workspace.list",
            Method::PaneList(_) => "pane.list",
            Method::CoordinatorStatus(_) => "coordinator.status",
            Method::CoordinatorEnd(_) => "coordinator.end",
            Method::TabCreate(_) => "tab.create",
            Method::TabSetRole(_) => "tab.set_role",
            Method::TabClose(_) => "tab.close",
            Method::AgentStart(_) => "agent.start",
            Method::AgentWaitStart(_) => "agent.wait_start",
            Method::AgentGet(_) => "agent.get",
            Method::AgentFocus(_) => "agent.focus",
            Method::AgentPromptConfirmed(_) => "agent.prompt_confirmed",
            other => panic!("open sent a method this test does not know: {other:?}"),
        }
    }

    impl Herdr for Fake {
        fn request(&self, _id: &str, method: Method) -> Result<serde_json::Value, Failure> {
            let key = method_key(&method);
            self.sent.borrow_mut().push(key);
            self.methods.borrow_mut().push(method.clone());
            let position = self
                .answers
                .borrow()
                .iter()
                .position(|(name, _)| *name == key);
            let Some(position) = position else {
                panic!("no answer scripted for {key}");
            };
            let (_, answer) = self.answers.borrow_mut().remove(position);
            match answer {
                Answer::Reply(result) => Ok(serde_json::json!({"id": "test", "result": result})),
                Answer::Refused(code, message) => Err(Failure::Server {
                    code: code.to_owned(),
                    message: message.to_owned(),
                }),
                Answer::Unreadable(message) => Err(Failure::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    (*message).to_owned(),
                ))),
            }
        }

        fn wait_change(
            &self,
            pane_id: &str,
            after: u64,
            deadline: std::time::Duration,
        ) -> Result<Change, Failure> {
            self.sent.borrow_mut().push("agent.wait_change");
            self.waited
                .borrow_mut()
                .push((pane_id.to_owned(), after, deadline));
            Ok(self.change.unwrap_or(Change::Changed))
        }

        fn notice(&self, text: &str) {
            self.notices.borrow_mut().push(text.to_owned());
        }
    }

    fn space(label: &str, workspace_id: &str, worktree: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "workspace_id": workspace_id,
            "number": 1,
            "label": label,
            "focused": false,
            "pane_count": 1,
            "tab_count": 1,
            "active_tab_id": format!("{workspace_id}:1"),
            "agent_status": "idle",
            "worktree": worktree.map(|path| serde_json::json!({"checkout_path": path})),
        })
    }

    /// A herdr holding one space, at `/repo` unless it records a worktree.
    fn open_fake(space: serde_json::Value) -> Fake {
        let fake = Fake::default();
        fake.replies("workspace.list", serde_json::json!({"workspaces": [space]}));
        fake.replies("pane.list", serde_json::json!({"panes": []}));
        fake
    }

    /// The answers an `open` into a free repository gets, up to its claim.
    fn free_repository(fake: &Fake) {
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": []}),
        );
        fake.replies(
            "tab.create",
            serde_json::json!({
                "tab": {"tab_id": "w1:2", "number": 2, "label": ""},
                "root_pane": {"pane_id": "w1-2"},
            }),
        );
        fake.replies(
            "tab.set_role",
            serde_json::json!({"tab": {"tab_id": "w1:2"}}),
        );
    }

    /// The answers a launch that takes prompts gets.
    fn launched(fake: &Fake) {
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.replies(
            "agent.wait_start",
            serde_json::json!({"agent": {"agent_status": "idle"}}),
        );
    }

    fn agent_state(fake: &Fake, status: &str, seq: u64) {
        fake.replies(
            "agent.get",
            serde_json::json!({"agent": {"agent_status": status, "state_change_seq": seq}}),
        );
    }

    #[test]
    fn open_refuses_a_space_it_cannot_find_or_place() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        let Err(Failure::Refused(message)) = open_with(&fake, &EVERYWHERE, &options(&["other"]))
        else {
            panic!("an unknown space is refused");
        };
        assert_eq!(message, "no space named other");
        assert_eq!(fake.sent(), ["workspace.list"]);

        // Unrecorded and with no pane in a repository.
        let fake = open_fake(space("empty", "w1", None));
        let Err(Failure::Refused(message)) = open_with(&fake, &EVERYWHERE, &options(&["empty"]))
        else {
            panic!("a space without a repository is refused");
        };
        assert_eq!(message, "space empty has no repository");
        assert!(!fake.sent().contains(&"tab.create"));
    }

    #[test]
    fn dry_run_prints_the_plan_and_starts_nothing() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        let outcome =
            open_with(&fake, &EVERYWHERE, &options(&["myrepo", "--dry-run"])).expect("plan");
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text,
            "would start claude as todo-myrepo in space myrepo, repository /repo, argv: \
             --remote-control \"TODO myrepo\", prompt: Rób TODO po kolei."
        );
        // Reads only: no tab, no claim, no agent.
        assert_eq!(fake.sent(), ["workspace.list"]);
        assert!(fake.notices.borrow().is_empty());

        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        let outcome = open_with(
            &fake,
            &EVERYWHERE,
            &options(&["myrepo", "--agent", "pi", "--dry-run"]),
        )
        .expect("plan");
        assert!(
            outcome
                .text
                .contains("argv: --model openai-codex/gpt-6.1-sol"),
            "{}",
            outcome.text
        );
    }

    #[test]
    fn open_starts_the_coordinator_and_names_what_it_started() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        launched(&fake);
        agent_state(&fake, "idle", 7);
        fake.replies(
            "agent.prompt_confirmed",
            serde_json::json!({"agent": {"agent_status": "working"}}),
        );

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text,
            "started claude as todo-myrepo in space myrepo, tab 2, pane w1-2, repository /repo"
        );
        assert_eq!(
            fake.sent(),
            [
                "workspace.list",
                "coordinator.status",
                "tab.create",
                "tab.set_role",
                "agent.start",
                "agent.wait_start",
                "agent.get",
                "agent.prompt_confirmed",
            ]
        );
        assert!(fake.notices.borrow().is_empty());
        // The started agent is no worker of the caller's tenure, and the
        // prompt is a message rather than a worker's assignment.
        for method in fake.methods.borrow().iter() {
            match method {
                Method::AgentStart(params) => assert_eq!(params.caller_pane_id, None),
                Method::AgentPromptConfirmed(params) => assert_eq!(params.caller_pane_id, None),
                _ => {}
            }
        }
    }

    #[test]
    fn a_coordinated_repository_is_left_as_it_is() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": [{
                "coordinator_id": "c-abcd2345",
                "pane_id": "w1-9",
                "holder_kind": "pane",
            }]}),
        );
        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.text, "already coordinated: pane w1-9");
        assert_eq!(fake.sent(), ["workspace.list", "coordinator.status"]);

        // A claim another open won: the tab this one made is closed again, so
        // two opens leave one tab and one coordinator.
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": []}),
        );
        fake.replies(
            "tab.create",
            serde_json::json!({
                "tab": {"tab_id": "w1:2", "number": 2, "label": ""},
                "root_pane": {"pane_id": "w1-2"},
            }),
        );
        fake.script(
            "tab.set_role",
            Answer::Refused(
                "coordinator_active",
                "repository /repo already has coordinator c-abcd2345 in pane w1-9",
            ),
        );
        fake.replies("tab.close", serde_json::json!({}));
        // The holder is read again, as the status path reads it: the claim's
        // own wording is never printed.
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": [{
                "coordinator_id": "c-abcd2345",
                "pane_id": "w1-9",
                "holder_kind": "pane",
            }]}),
        );
        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.text, "already coordinated: pane w1-9");
        assert!(
            !outcome.text.contains("repository /repo already has"),
            "{}",
            outcome.text
        );
        assert_eq!(fake.sent().last(), Some(&"coordinator.status"));
        assert!(!fake.sent().contains(&"agent.start"));

        // A tenure that ended between the refusal and the read: the line names
        // the repository, since nothing else is left to name.
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": []}),
        );
        fake.replies(
            "tab.create",
            serde_json::json!({
                "tab": {"tab_id": "w1:2", "number": 2, "label": ""},
                "root_pane": {"pane_id": "w1-2"},
            }),
        );
        fake.script(
            "tab.set_role",
            Answer::Refused("coordinator_active", "repository /repo is coordinated"),
        );
        fake.replies("tab.close", serde_json::json!({}));
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": []}),
        );
        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text,
            "already coordinated: repository /repo is held by another coordinator"
        );
    }

    #[test]
    fn a_tab_that_stays_open_is_reported_with_the_claim_refusal() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": []}),
        );
        fake.replies(
            "tab.create",
            serde_json::json!({
                "tab": {"tab_id": "w1:2", "number": 2, "label": ""},
                "root_pane": {"pane_id": "w1-2"},
            }),
        );
        fake.script(
            "tab.set_role",
            Answer::Refused(
                "coordinator_active",
                "repository /repo already has coordinator c-abcd2345 in pane w1-9",
            ),
        );
        fake.script(
            "tab.close",
            Answer::Refused("tab_not_found", "tab w1:2 is gone"),
        );
        fake.replies(
            "coordinator.status",
            serde_json::json!({"coordinators": [{
                "coordinator_id": "c-abcd2345",
                "pane_id": "w1-9",
                "holder_kind": "pane",
            }]}),
        );

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text,
            "already coordinated: pane w1-9; tab 2 is still open: tab_not_found: tab w1:2 is gone"
        );
    }

    #[test]
    fn a_startup_dialog_is_the_users_to_answer() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.script(
            "agent.wait_start",
            Answer::Refused(
                "agent_not_ready",
                "agent todo-myrepo is blocked during startup by Claude's folder-trust dialog",
            ),
        );
        agent_state(&fake, "blocked", 7);
        fake.replies("agent.focus", serde_json::json!({"agent": {}}));
        // Answered, the agent reads as ready to take the prompt.
        agent_state(&fake, "idle", 9);
        fake.replies(
            "agent.prompt_confirmed",
            serde_json::json!({"agent": {"agent_status": "working"}}),
        );

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 0);
        assert!(
            outcome.text.starts_with("started claude as todo-myrepo"),
            "{}",
            outcome.text
        );
        // The user is told where to answer before the wait, and the wait is
        // the agent's next state change, from the seq agent.get showed.
        let notices = fake.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("pane w1-2"), "{}", notices[0]);
        assert!(notices[0].contains("folder-trust"), "{}", notices[0]);
        let waited = fake.waited.borrow();
        assert_eq!(waited.len(), 1);
        assert_eq!(waited[0].0, "w1-2");
        assert_eq!(waited[0].1, 7);
        assert_eq!(waited[0].2, DIALOG_ANSWER_DEADLINE);
        assert!(fake.sent().contains(&"agent.prompt_confirmed"));
    }

    #[test]
    fn an_unanswered_dialog_leaves_the_tab_and_its_tenure() {
        let mut fake = open_fake(space("myrepo", "w1", Some("/repo")));
        fake.change = Some(Change::TimedOut);
        free_repository(&fake);
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.script(
            "agent.wait_start",
            Answer::Refused(
                "agent_not_ready",
                "agent todo-myrepo is blocked during startup",
            ),
        );
        agent_state(&fake, "blocked", 7);
        fake.replies("agent.focus", serde_json::json!({"agent": {}}));

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 1);
        assert!(
            outcome.text.contains("not answered within 10 minutes"),
            "{}",
            outcome.text
        );
        assert!(outcome.text.contains("pane w1-2"), "{}", outcome.text);
        // The tab and the tenure are the user's to answer in.
        assert!(!fake.sent().contains(&"agent.prompt_confirmed"));
        assert!(!fake.sent().contains(&"coordinator.end"));
        assert!(!fake.sent().contains(&"tab.close"));
    }

    #[test]
    fn a_failed_start_ends_the_tenure_and_closes_the_tab() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.script(
            "agent.start",
            Answer::Refused(
                "agent_pane_busy",
                "agent target pane w1-2 is not an available shell",
            ),
        );
        fake.replies("coordinator.end", serde_json::json!({"coordinator": {}}));
        fake.replies("tab.close", serde_json::json!({}));

        let Err(Failure::Server { code, message }) =
            open_with(&fake, &EVERYWHERE, &options(&["myrepo"]))
        else {
            panic!("a failed start is a failure");
        };
        assert_eq!(code, "agent_pane_busy");
        assert!(message.contains("w1-2"), "{message}");
        let sent = fake.sent();
        let ended = sent.iter().position(|method| *method == "coordinator.end");
        let closed = sent.iter().position(|method| *method == "tab.close");
        assert!(ended.is_some(), "{sent:?}");
        assert!(closed > ended, "{sent:?}");
        // The tenure is ended by the new tab's pane, not by an id an earlier
        // read found: one a successor took over is then not found and kept.
        let end = fake
            .methods
            .borrow()
            .iter()
            .find_map(|method| match method {
                Method::CoordinatorEnd(params) => Some(params.clone()),
                _ => None,
            })
            .expect("the tenure is ended");
        assert_eq!(end.pane_id.as_deref(), Some("w1-2"));
        assert_eq!(end.coordinator_id, None);
        // The cleanup reads no status: the pane names the tenure it ends, so a
        // tenure a successor took over is not found and never ended here.
        let claimed = sent
            .iter()
            .position(|method| *method == "tab.set_role")
            .expect("the tab is claimed");
        assert!(!sent[claimed..].contains(&"coordinator.status"), "{sent:?}");
        assert!(!sent.contains(&"agent.prompt_confirmed"));
    }

    #[test]
    fn a_cleanup_that_fails_is_reported_with_the_step_that_failed() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.script(
            "agent.start",
            Answer::Refused("agent_pane_busy", "pane w1-2 is not an available shell"),
        );
        fake.script(
            "coordinator.end",
            Answer::Refused(
                "worker_store_unavailable",
                "the worker store cannot be read",
            ),
        );
        fake.script(
            "tab.close",
            Answer::Refused("tab_not_found", "tab w1:2 is gone"),
        );

        let Err(Failure::Server { code, message }) =
            open_with(&fake, &EVERYWHERE, &options(&["myrepo"]))
        else {
            panic!("a failed start is a failure");
        };
        assert_eq!(code, "agent_pane_busy");
        assert!(
            message.contains("the tenure of pane w1-2 did not end: worker_store_unavailable"),
            "{message}"
        );
        assert!(
            message.contains("tab 2 is still open: tab_not_found"),
            "{message}"
        );
        // A tenure the cleanup did not find is nothing of this command's left.
        assert!(!message.contains("coordinator_not_found"));
    }

    #[test]
    fn a_launch_stopped_at_a_dialog_prompts_once_the_agent_takes_input() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.script(
            "agent.wait_start",
            Answer::Refused(
                "agent_not_ready",
                "agent todo-myrepo is blocked during startup",
            ),
        );
        agent_state(&fake, "blocked", 7);
        fake.replies("agent.focus", serde_json::json!({"agent": {}}));
        // A second dialog: the wait changed the state, and the read shows one
        // that takes no prompt, so none is typed.
        agent_state(&fake, "blocked", 9);
        fake.replies("agent.focus", serde_json::json!({"agent": {}}));

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 1);
        assert!(outcome.text.contains("pane w1-2"), "{}", outcome.text);
        assert!(outcome.text.contains("tab 2"), "{}", outcome.text);
        assert!(
            outcome.text.contains("blocked on a dialog again"),
            "{}",
            outcome.text
        );
        assert!(!fake.sent().contains(&"agent.prompt_confirmed"));
        // The tab and the tenure are the user's to answer in.
        assert!(!fake.sent().contains(&"coordinator.end"));
        assert!(!fake.sent().contains(&"tab.close"));
    }

    #[test]
    fn an_agent_that_left_after_its_dialog_is_reported_not_prompted() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.script(
            "agent.wait_start",
            Answer::Refused(
                "agent_not_ready",
                "agent todo-myrepo is blocked during startup",
            ),
        );
        agent_state(&fake, "blocked", 7);
        fake.replies("agent.focus", serde_json::json!({"agent": {}}));
        // The trust prompt was refused: the agent is gone.
        fake.script(
            "agent.get",
            Answer::Refused("agent_not_found", "pane w1-2 has no agent"),
        );

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 1);
        assert!(outcome.text.contains("left pane w1-2"), "{}", outcome.text);
        assert!(
            outcome
                .text
                .contains("agent_not_found: pane w1-2 has no agent"),
            "{}",
            outcome.text
        );
        assert!(!fake.sent().contains(&"agent.prompt_confirmed"));
        assert!(!fake.sent().contains(&"coordinator.end"));
        assert!(!fake.sent().contains(&"tab.close"));
    }

    #[test]
    fn a_launch_refused_not_ready_without_a_dialog_reports_the_launch() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.script(
            "agent.wait_start",
            Answer::Refused(
                "agent_not_ready",
                "agent todo-myrepo did not report a session",
            ),
        );
        // The agent is not at a dialog and not ready: it exited, or never
        // became an agent at all.
        agent_state(&fake, "unknown", 4);
        fake.replies("coordinator.end", serde_json::json!({"coordinator": {}}));
        fake.replies("tab.close", serde_json::json!({}));

        let Err(Failure::Server { code, message }) =
            open_with(&fake, &EVERYWHERE, &options(&["myrepo"]))
        else {
            panic!("a refused launch is a failure");
        };
        assert_eq!(code, "agent_not_ready");
        assert_eq!(message, "agent todo-myrepo did not report a session");
        assert!(!fake.sent().contains(&"agent.prompt_confirmed"));
        assert!(fake.sent().contains(&"coordinator.end"));
        assert!(fake.sent().contains(&"tab.close"));
    }

    #[test]
    fn a_launch_refused_for_an_agent_that_is_gone_reports_the_launch() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        fake.replies(
            "agent.start",
            serde_json::json!({"agent": {
                "terminal_id": "t1", "pane_id": "w1-2", "workspace_id": "w1",
            }}),
        );
        fake.script(
            "agent.wait_start",
            Answer::Refused(
                "agent_not_ready",
                "agent todo-myrepo is blocked during startup by a folder-trust dialog",
            ),
        );
        fake.script(
            "agent.get",
            Answer::Refused("agent_not_found", "pane w1-2 has no agent"),
        );
        fake.replies("coordinator.end", serde_json::json!({"coordinator": {}}));
        fake.replies("tab.close", serde_json::json!({}));

        let Err(Failure::Server { code, message }) =
            open_with(&fake, &EVERYWHERE, &options(&["myrepo"]))
        else {
            panic!("a refused launch is a failure");
        };
        // The launch's own message, not the read that found no agent.
        assert_eq!(code, "agent_not_ready");
        assert_eq!(
            message,
            "agent todo-myrepo is blocked during startup by a folder-trust dialog"
        );
        assert!(!fake.sent().contains(&"agent.prompt_confirmed"));
        assert!(!fake.sent().contains(&"agent.focus"));
    }

    #[test]
    fn a_lost_prompt_answer_leaves_the_tab_and_its_tenure() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        free_repository(&fake);
        launched(&fake);
        agent_state(&fake, "idle", 7);
        fake.script(
            "agent.prompt_confirmed",
            Answer::Unreadable("the answer to agent.prompt could not be read"),
        );

        let outcome = open_with(&fake, &EVERYWHERE, &options(&["myrepo"])).expect("open");
        assert_eq!(outcome.code, 1);
        assert!(
            outcome.text.contains("whether it was accepted is unknown"),
            "{}",
            outcome.text
        );
        assert!(
            outcome.text.contains("it is not sent again"),
            "{}",
            outcome.text
        );
        assert!(outcome.text.contains("pane w1-2"), "{}", outcome.text);
        // A prompt may have been typed, so nothing of the tab is undone and
        // the prompt is never sent twice.
        assert_eq!(
            fake.sent()
                .iter()
                .filter(|method| **method == "agent.prompt_confirmed")
                .count(),
            1
        );
        assert!(!fake.sent().contains(&"coordinator.end"));
        assert!(!fake.sent().contains(&"tab.close"));
    }

    #[test]
    fn a_model_given_to_claude_is_reported_not_dropped() {
        let fake = open_fake(space("myrepo", "w1", Some("/repo")));
        let outcome = open_with(
            &fake,
            &EVERYWHERE,
            &options(&["myrepo", "--model", "opus", "--dry-run"]),
        )
        .expect("plan");
        assert_eq!(outcome.code, 0);
        assert!(
            outcome.text.contains("argv: --remote-control"),
            "{}",
            outcome.text
        );
        assert!(!outcome.text.contains("opus"), "{}", outcome.text);
        let notices = fake.notices.borrow();
        assert_eq!(notices.len(), 1);
        assert!(
            notices[0].contains("claude takes no --model"),
            "{}",
            notices[0]
        );
        assert!(notices[0].contains("opus"), "{}", notices[0]);
    }
}
