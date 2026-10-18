//! `herdr handoff`: per-session handoff notes, one per agent session, outside
//! any repository, and the claims that hand a note from one session to the
//! next.
//!
//! A note lives at `<state dir>/handoffs/sessions/<agent>/<session id>.md`,
//! because a session may work in several repositories and session ids are
//! unique only per agent. The session id is the environment
//! (`$PI_SESSION_ID` or `$CLAUDE_CODE_SESSION_ID`); inside a herdr pane the
//! agent session the server reports for that pane must agree with it, and
//! nothing is claimed or written when they disagree.
//!
//! `claim` creates the note's claim file exclusively and whole, so exactly one
//! session wins and the others are told who holds it. Every mutation (a claim,
//! `--break`, `done`, `forget`) holds the note's claim guard, an operating
//! system advisory lock on `<note>.claim.guard`: a contested mutation is
//! refused, naming the guard, and never retried. The kernel drops the lock
//! with its process, so a crashed mutation leaves no stale guard and nothing
//! is ever recovered by age, a timer or a retry. Nothing expires on a timer:
//! `--break` replaces a claimant only when its process is provably gone (the
//! recorded start token is no longer that pid's, or the pid does not run).
//!
//! Notes are per host: they live in this host's state directory, and a claim
//! recorded on another host is treated as held and is never replaced from
//! here. `herdr handoff` is not `herdr agent handoff` (an agent session
//! changing kind) and not `herdr coordinator handoff` (a coordination tenure
//! moving to a successor).
//!
//! The note's first lines, which `show` and `take` read, are roagent's:
//!
//! ```text
//! Status: pending
//! Owner: <agent> <session id>
//! Repositories: <worktree path>[, <worktree path>...]
//! ```

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::schema::{EmptyParams, Method, Request};

const USAGE: &str = "usage:
  herdr handoff path
      Print this session's handoff note path, creating its directory. The
      note itself is a file this session writes; nothing is created here.
  herdr handoff claim [--break] PATH
      Take PATH's note over: the claim file is created exclusively, so
      exactly one session wins (exit 1 names the holder otherwise). --break
      replaces a claimant that is provably gone (its process no longer
      runs, or it is an earlier session of this same process); a live
      claimant is never replaced.
  herdr handoff done PATH
      The claimant finished the note: remove the note and its claim, the
      claim last. Only the claiming session may (exit 1 otherwise).
  herdr handoff forget PATH
      Remove a note nobody will take (a person's call), with its claim.
  herdr handoff show [--repo DIR]
      List the notes with their status, claim, owner and repositories;
      --repo keeps those naming DIR's repository (any of its worktrees).
      Exits 3 when there is none.
  herdr handoff take [--repo DIR]
      Claim the single pending, unclaimed note of DIR's repository (default:
      the current directory): one is claimed, several are listed newest
      first (exit 4), none exits 3. A note a coordinator handoff waits for
      is skipped; take that handoff first.

The note starts with these lines, which show and take read:

    Status: pending
    Owner: <agent> <session id>
    Repositories: <worktree path>[, <worktree path>...]

The session id comes from $PI_SESSION_ID (pi) or $CLAUDE_CODE_SESSION_ID
(Claude Code), and inside a herdr pane it must agree with the agent session
the server has for that pane. Notes are per host, under this host's state
directory: a claim made on another host is never replaced from here.
This command is not `herdr agent handoff` (an agent session changing kind)
and not `herdr coordinator handoff` (a coordination tenure moving to a
successor).";

/// The status a note must have to be taken.
const PENDING: &str = "pending";
/// How many of a note's first lines its header is read from.
const HEADER_LINES: usize = 10;
/// The session variables and the agent each names, in the order they are read.
const SESSION_VARS: [(&str, &str); 2] = [
    ("PI_SESSION_ID", "pi"),
    ("CLAUDE_CODE_SESSION_ID", "claude"),
];
/// How far the walk to the agent process goes: process trees are shallow.
const MAX_ANCESTORS: usize = 32;

pub(super) fn run_handoff_command(args: &[String]) -> std::io::Result<i32> {
    let command = match parse(args) {
        Ok(Some(command)) => command,
        Ok(None) => {
            println!("{USAGE}");
            return Ok(0);
        }
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    let root = notes_root(&crate::config::state_dir());
    let view = View::system();
    let outcome = match command {
        Command::Path => command_path(&root),
        Command::Claim { path, breaking } => run_claim(&root, &path, breaking, &view),
        Command::Done(path) => run_done(&root, &path, &view),
        Command::Forget(path) => run_forget(&root, &path, &view),
        Command::Show { repository } => show(&root, repository.as_deref().map(Path::new), &view),
        Command::Take { repository } => run_take(&root, repository.as_deref(), &view),
    };
    match outcome {
        Ok(outcome) => {
            if let Some(text) = outcome.text {
                println!("{text}");
            }
            Ok(outcome.code)
        }
        Err(message) => {
            eprintln!("herdr handoff: {message}");
            Ok(1)
        }
    }
}

/// One subcommand.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Path,
    Claim { path: String, breaking: bool },
    Done(String),
    Forget(String),
    Show { repository: Option<String> },
    Take { repository: Option<String> },
}

/// The arguments after `handoff`: `None` asks for the usage text.
fn parse(args: &[String]) -> Result<Option<Command>, String> {
    let Some(name) = args.first() else {
        return Ok(None);
    };
    let rest = &args[1..];
    let command = match name.as_str() {
        "path" => {
            if let Some(argument) = rest.first() {
                return Err(format!("path takes no arguments, not {argument}"));
            }
            Command::Path
        }
        "claim" => {
            let mut breaking = false;
            let mut path = None;
            for argument in rest {
                match argument.as_str() {
                    "--break" if !breaking => breaking = true,
                    other if path.is_none() && !other.starts_with("--") => {
                        path = Some(other.to_string());
                    }
                    other => return Err(format!("unexpected argument: {other}")),
                }
            }
            Command::Claim {
                path: path.ok_or_else(|| "claim needs a note path".to_string())?,
                breaking,
            }
        }
        "done" | "forget" => {
            let [path] = rest else {
                return Err(format!("{name} takes one note path"));
            };
            if path.starts_with("--") {
                return Err(format!("unexpected argument: {path}"));
            }
            if name == "done" {
                Command::Done(path.clone())
            } else {
                Command::Forget(path.clone())
            }
        }
        "show" | "take" => {
            let mut repository = None;
            let mut index = 0;
            while index < rest.len() {
                match rest[index].as_str() {
                    "--repo" if repository.is_none() && index + 1 < rest.len() => {
                        repository = Some(rest[index + 1].clone());
                        index += 2;
                    }
                    other => return Err(format!("unexpected argument: {other}")),
                }
            }
            if name == "show" {
                Command::Show { repository }
            } else {
                Command::Take { repository }
            }
        }
        "help" | "--help" | "-h" => return Ok(None),
        other => return Err(format!("unknown subcommand: {other}")),
    };
    Ok(Some(command))
}

/// What a subcommand prints and exits with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    /// The lines it prints, if any.
    text: Option<String>,
    /// 0 success, 1 refused, 3 no note, 4 several notes.
    code: i32,
}

impl Outcome {
    fn new(code: i32, text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            code,
        }
    }

    fn silent(code: i32) -> Self {
        Self { text: None, code }
    }
}

/// This session: the agent that runs it and its session id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    agent: &'static str,
    id: String,
}

impl Session {
    /// The `session` a claim records: the agent and the id, so a claim is
    /// never mistaken for another agent's session of the same id.
    fn claimed_as(&self) -> String {
        format!("{} {}", self.agent, self.id)
    }

    /// The variable the session id came from.
    fn variable(&self) -> &'static str {
        SESSION_VARS
            .iter()
            .find(|(_, agent)| *agent == self.agent)
            .map(|(name, _)| *name)
            .unwrap_or("a session variable")
    }

    /// Whether a session the server reported for this pane is this one.
    fn matches(&self, reported: &Reported) -> bool {
        if reported
            .agent
            .as_deref()
            .is_some_and(|agent| agent != self.agent)
        {
            return false;
        }
        if reported.is_path {
            // pi reports its session as the transcript's path, which carries
            // the session id in its file name.
            let name = reported
                .value
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&reported.value);
            name.contains(&self.id)
        } else {
            reported.value == self.id
        }
    }
}

/// The session of this command, from its environment.
fn session_from_env(variable: impl Fn(&str) -> Option<String>) -> Result<Session, String> {
    for (name, agent) in SESSION_VARS {
        let Some(id) = variable(name).filter(|id| !id.is_empty()) else {
            continue;
        };
        if !plain_session_id(&id) {
            return Err(format!("${name} is not a plain session id: {id:?}"));
        }
        return Ok(Session { agent, id });
    }
    Err("not run by a pi or Claude Code session (neither $PI_SESSION_ID nor $CLAUDE_CODE_SESSION_ID is set)".into())
}

