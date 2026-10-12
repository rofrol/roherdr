//! The context pack of a run's model calls ([`super::auto_draft`],
//! [`super::auto_review`]): what the server knows about the item, built by
//! herdr with no model and given to the call, so a stateless call does not
//! judge the item without its history. It holds
//!
//! - the item's text,
//! - the `DECISIONS.md` sections the item names,
//! - the item's run history: each earlier run's records, and this run's
//!   failed verifies, retry reviews and aborts, each with its reason,
//! - `git log --oneline -15` for the paths the call is about (a review: the
//!   run's path globs; a draft: the paths the item's text names that exist
//!   in the repository),
//! - the lines of up to 20 hits (file and line) for the identifiers the
//!   item's text names in backticks and that exist in the repository
//!   (`rg`, or `git grep` where `rg` is not installed).
//!
//! The pack is cut deterministically to [`PACK_MAX`] bytes (code hits from
//! the last, then log lines from the last, then the oldest history records,
//! then decisions from the last) and says what was cut.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use super::auto_review::failed_checks;
use super::{git, Run};
use crate::api::schema::TodoRunEvent;
use crate::workers::WorkerSupervisor;

/// The pack's size, in bytes of its JSON.
pub(super) const PACK_MAX: usize = 40_000;
/// What the notes on the cut may add to the pack.
const CUT_NOTES_RESERVE: usize = 1_000;
/// The item's text is cut at this many characters.
const ITEM_TEXT_MAX: usize = 10_000;
const HITS_MAX: usize = 20;
const HITS_PER_SYMBOL: usize = 5;
const SYMBOLS_MAX: usize = 10;
const PATHS_MAX: usize = 12;
const HIT_TEXT_MAX: usize = 300;
/// A history record's text is cut at this many characters.
const HISTORY_TEXT_MAX: usize = 2_000;
/// Files that name the items themselves: a hit there is noise.
const NOTES: [&str; 3] = ["TODO.md", "DECISIONS.md", "TODO-deferred.md"];

/// The paths whose commits the pack lists.
pub(super) enum LogPaths<'a> {
    /// The run's path globs.
    Globs(&'a [String]),
    /// The paths the item's text names, as far as they exist.
    Named,
}

/// What the pack holds before it is bounded.
struct Parts {
    item_text: String,
    decisions: Vec<Value>,
    history: Vec<Value>,
    log: Vec<String>,
    hits: Vec<Value>,
}

fn cut_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_owned(),
    }
}

/// The pack, built from the repository (`rev` for the log, when given) and
/// the pieces the caller read from the store.
pub(super) fn build(
    repo: &Path,
    rev: Option<&str>,
    item_text: &str,
    decisions: Vec<Value>,
    history: Vec<Value>,
    log_paths: LogPaths<'_>,
) -> Value {
    let paths: Vec<String> = match log_paths {
        LogPaths::Globs(globs) => globs.to_vec(),
        LogPaths::Named => named_paths(repo, item_text),
    };
    let parts = Parts {
        item_text: item_text.to_owned(),
        decisions,
        history,
        log: log_of(repo, rev, &paths),
        hits: code_hits(repo, item_text),
    };
    bound(parts, PACK_MAX)
}

fn render(parts: &Parts, cut: &[String]) -> Value {
    json!({
        "item_text": parts.item_text,
        "decisions": parts.decisions,
        "run_history": parts.history,
        "git_log": parts.log,
        "code_hits": parts.hits,
        "cut": cut,
    })
}

