//! `herdr usage --workspace`: tokens and an estimated cost per workspace,
//! summed from the transcripts of the agent sessions herdr knows about.
//!
//! Runs only on the user's call, in the CLI process: it reads the
//! transcripts the agents themselves write (Claude Code, Codex, pi) and
//! never asks a provider. Each session counts once by its id, wherever it
//! was seen (resumed in several panes or tabs, or taken over from a
//! worker), and each Claude API response counts once by its message and
//! request ids, so a transcript copied into a resumed session is not
//! counted twice either.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use super::prices::{self, ModelPrice};

/// Printed with every report.
pub(crate) const ESTIMATE_LABEL: &str = "estimate, not the provider's bill";

/// Printed with every report: which sessions it can count.
pub(crate) const SESSIONS_COUNTED: &str = "Only sessions herdr still knows are counted: \
     the current or last session of each open pane and the workers the server lists";

/// How an agent names its session: by id, or by its transcript's path (pi).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SessionKey {
    Id(String),
    Path(String),
}

impl SessionKey {
    fn value(&self) -> &str {
        match self {
            Self::Id(value) | Self::Path(value) => value,
        }
    }
}

/// One place herdr saw an agent session: a pane, or a headless worker.
#[derive(Debug, Clone)]
pub(crate) struct SessionSighting {
    /// The space it counts under; `None` when it has none herdr lists.
    pub workspace_id: Option<String>,
    pub agent: String,
    pub session: SessionKey,
    /// `pane <id>` or `worker <id>`.
    pub seen_in: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceRef {
    pub workspace_id: String,
    pub label: String,
}

/// The directories the agents write their transcripts to.
#[derive(Debug, Clone, Default)]
pub(crate) struct TranscriptRoots {
    /// Claude Code's `projects` directory.
    pub claude_projects: Vec<PathBuf>,
    /// Codex's `sessions` directory.
    pub codex_sessions: Vec<PathBuf>,
    /// pi's `agent/sessions` directory.
    pub pi_sessions: Vec<PathBuf>,
}

impl TranscriptRoots {
    /// `$CLAUDE_CONFIG_DIR` (else `~/.claude`), `$CODEX_HOME` (else
    /// `~/.codex`) and `~/.pi`.
    pub(crate) fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let dir_or_home = |variable: &str, fallback: &str| {
            std::env::var_os(variable)
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
        };
        Self {
            claude_projects: dir_or_home("CLAUDE_CONFIG_DIR", ".claude")
                .map(|dir| dir.join("projects"))
                .into_iter()
                .collect(),
            codex_sessions: dir_or_home("CODEX_HOME", ".codex")
                .map(|dir| dir.join("sessions"))
                .into_iter()
                .collect(),
            pi_sessions: home
                .as_ref()
                .map(|home| home.join(".pi").join("agent").join("sessions"))
                .into_iter()
                .collect(),
        }
    }
}

/// Tokens and cost of one model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ModelUsage {
    pub model: String,
    /// Input tokens neither written to nor read from the prompt cache.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    /// Absent when herdr has no price for the model and the agent reported
    /// none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// `price_table` (herdr's prices) or `agent_reported` (the cost the
    /// agent wrote into its transcript, for models herdr has no price for).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionUsage {
    pub agent: String,
    /// The session id, or the transcript path for agents named by path.
    pub session: String,
    /// Every pane and worker the session was seen in.
    pub seen_in: Vec<String>,
    /// Other spaces the session was also seen in; it counts only under the
    /// first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_in_workspaces: Vec<String>,
    pub transcripts: Vec<String>,
    pub models: Vec<ModelUsage>,
    pub cost_usd: f64,
    /// Why nothing was counted: no transcript found, or an agent whose
    /// transcripts herdr cannot read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkspaceUsage {
    /// Absent for sessions whose space herdr no longer lists.
    pub workspace_id: Option<String>,
    pub label: String,
    pub sessions: Vec<SessionUsage>,
    /// The sessions' tokens summed per model.
    pub models: Vec<ModelUsage>,
    /// The sum of the priced models.
    pub cost_usd: f64,
    /// Some tokens have no price, so the cost is too low.
    pub cost_incomplete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageEstimate {
    /// Always [`ESTIMATE_LABEL`].
    pub estimate: &'static str,
    pub prices_as_of: &'static str,
    pub price_source: &'static str,
    /// Always [`SESSIONS_COUNTED`].
    pub sessions_counted: &'static str,
    /// Only transcript entries at or after this time (printed in local
    /// time) count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub workspaces: Vec<WorkspaceUsage>,
}