/// A session id is one plain file name, so a note path never leaves its
/// agent's directory: an ASCII letter or digit first, then letters, digits,
/// `.`, `_` and `-`.
fn plain_session_id(id: &str) -> bool {
    let mut characters = id.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
}

/// This session, checked against the pane the server reports when it answers:
/// the environment stays the identity when the pane is closed or the server is
/// stopped, but a disagreement with the pane is never resolved silently.
fn session() -> Result<Session, String> {
    let session = session_from_env(|name| std::env::var(name).ok())?;
    if let Some(pane) = super::target::caller_pane_id() {
        if let Some(reported) = reported_session(&pane) {
            if !session.matches(&reported) {
                return Err(format!(
                    "the agent session herdr has for pane {pane} is {} ({}), not {} {}: refusing to use this session's note",
                    reported.value,
                    reported.agent.as_deref().unwrap_or("?"),
                    session.agent,
                    session.id
                ));
            }
        }
    }
    Ok(session)
}

/// The agent session the server has for `pane`, when it answers.
fn reported_session(pane: &str) -> Option<Reported> {
    let response = super::send_request_unchecked(&Request {
        id: "cli:handoff:agents".into(),
        method: Method::AgentList(EmptyParams::default()),
    })
    .ok()?;
    let agents = response.get("result")?.get("agents")?.as_array()?;
    let agent = agents
        .iter()
        .find(|agent| agent.get("pane_id").and_then(|id| id.as_str()) == Some(pane))?;
    let session = agent.get("agent_session")?;
    Some(Reported {
        agent: agent
            .get("agent")
            .and_then(|agent| agent.as_str())
            .map(str::to_string),
        value: session.get("value")?.as_str()?.to_string(),
        is_path: session
            .get("kind")
            .and_then(|kind| kind.as_str())
            .is_some_and(|kind| kind.eq_ignore_ascii_case("path")),
    })
}

/// An agent session a pane reported.
struct Reported {
    agent: Option<String>,
    value: String,
    /// The session is named by its transcript's path, not by its id.
    is_path: bool,
}

/// This session's claim and the identity it rests on.
fn session_and_claim() -> Result<(Session, Claim), String> {
    let session = session()?;
    let claim = claimer(&session)?;
    Ok((session, claim))
}

/// This session's claim: the agent process it runs in with the start token
/// that tells a later process from it, its pane, and this host.
fn claimer(session: &Session) -> Result<Claim, String> {
    let (pid, agent) = agent_process()?;
    let agent = claiming_agent(pid, &agent, session)?;
    Ok(Claim {
        pid,
        start: crate::platform::process_start_token(pid),
        agent,
        pane: super::target::caller_pane_id(),
        session: session.claimed_as(),
        host: crate::platform::hostname(),
        nonce: crate::platform::random_hex_token().ok(),
    })
}

/// The agent a claim is made for: the agent process's own, which must be the
/// agent the environment's session id names. A session that inherited the
/// other agent's variable would otherwise claim under an agent it does not
/// run as, so it is refused rather than choosing one silently.
fn claiming_agent(pid: u32, process_agent: &str, session: &Session) -> Result<String, String> {
    if process_agent != session.agent {
        return Err(format!(
            "this agent process ({pid}) runs {process_agent}, but ${} names {}: refusing to claim \
             for another agent's session",
            session.variable(),
            session.agent
        ));
    }
    Ok(process_agent.to_string())
}

/// The nearest ancestor herdr recognizes as an agent: the process a claim
/// belongs to, because it outlives the command that makes the claim.
fn agent_process() -> Result<(u32, String), String> {
    let mut parent = crate::platform::process_parent(std::process::id());
    for _ in 0..MAX_ANCESTORS {
        let Some(pid) = parent.filter(|pid| *pid > 1) else {
            break;
        };
        if let Some(agent) = agent_of_process(pid) {
            return Ok((pid, crate::detect::agent_label(agent).to_string()));
        }
        parent = crate::platform::process_parent(pid).filter(|next| *next != pid);
    }
    Err(
        "no agent process (claude, pi or another agent herdr knows) among this command's ancestors"
            .into(),
    )
}

/// The agent a process runs: the name it calls itself (pi runs as node and
/// names itself `pi`), else its executable's name, else the agent script a
/// generic runtime was started with.
fn agent_of_process(pid: u32) -> Option<crate::detect::Agent> {
    if let Some(agent) = crate::platform::process_argv0(pid)
        .as_deref()
        .and_then(crate::detect::identify_agent)
    {
        return Some(agent);
    }
    if let Some(agent) = crate::platform::process_name(pid)
        .as_deref()
        .and_then(crate::detect::identify_agent)
    {
        return Some(agent);
    }
    crate::platform::process_argv(pid)
        .as_deref()
        .and_then(crate::detect::identify_agent_in_argv)
}

/// A note's claim: the agent process and session that took the note over.
/// `start` is the process's start token, so a later process that reuses its
/// pid is not the claimant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Claim {
    pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start: Option<u64>,
    agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pane: Option<String>,
    session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
}

/// What a note's claim file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// No claim file: the note is unclaimed.
    None,
    Claim(Claim),
    /// A claim file that cannot be read: held until a person removes it.
    Unreadable,
}

/// What a mutation guard file holds: the process that holds it.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Guard {
    pid: u32,
    #[serde(default)]
    start: Option<u64>,
    #[serde(default)]
    host: Option<String>,
}

/// The platform a claim is checked against: the host that recorded it and the
/// process checks.
struct View<'a> {
    host: Option<String>,
    processes: &'a dyn Processes,
}

impl View<'static> {
    fn system() -> Self {
        static SYSTEM: System = System;
        Self {
            host: crate::platform::hostname(),
            processes: &SYSTEM,
        }
    }
}

/// The operating system's processes.
trait Processes: Send + Sync {
    fn exists(&self, pid: u32) -> bool;
    fn start(&self, pid: u32) -> Option<u64>;
}

struct System;

impl Processes for System {
    fn exists(&self, pid: u32) -> bool {
        crate::platform::process_exists(pid)
    }

    fn start(&self, pid: u32) -> Option<u64> {
        crate::platform::process_start_token(pid)
    }
}

/// Whether the claim's process still runs on the host that recorded it. A
/// claim of another host cannot be checked here, so it counts as held.
fn alive(claim: &Claim, view: &View<'_>) -> bool {
    if claim.host != view.host {
        return true;
    }
    match claim.start {
        Some(start) => {
            view.processes.exists(claim.pid) && view.processes.start(claim.pid) == Some(start)
        }
        // A claim without a start token cannot be told from a later process
        // that reuses its pid, so its pid alone says whether it is held.
        None => view.processes.exists(claim.pid),
    }
}

/// The same process, by pid and start token; a claim recorded without one is
/// the same holder by pid.
fn same_holder(a: &Claim, b: &Claim) -> bool {
    a.pid == b.pid
        && match (a.start, b.start) {
            (Some(left), Some(right)) => left == right,
            (None, _) => true,
            (Some(_), None) => false,
        }
}

fn same_claimer(held: &Claim, me: &Claim) -> bool {
    same_holder(held, me) && held.session == me.session
}

/// Whether the claim may be replaced: it is gone, or it is an earlier session
/// of this same process (a new session in it is someone else).
fn breakable(held: &Held, me: &Claim, view: &View<'_>) -> bool {
    let Held::Claim(held) = held else {
        return false;
    };
    if !alive(held, view) {
        return true;
    }
    held.host == view.host && same_holder(held, me) && held.session != me.session
}

/// Who holds the note, as `show` and a refusal name it.
fn describe(held: &Held, view: &View<'_>) -> String {
    let Held::Claim(claim) = held else {
        return "an unreadable claim".into();
    };
    let session = if claim.session.is_empty() {
        claim.agent.as_str()
    } else {
        claim.session.as_str()
    };
    let pane = claim
        .pane
        .as_deref()
        .map(|pane| format!(", herdr pane {pane}"))
        .unwrap_or_default();
    let gone = if alive(claim, view) { "" } else { ", gone" };
    format!("{session}, pid {}{pane}{gone}", claim.pid)
}

/// Where the notes live: `<state dir>/handoffs/sessions`, in herdr's state
/// directory and never inside a repository.
fn notes_root(state_dir: &Path) -> PathBuf {
    state_dir.join("handoffs").join("sessions")
}