/// The pack as JSON, cut to `max` bytes with a record of what was cut.
fn bound(mut parts: Parts, max: usize) -> Value {
    let mut cut: Vec<String> = Vec::new();
    if parts.item_text.chars().count() > ITEM_TEXT_MAX {
        parts.item_text = cut_chars(&parts.item_text, ITEM_TEXT_MAX);
        cut.push(format!("item_text: cut at {ITEM_TEXT_MAX} characters"));
    }
    let before = (
        parts.decisions.len(),
        parts.history.len(),
        parts.log.len(),
        parts.hits.len(),
    );
    let limit = max.saturating_sub(CUT_NOTES_RESERVE);
    while render(&parts, &[]).to_string().len() > limit {
        if parts.hits.pop().is_some() || parts.log.pop().is_some() {
            continue;
        }
        if !parts.history.is_empty() {
            // The oldest record goes first.
            parts.history.remove(0);
            continue;
        }
        if parts.decisions.pop().is_some() {
            continue;
        }
        break;
    }
    for (name, was, now) in [
        ("code_hits", before.3, parts.hits.len()),
        ("git_log", before.2, parts.log.len()),
        ("run_history (oldest first)", before.1, parts.history.len()),
        ("decisions (last first)", before.0, parts.decisions.len()),
    ] {
        if now < was {
            cut.push(format!("{name}: kept {now} of {was} to fit {max} bytes"));
        }
    }
    render(&parts, &cut)
}

/// `git log --oneline -15 [rev] -- <paths>`, one string per commit; empty
/// without paths or when git cannot read it.
fn log_of(repo: &Path, rev: Option<&str>, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["log", "--oneline", "-15"];
    args.extend(rev);
    args.push("--");
    args.extend(paths.iter().map(String::as_str));
    git(repo, &args)
        .map(|log| log.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The paths the item's text names that exist in the repository: tokens
/// with a `/` or a file extension, relative, inside it, not the notes files.
pub(super) fn named_paths(repo: &Path, text: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let separators = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '(' | ')' | '[' | ']' | ',' | ';' | ':' | '"' | '\''
            )
    };
    for token in text.split(separators) {
        let token = token.trim_end_matches(['.', '/']);
        let has_extension = token.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty()
                && (1..=5).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
        });
        if token.is_empty()
            || !(token.contains('/') || has_extension)
            || token.starts_with('/')
            || token.split('/').any(|part| part == ".." || part.is_empty())
            || NOTES.contains(&token)
            || paths.iter().any(|path| path == token)
            || !repo.join(token).exists()
        {
            continue;
        }
        paths.push(token.to_owned());
        if paths.len() == PATHS_MAX {
            break;
        }
    }
    paths
}

/// The identifiers the text names in backticks: letters, digits, `_` and
/// `::`, starting with a letter or `_`, at least 3 characters, a trailing
/// `()` dropped.
pub(super) fn identifiers(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    // The odd segments are inside backticks.
    for segment in text.split('`').skip(1).step_by(2) {
        let name = segment.trim().trim_end_matches("()");
        let starts = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        let valid = name.len() >= 3
            && starts
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
            && !name.ends_with(':');
        if valid && !found.iter().any(|known| known == name) {
            found.push(name.to_owned());
            if found.len() == SYMBOLS_MAX {
                break;
            }
        }
    }
    found
}

/// One search's lines as `(file, line, text)`: `rg`, else `git grep`.
fn search(repo: &Path, symbol: &str) -> Vec<(String, u64, String)> {
    let mut command = Command::new("rg");
    command
        .current_dir(repo)
        .args([
            "--no-heading",
            "--line-number",
            "--color",
            "never",
            "--sort",
            "path",
            "--fixed-strings",
            "--word-regexp",
            "--max-count",
            "5",
            "--max-columns",
            "300",
        ])
        .stdin(Stdio::null());
    for name in NOTES {
        command.args(["--glob", &format!("!{name}")]);
    }
    command.args(["--", symbol, "."]);
    let text = match command.output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => {
            let excluded: Vec<String> = NOTES
                .iter()
                .map(|name| format!(":(exclude){name}"))
                .collect();
            let mut args = vec!["grep", "-n", "-I", "-w", "-F", "--no-color", "-e", symbol];
            args.extend(["--", "."]);
            args.extend(excluded.iter().map(String::as_str));
            git(repo, &args).unwrap_or_default()
        }
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            let file = fields.next()?.trim_start_matches("./").to_owned();
            let number = fields.next()?.parse().ok()?;
            let found = fields.next().unwrap_or_default().trim().to_owned();
            Some((file, number, found))
        })
        .take(HITS_PER_SYMBOL)
        .collect()
}