/// Parses `--since`: an RFC 3339 time, or a `YYYY-MM-DD` date (from its
/// local midnight, `utc_offset_secs` east of UTC). Returns the start and
/// how the report prints it, in local time.
pub(crate) fn parse_since(
    value: &str,
    utc_offset_secs: i64,
) -> Result<(OffsetDateTime, String), String> {
    let invalid = || format!("--since takes YYYY-MM-DD or an RFC 3339 time, not `{value}`");
    let offset = i32::try_from(utc_offset_secs)
        .ok()
        .and_then(|secs| UtcOffset::from_whole_seconds(secs).ok())
        .unwrap_or(UtcOffset::UTC);
    let start = if let Ok(time) = OffsetDateTime::parse(value, &Rfc3339) {
        time
    } else {
        let mut parts = value.splitn(3, '-');
        let mut next = || parts.next().and_then(|part| part.parse::<u16>().ok());
        let (Some(year), Some(month), Some(day)) = (next(), next(), next()) else {
            return Err(invalid());
        };
        let month = u8::try_from(month)
            .ok()
            .and_then(|month| Month::try_from(month).ok())
            .ok_or_else(invalid)?;
        let day = u8::try_from(day).map_err(|_| invalid())?;
        Date::from_calendar_date(i32::from(year), month, day)
            .map(|date| PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_offset(offset))
            .map_err(|_| invalid())?
    };
    Ok((start, local_text(start, offset)))
}

/// `2026-10-01 00:00 +02:00`: `time` in the local offset.
fn local_text(time: OffsetDateTime, offset: UtcOffset) -> String {
    let local = time.to_offset(offset);
    let (hours, minutes, _) = offset.as_hms();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {}{:02}:{:02}",
        local.year(),
        u8::from(local.month()),
        local.day(),
        local.hour(),
        local.minute(),
        if offset.is_negative() { '-' } else { '+' },
        hours.unsigned_abs(),
        minutes.unsigned_abs(),
    )
}

/// The space a worker's usage counts under: its owner pane's space, else
/// the space it was started in.
pub(crate) fn worker_workspace(
    owner_pane_id: Option<&str>,
    workspace_id: Option<&str>,
    pane_workspaces: &HashMap<String, String>,
) -> Option<String> {
    owner_pane_id
        .and_then(|pane| pane_workspaces.get(pane).cloned())
        .or_else(|| workspace_id.map(str::to_string))
}

/// A session's first space, where it was seen, and its other spaces.
type Seen = (Option<String>, Vec<String>, Vec<String>);

/// Builds the report for `workspaces` (in their order) from the sessions
/// seen in them; sightings in a space not in `workspaces` are gathered
/// under a space without id. Each session counts under the first space it
/// was seen in. With `only`, only that space's sessions are read and
/// reported.
pub(crate) fn build_report(
    workspaces: &[WorkspaceRef],
    sightings: &[SessionSighting],
    roots: &TranscriptRoots,
    since: Option<(OffsetDateTime, String)>,
    only: Option<&str>,
) -> UsageEstimate {
    // One session per (agent, key), in the order first seen.
    let mut order: Vec<(String, SessionKey)> = Vec::new();
    let mut sessions: HashMap<(String, SessionKey), Seen> = HashMap::new();
    let listed: HashSet<&str> = workspaces.iter().map(|w| w.workspace_id.as_str()).collect();
    for sighting in sightings {
        let workspace = sighting
            .workspace_id
            .clone()
            .filter(|id| listed.contains(id.as_str()));
        let key = (sighting.agent.clone(), sighting.session.clone());
        match sessions.get_mut(&key) {
            Some((first, seen_in, also_in)) => {
                seen_in.push(sighting.seen_in.clone());
                if let Some(workspace) = workspace {
                    if first.as_ref() != Some(&workspace) && !also_in.contains(&workspace) {
                        also_in.push(workspace);
                    }
                }
            }
            None => {
                order.push(key.clone());
                sessions.insert(key, (workspace, vec![sighting.seen_in.clone()], Vec::new()));
            }
        }
    }

    let since_time = since.as_ref().map(|(time, _)| *time);
    let mut seen_messages = HashSet::new();
    let mut per_workspace: HashMap<Option<String>, Vec<SessionUsage>> = HashMap::new();
    for key in order {
        let Some((workspace, seen_in, also_in)) = sessions.remove(&key) else {
            continue;
        };
        if only.is_some() && workspace.as_deref() != only {
            continue;
        }
        let (agent, session) = key;
        let usage = read_session(
            &agent,
            &session,
            roots,
            since_time,
            &mut seen_messages,
            seen_in,
            also_in,
        );
        per_workspace.entry(workspace).or_default().push(usage);
    }

    let mut report = Vec::new();
    for workspace in workspaces {
        if only.is_some_and(|only| only != workspace.workspace_id) {
            continue;
        }
        let sessions = per_workspace
            .remove(&Some(workspace.workspace_id.clone()))
            .unwrap_or_default();
        report.push(sum_workspace(
            Some(workspace.workspace_id.clone()),
            workspace.label.clone(),
            sessions,
        ));
    }
    if let Some(sessions) = per_workspace.remove(&None) {
        report.push(sum_workspace(None, "(no space)".into(), sessions));
    }
    UsageEstimate {
        estimate: ESTIMATE_LABEL,
        prices_as_of: prices::PRICES_AS_OF,
        price_source: prices::PRICE_SOURCE,
        sessions_counted: SESSIONS_COUNTED,
        since: since.map(|(_, text)| text),
        workspaces: report,
    }
}