/// This session's note, whose directory `path` creates.
fn session_note(root: &Path, session: &Session) -> PathBuf {
    root.join(session.agent).join(format!("{}.md", session.id))
}

fn claim_path(note: &Path) -> PathBuf {
    sibling(note, ".claim")
}

fn guard_path(note: &Path) -> PathBuf {
    sibling(note, ".claim.guard")
}

/// `note` with `suffix` appended to its name, as roagent writes both.
fn sibling(note: &Path, suffix: &str) -> PathBuf {
    let name = note
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    note.with_file_name(format!("{name}{suffix}"))
}

/// The note at `given`, which must be a `.md` file under the notes directory.
fn resolve_note(root: &Path, given: &str) -> Result<PathBuf, String> {
    let root = canonical_best_effort(root);
    let given = PathBuf::from(given);
    let given = if given.is_absolute() {
        given
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(given)
    };
    let note = canonical_best_effort(&given);
    if !note.starts_with(&root) || note.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return Err(format!(
            "{} is not a handoff note under {}",
            given.display(),
            root.display()
        ));
    }
    Ok(note)
}

/// `path` with its longest existing ancestor canonicalized, so a note is
/// found through a `..` or a symlinked state directory before it exists.
fn canonical_best_effort(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => canonical_best_effort(parent).join(name),
        _ => path.to_path_buf(),
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    canonical_best_effort(left) == canonical_best_effort(right)
}

/// The header lines a note's first ten lines hold.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Headers {
    status: Option<String>,
    owner: Option<String>,
    repositories: Vec<String>,
}

fn parse_headers(text: &str) -> Headers {
    let mut headers = Headers::default();
    for line in text.lines().take(HEADER_LINES) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "Status" => headers.status = Some(value.to_string()),
            "Owner" => headers.owner = Some(value.to_string()),
            "Repositories" => {
                headers.repositories = value
                    .split(',')
                    .map(str::trim)
                    .filter(|repository| !repository.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    headers
}

fn read_headers(note: &Path) -> Headers {
    std::fs::read_to_string(note)
        .map(|text| parse_headers(&text))
        .unwrap_or_default()
}

fn read_held(note: &Path) -> Held {
    let path = claim_path(note);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Held::Claim)
            .unwrap_or(Held::Unreadable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Held::None,
        Err(_) => Held::Unreadable,
    }
}

/// The notes under the sessions directory: by agent, then by file name, so a
/// listing and a `take` see them in one order.
fn notes(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    let mut agents: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    agents.sort();
    for agent in agents {
        let Ok(entries) = std::fs::read_dir(&agent) else {
            continue;
        };
        let mut notes: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
            .map(|path| canonical_best_effort(&path))
            .collect();
        notes.sort();
        found.extend(notes);
    }
    found
}

/// `herdr handoff path`.
fn command_path(root: &Path) -> Result<Outcome, String> {
    let session = session()?;
    let note = session_note(root, &session);
    let directory = note
        .parent()
        .ok_or_else(|| format!("{} has no directory", note.display()))?;
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    Ok(Outcome::new(0, note.display().to_string()))
}

fn run_claim(root: &Path, path: &str, breaking: bool, view: &View<'_>) -> Result<Outcome, String> {
    let (_, me) = session_and_claim()?;
    Ok(claim(&resolve_note(root, path)?, &me, breaking, view))
}

fn run_done(root: &Path, path: &str, view: &View<'_>) -> Result<Outcome, String> {
    let (_, me) = session_and_claim()?;
    Ok(done(&resolve_note(root, path)?, &me, view))
}

fn run_forget(root: &Path, path: &str, view: &View<'_>) -> Result<Outcome, String> {
    Ok(forget(&resolve_note(root, path)?, view))
}

fn run_take(root: &Path, repository: Option<&str>, view: &View<'_>) -> Result<Outcome, String> {
    let (_, me) = session_and_claim()?;
    take(
        root,
        Path::new(repository.unwrap_or(".")),
        &crate::workers::workers_dir(),
        &me,
        view,
    )
}

/// `herdr handoff claim [--break] PATH`.
fn claim(note: &Path, me: &Claim, breaking: bool, view: &View<'_>) -> Outcome {
    guarded(note, &guard_of(me), view, || {
        if !note.is_file() {
            return Outcome::new(1, format!("no note at {}", note.display()));
        }
        match create_claim(&claim_path(note), me) {
            Ok(()) => Outcome::new(0, format!("claimed: {}", note.display())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let held = read_held(note);
                if matches!(&held, Held::Claim(held) if same_claimer(held, me)) {
                    return Outcome::new(
                        0,
                        format!("already claimed by this agent: {}", note.display()),
                    );
                }
                if breaking && breakable(&held, me, view) && replace_claim(note, &held, me, view) {
                    return Outcome::new(0, format!("claimed: {}", note.display()));
                }
                Outcome::new(1, format!("claimed by {}", describe(&held, view)))
            }
            Err(error) => Outcome::new(1, format!("cannot claim {}: {error}", note.display())),
        }
    })
}

/// `herdr handoff done PATH`: the claimant finished the note.
fn done(note: &Path, me: &Claim, view: &View<'_>) -> Outcome {
    guarded(note, &guard_of(me), view, || {
        let held = read_held(note);
        if !matches!(&held, Held::Claim(held) if same_claimer(held, me)) {
            return Outcome::new(
                1,
                format!(
                    "not done: {} is not claimed by this session",
                    note.display()
                ),
            );
        }
        // The claim goes last: it guards the note, so a command that stops
        // between the two leaves a claim, never a note someone else could
        // claim, and `forget` clears what is left.
        match remove_note_and_claim(note) {
            Ok(()) => Outcome::new(0, format!("done: {}", note.display())),
            Err(error) => Outcome::new(1, format!("not done: {error}")),
        }
    })
}

/// `herdr handoff forget PATH`: a person's call removes a note nobody takes.
fn forget(note: &Path, view: &View<'_>) -> Outcome {
    guarded(
        note,
        &this_process(view),
        view,
        || match remove_note_and_claim(note) {
            Ok(()) => Outcome::new(0, format!("forgot: {}", note.display())),
            Err(error) => Outcome::new(1, format!("not forgotten: {error}")),
        },
    )
}

/// Removes the note first and its claim last. The claim guards the note, so a
/// note that cannot be removed keeps its claim: stopping here leaves the note
/// unclaimable by another session, where removing the claim too would leave it
/// open to the next one.
fn remove_note_and_claim(note: &Path) -> Result<(), String> {
    remove(note)?;
    remove(&claim_path(note))
}

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

/// A note as `take` may claim it.
struct Candidate {
    modified: std::time::SystemTime,
    note: PathBuf,
    headers: Headers,
}

/// `herdr handoff take [--repo DIR]`: the pending coordinator handoffs are
/// read from herdr's persisted state in `workers`, never from a note's name or
/// a path convention.
fn take(
    root: &Path,
    repository: &Path,
    workers: &Path,
    me: &Claim,
    view: &View<'_>,
) -> Result<Outcome, String> {
    let waiting = crate::workers::coordinator_handoff::pending_notes_in(workers);
    let key = git_common_dir(repository)?;
    let mut candidates: Vec<Candidate> = Vec::new();
    for note in notes(root) {
        let headers = read_headers(&note);
        if headers.status.as_deref() != Some(PENDING) {
            continue;
        }
        if !matches!(read_held(&note), Held::None) {
            continue;
        }
        // A note a coordinator handoff waits for is taken by taking that
        // handoff first, never by an unrelated session.
        if waiting.iter().any(|waiting| waits_for(waiting, &note)) {
            continue;
        }
        if !same_repo(&headers.repositories, &key) {
            continue;
        }
        let Ok(modified) = std::fs::metadata(&note).and_then(|metadata| metadata.modified()) else {
            continue;
        };
        candidates.push(Candidate {
            modified,
            note,
            headers,
        });
    }
    candidates.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.note.cmp(&right.note))
    });
    match candidates.len() {
        0 => Ok(Outcome::silent(3)),
        1 => {
            let candidate = &candidates[0];
            let outcome = claim(&candidate.note, me, false, view);
            if outcome.code != 0 {
                return Ok(outcome);
            }
            let claimed = outcome.text.unwrap_or_default();
            Ok(Outcome::new(
                0,
                format!(
                    "{claimed}\nrepositories {}",
                    repositories(&candidate.headers)
                ),
            ))
        }
        _ => {
            let lines: Vec<String> = candidates
                .iter()
                .map(|candidate| {
                    format!(
                        "{}\n  owner {}; repositories {}",
                        candidate.note.display(),
                        owner(&candidate.headers),
                        repositories(&candidate.headers)
                    )
                })
                .collect();
            Ok(Outcome::new(4, lines.join("\n")))
        }
    }
}