/// Up to [`HITS_MAX`] hits for the identifiers the text names that exist in
/// the repository, in the order the text names them.
fn code_hits(repo: &Path, text: &str) -> Vec<Value> {
    let mut hits = Vec::new();
    for symbol in identifiers(text) {
        for (file, line, found) in search(repo, &symbol) {
            if hits.len() == HITS_MAX {
                return hits;
            }
            hits.push(json!({
                "symbol": symbol, "file": file, "line": line,
                "text": cut_chars(&found, HIT_TEXT_MAX),
            }));
        }
    }
    hits
}

impl WorkerSupervisor {
    /// The item's text as the run's claim recorded it.
    pub(super) fn item_text_of(&self, run: &Run) -> String {
        self.run_store()
            .ok()
            .and_then(|store| store.run_events_of(&run.info.run_id, "run_created").ok())
            .and_then(|created| created.into_iter().next())
            .and_then(|created| created["item_text"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The item's run history: the records of its earlier runs, then this
    /// run's failed verifies, retry reviews and aborts, each with its
    /// reason.
    pub(super) fn run_history_of(&self, run: &Run) -> Vec<Value> {
        let Ok(store) = self.run_store() else {
            return Vec::new();
        };
        let mut history: Vec<Value> = store
            .item_history(Some(&run.info.repo), Some(&run.info.item))
            .unwrap_or_default()
            .into_iter()
            .map(|stored| stored.event)
            .filter(|event| event.run_id.as_deref() != Some(run.info.run_id.as_str()))
            .map(|event| {
                json!({
                    "run": event.run_id, "attempt": event.attempt, "kind": event.kind,
                    "ts_ms": event.ts_ms,
                    "text": event.text.map(|text| cut_chars(&text, HISTORY_TEXT_MAX)),
                })
            })
            .collect();
        let events = |kind: &str| {
            store
                .run_events_of(&run.info.run_id, kind)
                .unwrap_or_default()
        };
        for body in events("run_event") {
            let Ok(event) = serde_json::from_value::<TodoRunEvent>(body["event"].clone()) else {
                continue;
            };
            let attempt = body["attempt"].clone();
            match body["kind"].as_str() {
                Some("verify_failed") => {
                    let failed = event
                        .verification
                        .as_ref()
                        .map(failed_checks)
                        .unwrap_or_default();
                    history.push(json!({
                        "run": run.info.run_id, "attempt": attempt, "kind": "verify_failed",
                        "failed_checks": failed,
                    }));
                }
                Some(kind @ ("aborted" | "blocked")) => {
                    history.push(json!({
                        "run": run.info.run_id, "attempt": attempt, "kind": kind,
                        "text": event.error.map(|text| cut_chars(&text, HISTORY_TEXT_MAX)),
                    }));
                }
                _ => {}
            }
        }
        let retries = events("run_resumed")
            .into_iter()
            .chain(events("run_auto_reviewed"))
            .filter(|body| body["action"] == "retry");
        for body in retries {
            let text = body["task"].as_str().unwrap_or_default();
            history.push(json!({
                "run": run.info.run_id, "kind": "retry_review",
                "text": cut_chars(text, HISTORY_TEXT_MAX),
            }));
        }
        for body in events("run_resumed")
            .into_iter()
            .filter(|body| body["action"] == "abort")
        {
            history.push(json!({
                "run": run.info.run_id, "kind": "abort",
                "text": body["message"].as_str().map(|text| cut_chars(text, HISTORY_TEXT_MAX)),
            }));
        }
        history
    }
}

#[cfg(test)]
mod tests;