fn sum_workspace(
    workspace_id: Option<String>,
    label: String,
    sessions: Vec<SessionUsage>,
) -> WorkspaceUsage {
    let mut models = Models::default();
    for session in &sessions {
        for model in &session.models {
            models.add_usage(model);
        }
    }
    let models = models.finish();
    WorkspaceUsage {
        workspace_id,
        label,
        cost_usd: models.iter().filter_map(|m| m.cost_usd).sum(),
        cost_incomplete: models.iter().any(|m| m.cost_usd.is_none()),
        models,
        sessions,
    }
}

#[derive(Debug, Default)]
struct ModelTotals {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    cost: f64,
    priced: bool,
    reported: bool,
}

#[derive(Debug, Default)]
struct Models(BTreeMap<String, ModelTotals>);

impl Models {
    fn entry(&mut self, model: &str) -> &mut ModelTotals {
        self.0.entry(model.to_string()).or_default()
    }

    /// Adds one response's tokens, priced by herdr's table when it has the
    /// model, else by the cost the agent reported, if any.
    fn add(&mut self, model: &str, tokens: Tokens, reported_cost: Option<f64>) {
        let price = prices::price_for(model);
        let totals = self.entry(model);
        totals.input += tokens.input;
        totals.output += tokens.output;
        totals.cache_write += tokens.cache_write_5m + tokens.cache_write_1h;
        totals.cache_read += tokens.cache_read;
        if let Some(price) = price {
            totals.cost += tokens.cost(&price);
            totals.priced = true;
        } else if let Some(cost) = reported_cost {
            totals.cost += cost;
            totals.reported = true;
        }
    }

    fn add_usage(&mut self, usage: &ModelUsage) {
        let totals = self.entry(&usage.model);
        totals.input += usage.input_tokens;
        totals.output += usage.output_tokens;
        totals.cache_write += usage.cache_write_tokens;
        totals.cache_read += usage.cache_read_tokens;
        if let Some(cost) = usage.cost_usd {
            totals.cost += cost;
            match usage.cost_source {
                Some("agent_reported") => totals.reported = true,
                _ => totals.priced = true,
            }
        }
    }