/// `herdr handoff show [--repo DIR]`.
fn show(root: &Path, repository: Option<&Path>, view: &View<'_>) -> Result<Outcome, String> {
    let key = match repository {
        Some(repository) => Some(git_common_dir(repository)?),
        None => None,
    };
    let mut lines = Vec::new();
    for note in notes(root) {
        let headers = read_headers(&note);
        if let Some(key) = key.as_deref() {
            if !same_repo(&headers.repositories, key) {
                continue;
            }
        }
        let held = read_held(&note);
        let claim = match &held {
            Held::None => "unclaimed".to_string(),
            held => format!("claimed by {}", describe(held, view)),
        };
        lines.push(format!(
            "{}\n  {}; {claim}; owner {}; repositories {}",
            note.display(),
            headers.status.as_deref().unwrap_or("?"),
            owner(&headers),
            repositories(&headers)
        ));
    }
    if lines.is_empty() {
        Ok(Outcome::silent(3))
    } else {
        Ok(Outcome::new(0, lines.join("\n")))
    }
}

fn owner(headers: &Headers) -> &str {
    headers.owner.as_deref().unwrap_or("?")
}

fn repositories(headers: &Headers) -> String {
    if headers.repositories.is_empty() {
        "?".to_string()
    } else {
        headers.repositories.join(", ")
    }
}

/// Whether a pending coordinator handoff waits for `note`. Herdr records a
/// copy of the note in its own handoff directory, not the note's path, so the
/// note is matched by that copy's path and, as the recorded provenance, by the
/// sha256 of the content the handoff froze.
fn waits_for(waiting: &crate::workers::coordinator_handoff::PendingNote, note: &Path) -> bool {
    if same_path(&waiting.copy, note) {
        return true;
    }
    crate::checksum::file_sha256(note)
        .is_ok_and(|sha256| sha256.eq_ignore_ascii_case(&waiting.sha256))
}

/// Whether a note's repositories name the repository of `key`, the git common
/// directory every worktree of a clone shares.
fn same_repo(repositories: &[String], key: &Path) -> bool {
    repositories.iter().any(|repository| {
        crate::workspace::git_worktree_info(Path::new(repository))
            .is_some_and(|info| same_path(&info.git_common_dir, key))
    })
}

/// The repository's git common directory, which every worktree of the clone
/// shares.
fn git_common_dir(repository: &Path) -> Result<PathBuf, String> {
    let directory = std::path::absolute(repository).map_err(|error| error.to_string())?;
    repository_common_dir(&directory, |directory| {
        if let Some(info) = crate::workspace::git_worktree_info(directory) {
            return Ok(Some(info.git_common_dir));
        }
        // A repository whose git directory lives outside its worktree (a bare
        // repository that a git wrapper pairs with it) is found only by asking
        // git, as roagent does.
        common_dir_from_git(directory)
    })
}

/// The common directory `probe` finds, else the refusal. Split out so the
/// refusal is testable where the tests cannot make a directory outside every
/// repository (their temporary directory can fall inside one).
fn repository_common_dir(
    directory: &Path,
    probe: impl FnOnce(&Path) -> Result<Option<PathBuf>, String>,
) -> Result<PathBuf, String> {
    match probe(directory)? {
        Some(common) => Ok(common),
        None => Err(format!("{} is not a git repository", directory.display())),
    }
}

/// The common directory git itself names for `directory`, when it is a
/// repository.
fn common_dir_from_git(directory: &Path) -> Result<Option<PathBuf>, String> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(directory)
        .args([
            "--no-optional-locks",
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])
        .output()
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let common = String::from_utf8(output.stdout).unwrap_or_default();
    let common = common.trim();
    if !output.status.success() || common.is_empty() {
        return Ok(None);
    }
    Ok(Some(canonical_best_effort(Path::new(common))))
}

/// Runs one note mutation under the note's claim guard, an operating system
/// advisory lock on `<note>.claim.guard`. All mutations share it, so a release
/// and a second claim cannot slip between a breaker's check and its rename.
///
/// The kernel drops the lock with the process that holds it, so a mutation that
/// crashes leaves no guard to recover and a guard file left in the directory is
/// harmless. The file is never removed: an unlinked locked file would let a
/// second process lock a fresh inode and run the mutation at the same time. A
/// contested mutation is refused, naming the guard, and is never retried.
fn guarded(
    note: &Path,
    holder: &Guard,
    view: &View<'_>,
    action: impl FnOnce() -> Outcome,
) -> Outcome {
    let guard = guard_path(note);
    let held = match lock_guard(&guard) {
        Ok(held) => held,
        Err(GuardRefusal::Busy) => return busy(note, &guard, view),
        Err(GuardRefusal::Error(message)) => {
            return Outcome::new(1, format!("cannot hold {}: {message}", guard.display()))
        }
    };
    held.record(holder);
    action()
}

/// Why a mutation could not take the guard.
#[derive(Debug)]
enum GuardRefusal {
    /// Another mutation holds it.
    Busy,
    /// The guard file could not be opened or locked at all.
    Error(String),
}

/// A held guard: the advisory lock on its file, released when the file closes.
struct HeldGuard {
    file: std::fs::File,
}

/// Takes the guard's advisory lock, creating the guard file when it is new.
fn lock_guard(guard: &Path) -> Result<HeldGuard, GuardRefusal> {
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(guard)
        .map_err(|error| GuardRefusal::Error(error.to_string()))?;
    match file.try_lock() {
        Ok(()) => Ok(HeldGuard { file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(GuardRefusal::Busy),
        Err(std::fs::TryLockError::Error(error)) => Err(GuardRefusal::Error(error.to_string())),
    }
}

impl HeldGuard {
    /// Records this holder in the guard file: a diagnostic the refusal names,
    /// never a fact a decision rests on. The lock alone decides, so a reader
    /// that sees a half-written or stale record still learns only that a
    /// mutation is in progress, and a guard left by a crashed holder is taken
    /// by its next mutation without consulting the record.
    fn record(&self, holder: &Guard) {
        let Ok(text) = serde_json::to_string(holder) else {
            return;
        };
        let mut file = &self.file;
        let _ = file.set_len(0);
        let _ = file.seek(std::io::SeekFrom::Start(0));
        let _ = file.write_all(format!("{text}\n").as_bytes());
        let _ = file.flush();
    }
}

/// The guard record of the process a claim belongs to.
fn guard_of(claim: &Claim) -> Guard {
    Guard {
        pid: claim.pid,
        start: claim.start,
        host: claim.host.clone(),
    }
}

/// The guard record of this command's own process, for a mutation that needs
/// no session (`forget`).
fn this_process(view: &View<'_>) -> Guard {
    let pid = std::process::id();
    Guard {
        pid,
        start: crate::platform::process_start_token(pid),
        host: view.host.clone(),
    }
}

fn busy(note: &Path, guard: &Path, view: &View<'_>) -> Outcome {
    Outcome::new(
        1,
        format!(
            "claimed by {}; mutation in progress at {}",
            describe(&read_held(note), view),
            guard.display()
        ),
    )
}

/// Writes the claim exclusively: whoever created the file holds the note, so
/// two sessions never hold one.
fn create_claim(path: &Path, claim: &Claim) -> std::io::Result<()> {
    let text = serde_json::to_string(claim).map_err(std::io::Error::other)?;
    write_new(path, &format!("{text}\n"))
}

/// Writes `text` to `path` exclusively and whole: it goes to a temporary file
/// beside `path`, is synced, and is hard-linked into place. The link is atomic
/// and fails with `AlreadyExists` when someone else got there first, so a
/// reader never sees a half-written claim or guard, and a crash leaves either
/// no record or a complete one.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let temporary = temporary_beside(path, "new");
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::hard_link(&temporary, path)
    })();
    let _ = std::fs::remove_file(&temporary);
    written
}

/// A name beside `path` that no record uses: this process and this call tell
/// it apart, so a temporary file is never mistaken for a claim, a guard or a
/// note.
fn temporary_beside(path: &Path, purpose: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let call = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{purpose}-{}-{call}", std::process::id()))
}