    fn finish(self) -> Vec<ModelUsage> {
        self.0
            .into_iter()
            .map(|(model, totals)| ModelUsage {
                model,
                input_tokens: totals.input,
                output_tokens: totals.output,
                cache_write_tokens: totals.cache_write,
                cache_read_tokens: totals.cache_read,
                cost_usd: (totals.priced || totals.reported).then_some(totals.cost),
                cost_source: if totals.priced {
                    Some("price_table")
                } else if totals.reported {
                    Some("agent_reported")
                } else {
                    None
                },
            })
            .collect()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Tokens {
    input: u64,
    output: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
    cache_read: u64,
}

impl Tokens {
    fn cost(&self, price: &ModelPrice) -> f64 {
        (self.input as f64 * price.input
            + self.output as f64 * price.output
            + self.cache_write_5m as f64 * price.cache_write_5m
            + self.cache_write_1h as f64 * price.cache_write_1h
            + self.cache_read as f64 * price.cache_read)
            / 1_000_000.0
    }
}

#[derive(Clone, Copy)]
enum Reader {
    Claude,
    Codex,
    Pi,
}

fn reader_for(agent: &str) -> Option<Reader> {
    match agent {
        "claude" => Some(Reader::Claude),
        "codex" => Some(Reader::Codex),
        "pi" | "omp" => Some(Reader::Pi),
        _ => None,
    }
}

fn read_session(
    agent: &str,
    session: &SessionKey,
    roots: &TranscriptRoots,
    since: Option<OffsetDateTime>,
    seen_messages: &mut HashSet<String>,
    seen_in: Vec<String>,
    also_in: Vec<String>,
) -> SessionUsage {
    let mut usage = SessionUsage {
        agent: agent.to_string(),
        session: session.value().to_string(),
        seen_in,
        also_in_workspaces: also_in,
        transcripts: Vec::new(),
        models: Vec::new(),
        cost_usd: 0.0,
        note: None,
    };
    let Some(reader) = reader_for(agent) else {
        usage.note = Some(format!("herdr cannot read {agent} transcripts"));
        return usage;
    };
    let paths = locate(reader, session, roots);
    if paths.is_empty() {
        usage.note = Some("no transcript found".into());
        return usage;
    }
    let mut models = Models::default();
    for path in &paths {
        match reader {
            Reader::Claude => read_claude(path, since, seen_messages, &mut models),
            Reader::Codex => read_codex(path, since, &mut models),
            Reader::Pi => read_pi(path, since, &mut models),
        }
    }
    usage.transcripts = paths.iter().map(|p| p.display().to_string()).collect();
    usage.models = models.finish();
    usage.cost_usd = usage.models.iter().filter_map(|m| m.cost_usd).sum();
    usage
}

/// A session id that cannot leave the directory it is looked up in.
fn plain_file_name(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

fn locate(reader: Reader, session: &SessionKey, roots: &TranscriptRoots) -> Vec<PathBuf> {
    match (reader, session) {
        (_, SessionKey::Path(path)) => {
            let path = PathBuf::from(path);
            if !path.is_file() {
                return Vec::new();
            }
            let mut paths = vec![path.clone()];
            if matches!(reader, Reader::Claude) {
                paths.extend(claude_subagents(&path.with_extension("")));
            }
            paths
        }
        (_, SessionKey::Id(id)) if !plain_file_name(id) => Vec::new(),
        (Reader::Claude, SessionKey::Id(id)) => {
            let mut paths = Vec::new();
            for project in roots.claude_projects.iter().flat_map(|root| subdirs(root)) {
                let transcript = project.join(format!("{id}.jsonl"));
                if transcript.is_file() {
                    paths.push(transcript);
                    paths.extend(claude_subagents(&project.join(id)));
                }
            }
            paths
        }
        (Reader::Codex, SessionKey::Id(id)) => {
            let suffix = format!("-{id}.jsonl");
            let mut paths = Vec::new();
            for root in &roots.codex_sessions {
                find_files(
                    root,
                    4,
                    &mut |name| name.starts_with("rollout-") && name.ends_with(&suffix),
                    &mut paths,
                );
            }
            paths
        }
        (Reader::Pi, SessionKey::Id(id)) => {
            let suffix = format!("_{id}.jsonl");
            let mut paths = Vec::new();
            for root in &roots.pi_sessions {
                find_files(root, 2, &mut |name| name.ends_with(&suffix), &mut paths);
            }
            paths
        }
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The transcripts of a Claude session's subagents, in `<session>/subagents/`.
fn claude_subagents(session_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    find_files(
        &session_dir.join("subagents"),
        1,
        &mut |name| name.ends_with(".jsonl"),
        &mut paths,
    );
    paths
}

fn find_files(
    dir: &Path,
    depth: usize,
    matches: &mut dyn FnMut(&str) -> bool,
    found: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth > 1 {
                find_files(&path, depth - 1, matches, found);
            }
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(&mut *matches)
        {
            found.push(path);
        }
    }
}

fn for_each_line(path: &Path, mut handle: impl FnMut(Value)) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            return;
        };
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            handle(value);
        }
    }
}

/// Whether an entry with this `timestamp` counts with `--since`.
fn in_range(timestamp: Option<&Value>, since: Option<OffsetDateTime>) -> bool {
    let Some(since) = since else {
        return true;
    };
    timestamp
        .and_then(Value::as_str)
        .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
        .is_some_and(|time| time >= since)
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Claude Code writes one line per content block of a response, each with
/// the response's whole usage, so a response counts once by its message
/// and request ids, across all transcripts read.
fn read_claude(
    path: &Path,
    since: Option<OffsetDateTime>,
    seen_messages: &mut HashSet<String>,
    models: &mut Models,
) {
    for_each_line(path, |line| {
        if line.get("type").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let Some(message) = line.get("message") else {
            return;
        };
        let Some(usage) = message.get("usage") else {
            return;
        };
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if model == "<synthetic>" || !in_range(line.get("timestamp"), since) {
            return;
        }
        let message_id = message.get("id").and_then(Value::as_str);
        let request_id = line.get("requestId").and_then(Value::as_str);
        let key = match (message_id, request_id) {
            (None, None) => line
                .get("uuid")
                .and_then(Value::as_str)
                .map(|uuid| format!("uuid:{uuid}")),
            (message_id, request_id) => Some(format!(
                "{}:{}",
                message_id.unwrap_or(""),
                request_id.unwrap_or("")
            )),
        };
        if let Some(key) = key {
            if !seen_messages.insert(key) {
                return;
            }
        }
        let cache_write = number(usage, "cache_creation_input_tokens");
        let (write_5m, write_1h) = match usage.get("cache_creation") {
            Some(split) => {
                let write_1h = number(split, "ephemeral_1h_input_tokens").min(cache_write);
                (cache_write - write_1h, write_1h)
            }
            None => (cache_write, 0),
        };
        let tokens = Tokens {
            input: number(usage, "input_tokens"),
            output: number(usage, "output_tokens"),
            cache_write_5m: write_5m,
            cache_write_1h: write_1h,
            cache_read: number(usage, "cache_read_input_tokens"),
        };
        models.add(model, tokens, None);
    });
}

/// Codex writes the session's running total with each `token_count`
/// event; each event counts the growth since the previous one, under the
/// model of the turn it belongs to.
fn read_codex(path: &Path, since: Option<OffsetDateTime>, models: &mut Models) {
    let mut model = String::from("codex (model unknown)");
    let mut previous = [0u64; 4];
    for_each_line(path, |line| {
        let Some(payload) = line.get("payload") else {
            return;
        };
        if line.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(name) = payload.get("model").and_then(Value::as_str) {
                model = name.to_string();
            }
            return;
        }
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            return;
        }
        let Some(total) = payload
            .get("info")
            .and_then(|info| info.get("total_token_usage"))
        else {
            return;
        };
        let current = [
            number(total, "input_tokens"),
            number(total, "cached_input_tokens"),
            number(total, "cache_write_input_tokens"),
            number(total, "output_tokens"),
        ];
        // A smaller total starts a new count (a new process on the file).
        let restarted = current
            .iter()
            .zip(previous)
            .any(|(now, before)| *now < before);
        let delta: Vec<u64> = if restarted {
            current.to_vec()
        } else {
            current
                .iter()
                .zip(previous)
                .map(|(now, before)| now - before)
                .collect()
        };
        previous = current;
        if delta.iter().all(|value| *value == 0) || !in_range(line.get("timestamp"), since) {
            return;
        }
        // OpenAI counts cached and cache-written tokens inside the input.
        let tokens = Tokens {
            input: delta[0].saturating_sub(delta[1]).saturating_sub(delta[2]),
            output: delta[3],
            cache_write_5m: delta[2],
            cache_write_1h: 0,
            cache_read: delta[1],
        };
        models.add(&model, tokens, None);
    });
}