/// Replaces the eligible claim while the guard is held. The temporary file and
/// the rename keep the claim occupied throughout the takeover, so a release
/// and a second claim cannot slip in between the check and the rename.
fn replace_claim(note: &Path, seen: &Held, me: &Claim, view: &View<'_>) -> bool {
    let held = read_held(note);
    if &held != seen || !breakable(&held, me, view) {
        return false;
    }
    let claim = claim_path(note);
    let temporary = claim.with_file_name(format!(
        ".claim-{}-{}",
        std::process::id(),
        me.nonce.as_deref().unwrap_or("takeover")
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        let mut text = serde_json::to_string(me).map_err(std::io::Error::other)?;
        text.push('\n');
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &claim)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written.is_ok()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;

    /// A directory of its own under the temp dir, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let name = format!(
                "herdr-handoff-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
            // The temp directory this machine names, else one beside the build
            // output: a sandbox may refuse the first.
            let mut candidates = vec![std::env::temp_dir().join(&name)];
            if let Ok(directory) = std::env::current_dir() {
                candidates.push(directory.join("target/tmp").join(&name));
            }
            for candidate in candidates {
                if std::fs::create_dir_all(&candidate).is_ok() {
                    return Self(candidate);
                }
            }
            panic!("no writable directory for a test note");
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The processes a test says run: each pid and its start token.
    struct Fake {
        processes: Vec<(u32, u64)>,
    }

    impl Fake {
        fn running(processes: &[(u32, u64)]) -> Self {
            Self {
                processes: processes.to_vec(),
            }
        }
    }

    impl Processes for Fake {
        fn exists(&self, pid: u32) -> bool {
            self.processes.iter().any(|(known, _)| *known == pid)
        }

        fn start(&self, pid: u32) -> Option<u64> {
            self.processes
                .iter()
                .find(|(known, _)| *known == pid)
                .map(|(_, start)| *start)
        }
    }

    fn view_of(fake: &Fake) -> View<'_> {
        View {
            host: Some("test-host".into()),
            processes: fake,
        }
    }

    fn claimer(pid: u32, start: u64, session: &str) -> Claim {
        Claim {
            pid,
            start: Some(start),
            agent: "pi".into(),
            pane: Some("pane-1".into()),
            session: session.into(),
            host: Some("test-host".into()),
            nonce: Some(format!("nonce-{pid}")),
        }
    }

    fn write_note(dir: &Path, session: &str, status: &str, repositories: &[&str]) -> PathBuf {
        let note = dir.join("pi").join(format!("{session}.md"));
        std::fs::create_dir_all(note.parent().expect("agent dir")).expect("agent dir");
        let text = format!(
            "Status: {status}\nOwner: pi {session}\nRepositories: {}\n\n# note\n",
            repositories.join(", ")
        );
        std::fs::write(&note, text).expect("note");
        note
    }

    /// Sets a note's modification time, so the listing order is the test's
    /// and never the filesystem's clock.
    fn set_modified(path: &Path, seconds: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("note file");
        let time = std::time::UNIX_EPOCH + Duration::from_secs(seconds);
        file.set_times(std::fs::FileTimes::new().set_modified(time))
            .expect("modification time");
    }

    /// A repository with a `.git` directory holding a `HEAD`.
    fn repo_dir(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        let git = root.join(".git");
        std::fs::create_dir_all(&git).expect("git dir");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        std::fs::create_dir_all(git.join("objects")).expect("objects");
        std::fs::create_dir_all(git.join("refs")).expect("refs");
        root
    }

    /// A linked worktree of `repository_root`, whose `.git` is a file naming
    /// the worktree's git directory and that directory's common dir.
    fn worktree(base: &Path, repository_root: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        std::fs::create_dir_all(&root).expect("worktree");
        let git_dir = repository_root.join(".git/worktrees").join(name);
        std::fs::create_dir_all(&git_dir).expect("worktree git dir");
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/work\n").expect("HEAD");
        std::fs::write(git_dir.join("commondir"), "../..\n").expect("commondir");
        std::fs::write(
            root.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .expect("git file");
        root
    }

    #[test]
    fn one_claim_wins_among_concurrent_claimers() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        // Every claimer's process runs: a guard whose holder is alive is never
        // recovered, so the guard, not only the exclusive claim file, decides.
        let live: Vec<(u32, u64)> = (0..8).map(|index| (1000 + index, 7)).collect();
        let fake = Arc::new(Fake::running(&live));
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for index in 0..8 {
            let note = note.clone();
            let barrier = Arc::clone(&barrier);
            let fake = Arc::clone(&fake);
            let me = claimer(1000 + index, 7, &format!("pi s{index}"));
            handles.push(std::thread::spawn(move || {
                let view = view_of(&fake);
                barrier.wait();
                claim(&note, &me, false, &view)
            }));
        }
        let outcomes: Vec<Outcome> = handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread"))
            .collect();
        let holders: Vec<&Outcome> = outcomes
            .iter()
            .filter(|outcome| outcome.code == 0)
            .collect();
        assert_eq!(holders.len(), 1, "exactly one claim wins: {outcomes:?}");
        assert!(holders[0]
            .text
            .as_deref()
            .is_some_and(|text| text.starts_with("claimed: ")));
        let winner = match read_held(&note) {
            Held::Claim(winner) => winner,
            other => panic!("the winner holds the note, not {other:?}"),
        };
        // The others are refused, by the note's holder or by the mutation
        // guard that holder works under; nothing is retried.
        for outcome in outcomes.iter().filter(|outcome| outcome.code != 0) {
            assert_eq!(outcome.code, 1, "{outcome:?}");
            let text = outcome.text.as_deref().expect("a refusal says why");
            assert!(
                text.starts_with("claimed by ") || text.contains("mutation in progress at"),
                "{text}"
            );
        }
        assert!(
            lock_guard(&guard_path(&note)).is_ok(),
            "the winner released the guard's lock, and the guard file stays"
        );

        // Once the race is over the note names its holder to the next session.
        let late = claimer(9999, 1, "pi late");
        let refusal = claim(&note, &late, false, &view_of(&Fake::running(&live)));
        assert_eq!(refusal.code, 1);
        assert_eq!(
            refusal.text.as_deref(),
            Some(
                format!(
                    "claimed by {}, pid {}, herdr pane pane-1",
                    winner.session, winner.pid
                )
                .as_str()
            )
        );
    }

    /// One mutation at a time: while a mutation holds the guard, another is
    /// refused and names the guard. A guard that did not lock would let the
    /// second mutation run and claim the note, so this fails without the lock.
    #[test]
    fn a_mutation_in_progress_is_refused() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7), (2000, 9)]);
        let view = view_of(&fake);
        let holder = Guard {
            pid: 2000,
            start: Some(9),
            host: Some("test-host".into()),
        };
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let note_ref = &note;
        let holder_ref = &holder;
        let view_ref = &view;
        std::thread::scope(|scope| {
            let running = scope.spawn(move || {
                guarded(note_ref, holder_ref, view_ref, || {
                    entered_tx.send(()).expect("the mutation is entered");
                    release_rx.recv().expect("the observer finished");
                    Outcome::silent(0)
                })
            });
            entered_rx
                .recv()
                .expect("the first mutation holds the guard");
            let me = claimer(1000, 7, "pi s1");
            let outcome = claim(&note, &me, false, &view);
            assert_eq!(outcome.code, 1, "{outcome:?}");
            assert_eq!(
                outcome.text.as_deref(),
                Some(
                    format!(
                        "claimed by an unreadable claim; mutation in progress at {}",
                        guard_path(&note).display()
                    )
                    .as_str()
                )
            );
            assert!(
                !claim_path(&note).exists(),
                "nothing is claimed while another mutation holds the guard"
            );
            release_tx.send(()).expect("the observer releases it");
            assert_eq!(running.join().expect("guard holder").code, 0);
        });

        // Afterwards the guard is free again, and its file stays.
        assert!(
            guard_path(&note).exists(),
            "the guard file is never removed"
        );
        assert_eq!(
            claim(&note, &claimer(1000, 7, "pi s1"), false, &view).code,
            0
        );
    }

    /// A guard file left by a process that is gone (a crashed holder) does not
    /// wedge the note: its lock went with the process, so the next mutation
    /// takes it and never consults the record the file still names.
    #[test]
    fn a_guard_left_by_a_crashed_process_does_not_wedge_the_note() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let guard = guard_path(&note);
        let crashed = Guard {
            pid: 4242,
            start: Some(1),
            host: Some("test-host".into()),
        };
        std::fs::write(&guard, serde_json::to_string(&crashed).unwrap() + "\n").unwrap();

        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");
        let outcome = claim(&note, &me, false, &view);
        assert_eq!(outcome.code, 0, "{outcome:?}");
        assert_eq!(read_held(&note), Held::Claim(me));

        assert!(guard.exists(), "the guard file is never removed");
        let recorded: Guard =
            serde_json::from_str(&std::fs::read_to_string(&guard).unwrap()).expect("the record");
        assert_eq!(recorded.pid, 1000, "the holder records itself");
    }

    #[test]
    fn a_second_claim_names_the_holder() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7), (2000, 9)]);
        let view = view_of(&fake);
        let first = claimer(1000, 7, "pi s1");
        assert_eq!(claim(&note, &first, false, &view).code, 0);

        let second = claimer(2000, 9, "pi s2");
        let outcome = claim(&note, &second, false, &view);
        assert_eq!(outcome.code, 1);
        assert_eq!(
            outcome.text.as_deref(),
            Some("claimed by pi s1, pid 1000, herdr pane pane-1")
        );
        assert_eq!(read_held(&note), Held::Claim(first.clone()));
    }

    #[test]
    fn the_same_session_claims_again_without_taking_the_note_twice() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");
        assert_eq!(claim(&note, &me, false, &view).code, 0);
        let outcome = claim(&note, &me, false, &view);
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text.as_deref(),
            Some(format!("already claimed by this agent: {}", note.display()).as_str())
        );
    }

    #[test]
    fn break_refuses_a_live_claimant() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7), (2000, 9)]);
        let view = view_of(&fake);
        let holder = claimer(1000, 7, "pi s1");
        assert_eq!(claim(&note, &holder, false, &view).code, 0);

        let other = claimer(2000, 9, "pi s2");
        let outcome = claim(&note, &other, true, &view);
        assert_eq!(outcome.code, 1);
        assert_eq!(
            outcome.text.as_deref(),
            Some("claimed by pi s1, pid 1000, herdr pane pane-1")
        );
        assert_eq!(read_held(&note), Held::Claim(holder.clone()));
    }

    #[test]
    fn break_replaces_a_gone_claimant() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let gone = claimer(1000, 7, "pi s1");
        assert_eq!(
            claim(&note, &gone, false, &view_of(&Fake::running(&[(1000, 7)]))).code,
            0
        );

        // The pid runs again as another process: another start token.
        let fake = Fake::running(&[(1000, 99), (2000, 9)]);
        let view = view_of(&fake);
        let holder = read_held(&note);
        assert_eq!(
            describe(&holder, &view),
            "pi s1, pid 1000, herdr pane pane-1, gone"
        );

        let other = claimer(2000, 9, "pi s2");
        let outcome = claim(&note, &other, true, &view);
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text.as_deref(),
            Some(format!("claimed: {}", note.display()).as_str())
        );
        assert_eq!(read_held(&note), Held::Claim(other.clone()));
    }

    #[test]
    fn break_replaces_an_earlier_session_of_this_process() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let earlier = claimer(1000, 7, "pi earlier");
        assert_eq!(claim(&note, &earlier, false, &view).code, 0);

        // A new session in the same process is someone else.
        let mine = claimer(1000, 7, "pi mine");
        assert_eq!(claim(&note, &mine, false, &view).code, 1);
        let outcome = claim(&note, &mine, true, &view);
        assert_eq!(outcome.code, 0);
        assert_eq!(read_held(&note), Held::Claim(mine.clone()));
    }

    #[test]
    fn break_refuses_a_claim_of_another_host() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let mut foreign = claimer(1000, 7, "pi s1");
        foreign.host = Some("another-host".into());
        std::fs::write(claim_path(&note), serde_json::to_string(&foreign).unwrap()).unwrap();

        let fake = Fake::running(&[(2000, 9)]);
        let view = view_of(&fake);
        assert_eq!(
            describe(&read_held(&note), &view),
            "pi s1, pid 1000, herdr pane pane-1"
        );
        let other = claimer(2000, 9, "pi s2");
        let outcome = claim(&note, &other, true, &view);
        assert_eq!(outcome.code, 1);
        assert_eq!(read_held(&note), Held::Claim(foreign.clone()));
    }

    #[test]
    fn done_is_the_claimants_and_removes_the_claim() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7), (2000, 9)]);
        let view = view_of(&fake);
        let holder = claimer(1000, 7, "pi s1");
        assert_eq!(claim(&note, &holder, false, &view).code, 0);

        let stranger = claimer(2000, 9, "pi s2");
        let refusal = done(&note, &stranger, &view);
        assert_eq!(refusal.code, 1);
        assert_eq!(
            refusal.text.as_deref(),
            Some(
                format!(
                    "not done: {} is not claimed by this session",
                    note.display()
                )
                .as_str()
            )
        );
        assert!(note.is_file());
        assert!(claim_path(&note).is_file());

        let finished = done(&note, &holder, &view);
        assert_eq!(finished.code, 0);
        assert_eq!(
            finished.text.as_deref(),
            Some(format!("done: {}", note.display()).as_str())
        );
        assert!(!note.exists());
        assert!(!claim_path(&note).exists());
        assert!(
            guard_path(&note).exists(),
            "the guard file is never removed"
        );
    }

    #[test]
    fn a_note_that_cannot_be_removed_keeps_its_claim() {
        let dir = TempDir::new();
        // A note path that is a directory cannot be removed as a file: the
        // claim must stay, because it guards the note that is still there.
        let note = dir.path().join("pi").join("s1.md");
        std::fs::create_dir_all(note.join("inside")).unwrap();
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");
        std::fs::write(claim_path(&note), serde_json::to_string(&me).unwrap()).unwrap();

        let outcome = done(&note, &me, &view);
        assert_eq!(outcome.code, 1);
        let text = outcome.text.unwrap_or_default();
        assert!(text.starts_with("not done: cannot remove "), "{text}");
        assert!(
            claim_path(&note).is_file(),
            "the claim still guards the note"
        );
        assert!(note.is_dir());
        assert!(
            guard_path(&note).exists(),
            "the guard file is never removed"
        );

        let outcome = forget(&note, &view);
        assert_eq!(outcome.code, 1);
        assert!(claim_path(&note).is_file());
    }

    #[test]
    fn a_note_left_without_its_note_is_cleared_by_forget() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let holder = claimer(1000, 7, "pi s1");
        assert_eq!(claim(&note, &holder, false, &view).code, 0);

        // A `done` that stopped after removing the note: the claim is left,
        // and it guards a note that is no longer there.
        std::fs::remove_file(&note).unwrap();
        assert!(claim_path(&note).is_file());
        assert_eq!(claim(&note, &holder, false, &view).code, 1);
        assert_eq!(
            claim(&note, &holder, false, &view).text.as_deref(),
            Some(format!("no note at {}", note.display()).as_str())
        );

        let outcome = forget(&note, &view);
        assert_eq!(outcome.code, 0);
        assert_eq!(
            outcome.text.as_deref(),
            Some(format!("forgot: {}", note.display()).as_str())
        );
        assert!(!claim_path(&note).exists());
    }

    #[test]
    fn take_refuses_none_several_and_claims_one() {
        let dir = TempDir::new();
        let base = dir.path().join("repos");
        std::fs::create_dir_all(&base).unwrap();
        let repo = repo_dir(&base, "repo");
        let root = notes_root(dir.path());
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");
        let workers = dir.path().join("workers");

        assert_eq!(take(&root, &repo, &workers, &me, &view).unwrap().code, 3);

        let older = write_note(&root, "s1", PENDING, &[repo.to_str().unwrap()]);
        set_modified(&older, 1_000);
        let newer = write_note(&root, "s2", PENDING, &[repo.to_str().unwrap()]);
        set_modified(&newer, 2_000);
        let several = take(&root, &repo, &workers, &me, &view).unwrap();
        assert_eq!(several.code, 4);
        let listed = several.text.unwrap_or_default();
        let listed: Vec<&str> = listed
            .lines()
            .filter(|line| !line.starts_with("  "))
            .collect();
        assert_eq!(listed.len(), 2);
        // A listing prints the notes under their resolved paths, as the ported
        // command does (roagent prints `os.path.realpath` results), so the
        // expected paths are resolved the same way.
        let newest = canonical_best_effort(&newer);
        let oldest = canonical_best_effort(&older);
        assert_eq!(listed[0], newest.to_str().unwrap(), "newest first");
        assert_eq!(listed[1], oldest.to_str().unwrap());

        std::fs::remove_file(&older).unwrap();
        let one = take(&root, &repo, &workers, &me, &view).unwrap();
        assert_eq!(one.code, 0);
        let text = one.text.unwrap_or_default();
        assert!(text.starts_with("claimed: "), "{text}");
        assert!(
            text.ends_with(&format!("repositories {}", repo.display())),
            "{text}"
        );
        assert!(matches!(read_held(&newer), Held::Claim(_)));
    }

    #[test]
    fn take_skips_claimed_and_non_pending_notes() {
        let dir = TempDir::new();
        let base = dir.path().join("repos");
        std::fs::create_dir_all(&base).unwrap();
        let repo = repo_dir(&base, "repo");
        let root = notes_root(dir.path());
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");
        let repository = repo.to_str().unwrap().to_string();

        let claimed = write_note(&root, "s1", PENDING, &[&repository]);
        assert_eq!(claim(&claimed, &me, false, &view).code, 0);
        write_note(&root, "s2", "done", &[&repository]);
        assert_eq!(
            take(&root, &repo, &dir.path().join("workers"), &me, &view)
                .unwrap()
                .code,
            3
        );
    }

    /// A note a pending coordinator handoff waits for is not taken, read from
    /// herdr's persisted handoff state (`pending.json`): the state records a
    /// copy of the note, not its path, so the note is matched by the content
    /// the handoff froze. Nothing is handed to `take` but the state's own
    /// directory, so the whole production path is exercised.
    #[test]
    fn take_skips_a_note_a_coordinator_handoff_waits_for() {
        let dir = TempDir::new();
        let base = dir.path().join("repos");
        std::fs::create_dir_all(&base).unwrap();
        let repo = repo_dir(&base, "repo");
        let root = notes_root(dir.path());
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");
        let repository = repo.to_str().unwrap().to_string();
        let note = write_note(&root, "s1", PENDING, &[&repository]);

        let workers = dir.path().join("workers");
        let copies = workers.join("handoffs");
        std::fs::create_dir_all(&copies).unwrap();
        let copy = copies.join("t-1-e1.note.md");
        std::fs::copy(&note, &copy).unwrap();
        assert_ne!(
            canonical_best_effort(&copy),
            canonical_best_effort(&note),
            "the copy is not the note, so only its content can match it"
        );
        let sha256 = crate::checksum::file_sha256(&note).unwrap();
        let state = format!(
            r#"{{"version":1,"pending":[{{"coordinator_id":"t-1","epoch":1,"old_pane":"pane-1","note_path":{},"note_sha256":"{sha256}","phase":"frozen","kind":null,"successor_pane":null,"snapshot_sha256":null}}],"superseded":[]}}"#,
            serde_json::to_string(&copy).unwrap()
        );
        std::fs::write(copies.join("pending.json"), state).unwrap();
        assert_eq!(
            crate::workers::coordinator_handoff::pending_notes_in(&workers).len(),
            1,
            "the test state is the one production reads"
        );

        let outcome = take(&root, &repo, &workers, &me, &view).unwrap();
        assert_eq!(outcome.code, 3, "{outcome:?}");
        assert!(!claim_path(&note).exists());

        // A pending handoff naming the note's own path skips it too, whatever
        // its recorded content hash.
        let state = format!(
            r#"{{"version":1,"pending":[{{"coordinator_id":"t-1","epoch":1,"old_pane":"pane-1","note_path":{},"note_sha256":"{}","phase":"frozen","kind":null,"successor_pane":null,"snapshot_sha256":null}}],"superseded":[]}}"#,
            serde_json::to_string(&note).unwrap(),
            "0".repeat(64)
        );
        std::fs::write(copies.join("pending.json"), state).unwrap();
        assert_eq!(take(&root, &repo, &workers, &me, &view).unwrap().code, 3);

        // Once the handoff is gone the note is the single candidate again.
        std::fs::write(
            copies.join("pending.json"),
            r#"{"version":1,"pending":[],"superseded":[]}"#,
        )
        .unwrap();
        let outcome = take(&root, &repo, &workers, &me, &view).unwrap();
        assert_eq!(outcome.code, 0, "{outcome:?}");
        assert_eq!(read_held(&note), Held::Claim(me.clone()));
    }

    #[test]
    fn show_lists_each_note_with_its_claim() {
        let dir = TempDir::new();
        let base = dir.path().join("repos");
        std::fs::create_dir_all(&base).unwrap();
        let repo = repo_dir(&base, "repo");
        let root = notes_root(dir.path());
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let repository = repo.to_str().unwrap().to_string();

        let claimed = write_note(&root, "s1", PENDING, &[&repository]);
        let me = claimer(1000, 7, "pi s1");
        assert_eq!(claim(&claimed, &me, false, &view).code, 0);
        let unclaimed = write_note(&root, "s2", PENDING, &[&repository]);

        let outcome = show(&root, None, &view).unwrap();
        assert_eq!(outcome.code, 0);
        let text = outcome.text.unwrap();
        // A listing prints the notes under their resolved paths, as the ported
        // command does (roagent prints `os.path.realpath` results).
        let claimed_path = canonical_best_effort(&claimed);
        let unclaimed_path = canonical_best_effort(&unclaimed);
        assert!(text.contains(claimed_path.to_str().unwrap()));
        assert!(
            text.contains("pending; claimed by pi s1, pid 1000, herdr pane pane-1; owner pi s1")
        );
        assert!(text.contains(unclaimed_path.to_str().unwrap()));
        assert!(text.contains("pending; unclaimed; owner pi s2"));

        // A repository nothing names has no note.
        let other = repo_dir(&base, "other");
        assert_eq!(show(&root, Some(&other), &view).unwrap().code, 3);
    }

    #[test]
    fn show_has_no_notes_without_a_sessions_directory() {
        let dir = TempDir::new();
        let root = notes_root(dir.path());
        let fake = Fake::running(&[]);
        assert_eq!(show(&root, None, &view_of(&fake)).unwrap().code, 3);
    }

    #[test]
    fn notes_naming_any_worktree_match_the_repository() {
        let dir = TempDir::new();
        let base = dir.path().join("repos");
        std::fs::create_dir_all(&base).unwrap();
        let repo = repo_dir(&base, "repo");
        let linked = worktree(&base, &repo, "linked");
        let root = notes_root(dir.path());
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");

        let note = write_note(&root, "s1", PENDING, &[linked.to_str().unwrap()]);
        let outcome = take(&root, &repo, &dir.path().join("workers"), &me, &view).unwrap();
        assert_eq!(outcome.code, 0);
        assert_eq!(read_held(&note), Held::Claim(me.clone()));

        // And the other way round: a note of the repository, taken from its
        // worktree.
        let note = write_note(&root, "s2", PENDING, &[repo.to_str().unwrap()]);
        let outcome = take(&root, &linked, &dir.path().join("workers"), &me, &view).unwrap();
        assert_eq!(outcome.code, 0);
        assert!(matches!(read_held(&note), Held::Claim(_)));
    }

    #[test]
    fn a_directory_that_is_not_in_a_repository_is_refused() {
        // A temporary directory can fall inside a repository (this one may),
        // so the direct case runs only where the precondition holds and the
        // precondition is asserted either way: a directory no discovery finds
        // is refused, and a directory inside a checkout says so.
        let dir = TempDir::new();
        let directory = dir.path().join("plain");
        std::fs::create_dir_all(&directory).unwrap();
        if crate::workspace::git_worktree_info(&directory).is_some() {
            assert!(
                directory
                    .ancestors()
                    .any(|ancestor| ancestor.join(".git").exists()),
                "a repository was found above a directory that has none: {}",
                directory.display()
            );
        } else {
            let error = git_common_dir(&directory).unwrap_err();
            assert!(error.contains("is not a git repository"), "{error}");
        }

        // A directory no discovery finds is refused, whatever the test's own
        // location.
        let nowhere = Path::new("/nowhere/herdr-handoff-test");
        let error = repository_common_dir(nowhere, |_| Ok(None)).unwrap_err();
        assert!(error.contains("is not a git repository"), "{error}");
        let error = repository_common_dir(nowhere, |_| {
            Err(format!("{}: git failed", nowhere.display()))
        })
        .unwrap_err();
        assert!(error.contains("git failed"), "{error}");
        let common = PathBuf::from("/nowhere/herdr-handoff-test/.git");
        assert_eq!(
            repository_common_dir(nowhere, |_| Ok(Some(common.clone()))).unwrap(),
            common
        );
    }

    #[test]
    fn headers_are_read_from_the_first_ten_lines() {
        let text = "Status: pending\nOwner: pi abc\nRepositories: /one, /two\n\
                    Note: ignored\n\n\n\n\n\n\nStatus: done\nRepositories: /late\n";
        let headers = parse_headers(text);
        assert_eq!(headers.status.as_deref(), Some("pending"));
        assert_eq!(headers.owner.as_deref(), Some("pi abc"));
        assert_eq!(headers.repositories, vec!["/one", "/two"]);

        // A key with an empty value is no header, and other keys are ignored.
        let headers = parse_headers("Status:\nOwner: claude x:y\nRepository: /nope\n");
        assert_eq!(headers.status, None);
        assert_eq!(headers.owner.as_deref(), Some("claude x:y"));
        assert!(headers.repositories.is_empty());
        assert_eq!(repositories(&headers), "?");
        assert_eq!(owner(&headers), "claude x:y");
    }

    #[test]
    fn a_session_id_is_one_plain_file_name() {
        let valid =
            |id: &str| session_from_env(|name| (name == "PI_SESSION_ID").then(|| id.to_string()));
        assert_eq!(
            valid("01a14ecc-1ac6").unwrap(),
            Session {
                agent: "pi",
                id: "01a14ecc-1ac6".into()
            }
        );
        assert!(valid("a").is_ok());
        assert!(valid("a.b_c-1").is_ok());
        for invalid in ["", "../escape", "with space", "-leading", "a/b", "."] {
            assert!(valid(invalid).is_err(), "{invalid}");
        }
        let pi_first = session_from_env(|name| match name {
            "PI_SESSION_ID" => Some("pi-session".into()),
            _ => Some("claude-session".into()),
        })
        .unwrap();
        assert_eq!(pi_first.agent, "pi");
        let claude = session_from_env(|name| {
            (name == "CLAUDE_CODE_SESSION_ID").then(|| "claude-session".to_string())
        })
        .unwrap();
        assert_eq!(
            claude,
            Session {
                agent: "claude",
                id: "claude-session".into()
            }
        );
        assert!(session_from_env(|_| None).is_err());
        assert_eq!(claude.claimed_as(), "claude claude-session");
    }

    #[test]
    fn a_claim_names_the_agent_the_session_id_belongs_to() {
        let pi = Session {
            agent: "pi",
            id: "abc".into(),
        };
        assert_eq!(claiming_agent(42, "pi", &pi).unwrap(), "pi");
        // A pi session that inherited Claude's variable is refused, not
        // recorded as a pi claim under a Claude id.
        let error = claiming_agent(42, "claude", &pi).unwrap_err();
        assert!(error.contains("runs claude"), "{error}");
        assert!(error.contains("$PI_SESSION_ID"), "{error}");
        let claude = Session {
            agent: "claude",
            id: "abc".into(),
        };
        let error = claiming_agent(42, "pi", &claude).unwrap_err();
        assert!(error.contains("$CLAUDE_CODE_SESSION_ID"), "{error}");
    }

    #[test]
    fn the_pane_session_must_agree_with_the_environment() {
        let pi = Session {
            agent: "pi",
            id: "abc-123".into(),
        };
        let agrees = Reported {
            agent: Some("pi".into()),
            value: "abc-123".into(),
            is_path: false,
        };
        assert!(pi.matches(&agrees));
        let path = Reported {
            agent: Some("pi".into()),
            value: "/home/me/.pi/sessions/x/2026-abc-123.jsonl".into(),
            is_path: true,
        };
        assert!(pi.matches(&path));
        let other = Reported {
            agent: Some("pi".into()),
            value: "def-456".into(),
            is_path: false,
        };
        assert!(!pi.matches(&other));
        let another_agent = Reported {
            agent: Some("claude".into()),
            value: "abc-123".into(),
            is_path: false,
        };
        assert!(!pi.matches(&another_agent));
        let unknown_agent = Reported {
            agent: None,
            value: "abc-123".into(),
            is_path: false,
        };
        assert!(pi.matches(&unknown_agent));
    }

    #[test]
    fn a_note_path_outside_the_notes_directory_is_refused() {
        let dir = TempDir::new();
        let root = notes_root(dir.path());
        std::fs::create_dir_all(root.join("pi")).unwrap();
        let outside = dir.path().join("elsewhere.md");
        std::fs::write(&outside, "Status: pending\n").unwrap();

        let error = resolve_note(&root, outside.to_str().unwrap()).unwrap_err();
        assert!(error.contains("is not a handoff note under"), "{error}");
        let error = resolve_note(&root, "/etc/passwd").unwrap_err();
        assert!(error.contains("is not a handoff note under"), "{error}");

        let inside = root.join("pi/s1.md");
        std::fs::write(&inside, "Status: pending\n").unwrap();
        assert_eq!(
            resolve_note(&root, inside.to_str().unwrap()).unwrap(),
            canonical_best_effort(&inside)
        );
    }

    #[test]
    fn a_guard_in_progress_is_refused_and_its_file_stays() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        let fake = Fake::running(&[(1000, 7), (2000, 9)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");

        // Holding the guard's lock refuses another mutation at once.
        let guard = guard_path(&note);
        let held = lock_guard(&guard).expect("the guard is free");
        let outcome = claim(&note, &me, false, &view);
        assert_eq!(outcome.code, 1);
        let text = outcome.text.unwrap_or_default();
        assert!(text.contains("mutation in progress at"), "{text}");
        assert!(text.contains(guard.to_str().unwrap()), "{text}");
        assert!(!claim_path(&note).exists());

        // Dropping the lock frees it without removing the file.
        drop(held);
        assert!(guard.exists(), "the guard file is never removed");
        let outcome = claim(&note, &me, false, &view);
        assert_eq!(outcome.code, 0);
        assert_eq!(read_held(&note), Held::Claim(me));
        assert!(guard.exists());
    }

    #[test]
    fn an_unreadable_claim_is_held_until_a_person_forgets_it() {
        let dir = TempDir::new();
        let note = write_note(dir.path(), "s1", PENDING, &[]);
        std::fs::write(claim_path(&note), "not json\n").unwrap();
        let fake = Fake::running(&[(1000, 7)]);
        let view = view_of(&fake);
        let me = claimer(1000, 7, "pi s1");

        let outcome = claim(&note, &me, true, &view);
        assert_eq!(outcome.code, 1);
        assert_eq!(
            outcome.text.as_deref(),
            Some("claimed by an unreadable claim")
        );
        let refusal = done(&note, &me, &view);
        assert_eq!(refusal.code, 1);
        assert_eq!(forget(&note, &view).code, 0);
        assert!(!note.exists());
    }

    #[test]
    fn the_claim_and_guard_live_beside_the_note() {
        let note = Path::new("/state/handoffs/sessions/pi/abc.md");
        assert_eq!(
            claim_path(note),
            Path::new("/state/handoffs/sessions/pi/abc.md.claim")
        );
        assert_eq!(
            guard_path(note),
            Path::new("/state/handoffs/sessions/pi/abc.md.claim.guard")
        );
        assert_eq!(
            session_note(
                Path::new("/state/handoffs/sessions"),
                &Session {
                    agent: "claude",
                    id: "x1".into()
                }
            ),
            Path::new("/state/handoffs/sessions/claude/x1.md")
        );
    }

    #[test]
    fn claims_are_json_a_later_command_reads() {
        let me = claimer(1000, 7, "pi s1");
        let text = serde_json::to_string(&me).unwrap();
        assert_eq!(serde_json::from_str::<Claim>(&text).unwrap(), me);
        // A claim without a start token still reads.
        let older: Claim =
            serde_json::from_str(r#"{"pid":1,"agent":"pi","session":"pi s1","host":"h"}"#).unwrap();
        assert_eq!(older.start, None);
        assert_eq!(older.pane, None);
    }

    #[test]
    fn the_usage_names_the_three_handoffs() {
        assert!(USAGE.contains("herdr handoff path"));
        assert!(USAGE.contains("herdr agent handoff"));
        assert!(USAGE.contains("herdr coordinator handoff"));
        assert!(USAGE.contains("per host"));
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(parse(&["help".into()]).unwrap(), None);
        assert!(parse(&["nope".into()]).is_err());
        assert!(parse(&["done".into()]).is_err());
        assert!(parse(&["claim".into()]).is_err());
        assert!(parse(&["show".into(), "--repo".into()]).is_err());
        assert_eq!(
            parse(&["show".into(), "--repo".into(), "/x".into()]).unwrap(),
            Some(Command::Show {
                repository: Some("/x".into())
            })
        );
        assert_eq!(
            parse(&["take".into()]).unwrap(),
            Some(Command::Take { repository: None })
        );
        assert_eq!(
            parse(&["claim".into(), "--break".into(), "/n".into()]).unwrap(),
            Some(Command::Claim {
                path: "/n".into(),
                breaking: true
            })
        );
        assert_eq!(
            parse(&["claim".into(), "/n".into()]).unwrap(),
            Some(Command::Claim {
                path: "/n".into(),
                breaking: false
            })
        );
        assert_eq!(
            parse(&["done".into(), "/n".into()]).unwrap(),
            Some(Command::Done("/n".into()))
        );
        assert_eq!(
            parse(&["forget".into(), "/n".into()]).unwrap(),
            Some(Command::Forget("/n".into()))
        );
        assert_eq!(parse(&["path".into()]).unwrap(), Some(Command::Path));
    }
}