/// pi writes each assistant message with its usage and the cost it
/// computed, used when herdr has no price for the model.
fn read_pi(path: &Path, since: Option<OffsetDateTime>, models: &mut Models) {
    for_each_line(path, |line| {
        if line.get("type").and_then(Value::as_str) != Some("message") {
            return;
        }
        let Some(message) = line.get("message") else {
            return;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let Some(usage) = message.get("usage") else {
            return;
        };
        if !in_range(line.get("timestamp"), since) {
            return;
        }
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let tokens = Tokens {
            input: number(usage, "input"),
            output: number(usage, "output"),
            cache_write_5m: number(usage, "cacheWrite"),
            cache_write_1h: 0,
            cache_read: number(usage, "cacheRead"),
        };
        let reported = usage
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(Value::as_f64);
        models.add(model, tokens, reported);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-usage-estimate-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write(path: &Path, lines: &[String]) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, lines.join("\n") + "\n").expect("write");
    }

    /// One Claude response, written as Claude Code does: one line per
    /// content block, each with the whole usage.
    fn claude_response(message_id: &str, model: &str, timestamp: &str) -> Vec<String> {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": timestamp,
            "requestId": format!("req_{message_id}"),
            "message": {
                "id": message_id,
                "model": model,
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 2000,
                    "cache_creation_input_tokens": 3000,
                    "cache_read_input_tokens": 4000,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 1000,
                        "ephemeral_1h_input_tokens": 2000
                    }
                }
            }
        })
        .to_string();
        vec![line.clone(), line]
    }

    fn sighting(workspace: &str, agent: &str, session: &str, seen_in: &str) -> SessionSighting {
        SessionSighting {
            workspace_id: Some(workspace.into()),
            agent: agent.into(),
            session: SessionKey::Id(session.into()),
            seen_in: seen_in.into(),
        }
    }

    fn spaces() -> Vec<WorkspaceRef> {
        vec![
            WorkspaceRef {
                workspace_id: "w1".into(),
                label: "one".into(),
            },
            WorkspaceRef {
                workspace_id: "w2".into(),
                label: "two".into(),
            },
        ]
    }

    fn claude_roots(root: &Path) -> TranscriptRoots {
        TranscriptRoots {
            claude_projects: vec![root.join("projects")],
            ..TranscriptRoots::default()
        }
    }

    #[test]
    fn a_resumed_session_counts_once() {
        let root = temp_root("resumed");
        write(
            &root.join("projects/-repo/s1.jsonl"),
            &claude_response("msg_1", "claude-opus-5-5", "2026-10-01T10:00:00Z"),
        );
        // Resumed in a second pane of the same space and in a tab of
        // another space.
        let sightings = vec![
            sighting("w1", "claude", "s1", "pane p1"),
            sighting("w1", "claude", "s1", "pane p2"),
            sighting("w2", "claude", "s1", "pane p3"),
        ];
        let report = build_report(&spaces(), &sightings, &claude_roots(&root), None, None);

        let one = &report.workspaces[0];
        assert_eq!(one.sessions.len(), 1);
        assert_eq!(
            one.sessions[0].seen_in,
            vec!["pane p1", "pane p2", "pane p3"]
        );
        assert_eq!(one.sessions[0].also_in_workspaces, vec!["w2"]);
        let model = &one.models[0];
        assert_eq!(model.model, "claude-opus-5-5");
        // Both lines of the response are one response.
        assert_eq!(model.input_tokens, 1000);
        assert_eq!(model.output_tokens, 2000);
        assert_eq!(model.cache_write_tokens, 3000);
        assert_eq!(model.cache_read_tokens, 4000);
        assert!(report.workspaces[1].sessions.is_empty());
        assert_eq!(report.workspaces[1].cost_usd, 0.0);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_response_copied_into_a_resumed_session_counts_once() {
        let root = temp_root("copied");
        let mut resumed = claude_response("msg_1", "claude-opus-5-5", "2026-10-01T10:00:00Z");
        resumed.extend(claude_response(
            "msg_2",
            "claude-opus-5-5",
            "2026-10-02T10:00:00Z",
        ));
        write(
            &root.join("projects/-repo/s1.jsonl"),
            &claude_response("msg_1", "claude-opus-5-5", "2026-10-01T10:00:00Z"),
        );
        write(&root.join("projects/-repo/s2.jsonl"), &resumed);
        let sightings = vec![
            sighting("w1", "claude", "s1", "pane p1"),
            sighting("w1", "claude", "s2", "pane p2"),
        ];
        let report = build_report(&spaces(), &sightings, &claude_roots(&root), None, None);
        assert_eq!(report.workspaces[0].models[0].input_tokens, 2000);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_worker_counts_under_its_owner_space() {
        let root = temp_root("worker");
        write(
            &root.join("projects/-slot/ws.jsonl"),
            &claude_response("msg_w", "claude-sonnet-5-5", "2026-10-01T10:00:00Z"),
        );
        let panes = HashMap::from([("p-owner".to_string(), "w2".to_string())]);
        // Started in w1, owned by a coordinator pane in w2.
        let space = worker_workspace(Some("p-owner"), Some("w1"), &panes);
        assert_eq!(space.as_deref(), Some("w2"));
        // Without a live owner pane it counts where it started.
        assert_eq!(
            worker_workspace(Some("gone"), Some("w1"), &panes).as_deref(),
            Some("w1")
        );
        let sightings = vec![SessionSighting {
            workspace_id: space,
            agent: "claude".into(),
            session: SessionKey::Id("ws".into()),
            seen_in: "worker wk1".into(),
        }];
        let report = build_report(&spaces(), &sightings, &claude_roots(&root), None, None);
        assert!(report.workspaces[0].sessions.is_empty());
        assert_eq!(report.workspaces[1].sessions[0].seen_in, vec!["worker wk1"]);
        assert_eq!(report.workspaces[1].models[0].output_tokens, 2000);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cost_comes_from_the_price_table() {
        let root = temp_root("cost");
        write(
            &root.join("projects/-repo/s1.jsonl"),
            &claude_response("msg_1", "claude-sonnet-5-5", "2026-10-01T10:00:00Z"),
        );
        let report = build_report(
            &spaces(),
            &[sighting("w1", "claude", "s1", "pane p1")],
            &claude_roots(&root),
            None,
            None,
        );
        // Sonnet 5.5: $2 in, $10 out, $2.50 5m write, $4 1h write, $0.20 read.
        let expected =
            (1000.0 * 2.0 + 2000.0 * 10.0 + 1000.0 * 2.5 + 2000.0 * 4.0 + 4000.0 * 0.2) / 1e6;
        let one = &report.workspaces[0];
        assert!((one.cost_usd - expected).abs() < 1e-12);
        assert_eq!(one.models[0].cost_source, Some("price_table"));
        assert!(!one.cost_incomplete);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_report_says_it_is_an_estimate() {
        let report = build_report(&spaces(), &[], &TranscriptRoots::default(), None, None);
        assert_eq!(report.estimate, "estimate, not the provider's bill");
        let json = serde_json::to_value(&report).expect("json");
        assert_eq!(json["estimate"], "estimate, not the provider's bill");
        assert_eq!(json["prices_as_of"], prices::PRICES_AS_OF);
    }

    #[test]
    fn since_drops_older_entries() {
        let root = temp_root("since");
        let mut lines = claude_response("msg_old", "claude-opus-5-5", "2026-09-30T23:59:59Z");
        lines.extend(claude_response(
            "msg_new",
            "claude-opus-5-5",
            "2026-10-01T00:00:00Z",
        ));
        write(&root.join("projects/-repo/s1.jsonl"), &lines);
        let since = parse_since("2026-10-01", 0).expect("date");
        let report = build_report(
            &spaces(),
            &[sighting("w1", "claude", "s1", "pane p1")],
            &claude_roots(&root),
            Some(since),
            None,
        );
        assert_eq!(report.workspaces[0].models[0].input_tokens, 1000);
        assert_eq!(report.since.as_deref(), Some("2026-10-01 00:00 +00:00"));
        assert!(parse_since("yesterday", 0).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn since_a_date_starts_at_local_midnight() {
        let root = temp_root("since-local");
        // Local midnight of 2026-10-01 at +02:00 is 2026-09-30T22:00:00Z.
        let mut lines = claude_response("msg_old", "claude-opus-5-5", "2026-09-30T21:59:59Z");
        lines.extend(claude_response(
            "msg_new",
            "claude-opus-5-5",
            "2026-09-30T22:00:00Z",
        ));
        write(&root.join("projects/-repo/s1.jsonl"), &lines);
        let since = parse_since("2026-10-01", 2 * 3600).expect("date");
        assert_eq!(
            since.0,
            OffsetDateTime::parse("2026-09-30T22:00:00Z", &Rfc3339).expect("time")
        );
        let report = build_report(
            &spaces(),
            &[sighting("w1", "claude", "s1", "pane p1")],
            &claude_roots(&root),
            Some(since),
            None,
        );
        assert_eq!(report.workspaces[0].models[0].input_tokens, 1000);
        assert_eq!(report.since.as_deref(), Some("2026-10-01 00:00 +02:00"));
        // West of UTC, and an RFC 3339 time printed in local time.
        let west = parse_since("2026-10-01", -(5 * 3600 + 30 * 60)).expect("date");
        assert_eq!(
            west.0,
            OffsetDateTime::parse("2026-10-01T05:30:00Z", &Rfc3339).expect("time")
        );
        assert_eq!(west.1, "2026-10-01 00:00 -05:30");
        let rfc = parse_since("2026-10-01T12:00:00Z", 2 * 3600).expect("time");
        assert_eq!(rfc.1, "2026-10-01 14:00 +02:00");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn codex_counts_growth_of_the_running_total() {
        let root = temp_root("codex");
        let count = |input: u64, cached: u64, output: u64| {
            serde_json::json!({
                "timestamp": "2026-10-01T10:00:00Z",
                "type": "event_msg",
                "payload": {"type": "token_count", "info": {"total_token_usage": {
                    "input_tokens": input, "cached_input_tokens": cached,
                    "output_tokens": output
                }}}
            })
            .to_string()
        };
        let lines = vec![
            serde_json::json!({"type": "turn_context", "payload": {"model": "gpt-6.1-sol"}})
                .to_string(),
            count(100, 40, 10),
            count(100, 40, 10),
            count(300, 140, 30),
        ];
        write(
            &root.join("sessions/2026/10/01/rollout-2026-10-01T10-00-00-abc.jsonl"),
            &lines,
        );
        let roots = TranscriptRoots {
            codex_sessions: vec![root.join("sessions")],
            ..TranscriptRoots::default()
        };
        let report = build_report(
            &spaces(),
            &[sighting("w1", "codex", "abc", "pane p1")],
            &roots,
            None,
            None,
        );
        let one = &report.workspaces[0];
        let model = &one.models[0];
        assert_eq!(model.model, "gpt-6.1-sol");
        assert_eq!(model.input_tokens, 160);
        assert_eq!(model.cache_read_tokens, 140);
        assert_eq!(model.output_tokens, 30);
        // herdr has no OpenAI prices.
        assert_eq!(model.cost_usd, None);
        assert!(one.cost_incomplete);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pi_uses_its_reported_cost_without_a_price() {
        let root = temp_root("pi");
        let path = root.join("sessions/--repo--/2026-10-01_xyz.jsonl");
        let line = serde_json::json!({
            "type": "message",
            "timestamp": "2026-10-01T10:00:00Z",
            "message": {"role": "assistant", "model": "gpt-6.1-sol", "usage": {
                "input": 10, "output": 5, "cacheRead": 2, "cacheWrite": 0,
                "cost": {"total": 0.5}
            }}
        })
        .to_string();
        write(&path, &[line]);
        let sightings = vec![SessionSighting {
            workspace_id: Some("w1".into()),
            agent: "pi".into(),
            session: SessionKey::Path(path.display().to_string()),
            seen_in: "pane p1".into(),
        }];
        let report = build_report(
            &spaces(),
            &sightings,
            &TranscriptRoots::default(),
            None,
            None,
        );
        let model = &report.workspaces[0].models[0];
        assert_eq!(model.cost_usd, Some(0.5));
        assert_eq!(model.cost_source, Some("agent_reported"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unreadable_and_missing_sessions_say_why() {
        let sightings = vec![
            sighting("w1", "copilot", "c1", "pane p1"),
            sighting("w1", "claude", "missing", "pane p2"),
            sighting("w1", "claude", "../escape", "pane p3"),
            SessionSighting {
                workspace_id: Some("gone".into()),
                agent: "claude".into(),
                session: SessionKey::Id("x".into()),
                seen_in: "worker wk9".into(),
            },
        ];
        let report = build_report(
            &spaces(),
            &sightings,
            &TranscriptRoots::default(),
            None,
            None,
        );
        let notes: Vec<_> = report.workspaces[0]
            .sessions
            .iter()
            .map(|s| s.note.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            notes,
            vec![
                "herdr cannot read copilot transcripts",
                "no transcript found",
                "no transcript found"
            ]
        );
        assert_eq!(report.workspaces[2].workspace_id, None);
        let only = build_report(
            &spaces(),
            &sightings,
            &TranscriptRoots::default(),
            None,
            Some("w2"),
        );
        assert_eq!(only.workspaces.len(), 1);
        assert_eq!(only.workspaces[0].workspace_id.as_deref(), Some("w2"));
        assert!(only.workspaces[0].sessions.is_empty());
    }
}
