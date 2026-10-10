//! `herdr report` and `herdr reports`: protocol problems coordinators
//! report themselves (`report.record`, `report.list`, `report.close`), and
//! the `herdr report ...` line herdr's own protocol errors print with the
//! facts filled in ([`print_hint`]).

use std::io::Read;

use crate::api::schema::{
    Method, ReportCloseParams, ReportEvidenceUpload, ReportInfo, ReportListParams,
    ReportRecordParams, ReportState, Request,
};
use crate::workers::reports::EVIDENCE_MAX_BYTES;

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr report --kind KIND --summary TEXT [--evidence PATH] [--uncertain]
               [--command TEXT] [--json]
      Record a protocol problem: a herdr command, wait, sandbox or skill step
      that did not give its promised outcome or a clear next step (a wait that
      ended without a verdict, an unexplained exit code, a headless worker
      blocked by the sandbox, a skill step written for one repository, an
      error that does not say the way out), including a mistake unclear
      instructions caused. Not a worker's expected turn, a worker's wrong
      answer or the repository's own bug. KIND is a slug (lowercase letters,
      digits, -); TEXT one line. --uncertain when you are not sure it is
      herdr's; the herdr coordinator decides. --command is the command the
      problem came from. The first 64 KiB of PATH are kept in herdr's state,
      so a temporary file may go away. Herdr records your repository, pane,
      agent session and the time, and fingerprints KIND with TEXT (ids, paths,
      numbers and commit hashes taken out): prints `report r-N, occurrence
      K`. A new fingerprint notifies the herdr repository's coordinator once
      (the user when none holds it); repeats only count.
  herdr report close <r-N> (--fix ITEM_OR_COMMIT | --not-reproducible)
      Close a report with the TODO item (t-...) or commit that fixed it. Only
      from the pane that holds the herdr repository's coordination tenure. A
      later occurrence reopens it and notifies again.
  herdr reports [--open | --all] [--json]
      The reports, the latest occurrence first: the open ones (default) or
      all, each with its occurrences (time, repository, pane, session).";

pub(super) fn run_report_command(args: &[String]) -> std::io::Result<i32> {
    let (method, json) = match parse_report(args) {
        Ok(Some(parsed)) => parsed,
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
    let response = super::send_request(&Request {
        id: "cli:report".into(),
        method,
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    let result = &response["result"];
    match result["type"].as_str() {
        Some("report_recorded") => {
            println!(
                "report {}, occurrence {}",
                result["report"]["report_id"].as_str().unwrap_or("?"),
                result["occurrence"]
            );
            Ok(0)
        }
        Some("report") => match serde_json::from_value::<ReportInfo>(result["report"].clone()) {
            Ok(report) => {
                println!("report {} {}", report.report_id, closure(&report));
                Ok(0)
            }
            Err(_) => super::print_response(&response),
        },
        _ => super::print_response(&response),
    }
}

pub(super) fn run_reports_command(args: &[String]) -> std::io::Result<i32> {
    let mut all = false;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--all" => all = true,
            "--open" => all = false,
            "--json" => json = true,
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(0);
            }
            other => {
                eprintln!("unexpected argument: {other}");
                eprintln!("{USAGE}");
                return Ok(2);
            }
        }
    }
    let response = super::send_request(&Request {
        id: "cli:reports".into(),
        method: Method::ReportList(ReportListParams { all }),
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    match serde_json::from_value::<Vec<ReportInfo>>(response["result"]["reports"].clone()) {
        Ok(reports) => {
            print!(
                "{}",
                reports_text(&reports, crate::usage::local_utc_offset_secs())
            );
            Ok(0)
        }
        Err(_) => super::print_response(&response),
    }
}

/// `Ok(None)` asks for help; the flag says `--json`.
fn parse_report(args: &[String]) -> Result<Option<(Method, bool)>, String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        return Ok(None);
    }
    let json = args.iter().any(|arg| arg == "--json");
    let rest: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--json")
        .cloned()
        .collect();
    if rest.first().map(String::as_str) == Some("close") {
        let rest = &rest[1..];
        let (fix, rest) = take_string_option(rest, "--fix")?;
        let not_reproducible = rest.iter().any(|arg| arg == "--not-reproducible");
        let rest: Vec<&String> = rest
            .iter()
            .filter(|arg| *arg != "--not-reproducible")
            .collect();
        let [report_id] = rest.as_slice() else {
            return Err("close takes one report id (r-N)".into());
        };
        if fix.is_some() == not_reproducible {
            return Err("close takes --fix ITEM_OR_COMMIT or --not-reproducible".into());
        }
        return Ok(Some((
            Method::ReportClose(ReportCloseParams {
                report_id: (*report_id).clone(),
                fix,
                not_reproducible,
                pane_id: super::target::caller_pane_id(),
            }),
            json,
        )));
    }
    let uncertain = rest.iter().any(|arg| arg == "--uncertain");
    let rest: Vec<String> = rest
        .into_iter()
        .filter(|arg| arg != "--uncertain")
        .collect();
    let (kind, rest) = take_string_option(&rest, "--kind")?;
    let (summary, rest) = take_string_option(&rest, "--summary")?;
    let (evidence, rest) = take_string_option(&rest, "--evidence")?;
    let (command, rest) = take_string_option(&rest, "--command")?;
    if !rest.is_empty() {
        return Err(format!("unexpected arguments: {}", rest.join(" ")));
    }
    let (Some(kind), Some(summary)) = (kind, summary) else {
        return Err("report takes --kind and --summary".into());
    };
    let pane_id = super::target::caller_pane_id();
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|dir| std::path::absolute(dir).ok())
        .map(|dir| dir.display().to_string());
    Ok(Some((
        Method::ReportRecord(ReportRecordParams {
            kind,
            summary,
            uncertain,
            command,
            evidence: evidence.map(|path| read_evidence(&path)).transpose()?,
            session_id: pane_id
                .as_ref()
                .and(std::env::var("CLAUDE_CODE_SESSION_ID").ok())
                .filter(|session| !session.trim().is_empty()),
            pane_id,
            cwd,
        }),
        json,
    )))
}

/// The first [`EVIDENCE_MAX_BYTES`] of the file, as text.
fn read_evidence(path: &str) -> Result<ReportEvidenceUpload, String> {
    let source = std::path::absolute(path)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| path.to_owned());
    let file = std::fs::File::open(path).map_err(|error| format!("--evidence {path}: {error}"))?;
    let bytes = file
        .metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| format!("--evidence {path}: {error}"))?;
    let mut content = Vec::new();
    file.take(EVIDENCE_MAX_BYTES as u64)
        .read_to_end(&mut content)
        .map_err(|error| format!("--evidence {path}: {error}"))?;
    let mut text = String::from_utf8_lossy(&content).into_owned();
    // A replacement character may make the text longer than the bytes read.
    while text.len() > EVIDENCE_MAX_BYTES {
        text.pop();
    }
    Ok(ReportEvidenceUpload {
        source,
        content: text,
        bytes,
        truncated: bytes > EVIDENCE_MAX_BYTES as u64,
    })
}

fn closure(report: &ReportInfo) -> String {
    match (report.state, report.fix.as_deref(), report.not_reproducible) {
        (ReportState::Closed, _, true) => "closed, not reproducible".into(),
        (ReportState::Closed, Some(fix), _) => format!("closed, fix {fix}"),
        (ReportState::Closed, None, _) => "closed".into(),
        _ => "open".into(),
    }
}

fn reports_text(reports: &[ReportInfo], offset: i64) -> String {
    if reports.is_empty() {
        return "no reports\n".into();
    }
    let when = |ms: u64| super::history::when(ms, offset);
    let mut out = String::new();
    for report in reports {
        out.push_str(&format!(
            "{}  {}  {}  {} occurrence{}{}{}\n    {}\n",
            report.report_id,
            closure(report),
            report.kind,
            report.count,
            if report.count == 1 { "" } else { "s" },
            if report.uncertain { "  uncertain" } else { "" },
            if report.reopened > 0 {
                format!("  reopened {}x", report.reopened)
            } else {
                String::new()
            },
            report.summary
        ));
        for occurrence in &report.occurrences {
            let mut line = format!("    {}  #{}", when(occurrence.ts_ms), occurrence.number);
            for (label, value) in [
                ("repo", occurrence.repo.as_deref()),
                ("pane", occurrence.pane_id.as_deref()),
                ("session", occurrence.session_id.as_deref()),
                ("coordinator", occurrence.coordinator_id.as_deref()),
                ("reopened", occurrence.reopened.as_deref()),
            ] {
                if let Some(value) = value {
                    line.push_str(&format!("  {label} {value}"));
                }
            }
            if occurrence.uncertain {
                line.push_str("  uncertain");
            }
            out.push_str(&line);
            out.push('\n');
            if occurrence.number > 1 && occurrence.summary != report.summary {
                out.push_str(&format!("        {}\n", occurrence.summary));
            }
            if let Some(command) = &occurrence.command {
                out.push_str(&format!("        command: {command}\n"));
            }
            if let Some(evidence) = &occurrence.evidence {
                out.push_str(&format!("        evidence: {evidence}\n"));
            }
        }
    }
    out
}

/// `text` as one shell word.
fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return text.to_owned();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The `herdr report` command for a protocol problem herdr saw itself, with
/// its kind, its summary (one line, at most 300 characters) and the
/// command it came from.
pub(super) fn hint(kind: &str, summary: &str, command: &str) -> String {
    let summary: String = summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
    format!(
        "herdr: this is a herdr protocol problem; if it left you without a next step, report \
         it:\n  herdr report --kind {} --summary {} --command {}",
        shell_quote(kind),
        shell_quote(&summary),
        shell_quote(command)
    )
}

/// Prints [`hint`] to stderr.
pub(super) fn print_hint(kind: &str, summary: &str, command: &str) {
    eprintln!("{}", hint(kind, summary, command));
}

/// The command line as the caller ran it: `herdr` and `args`.
pub(super) fn command_line(words: &[&str], args: &[String]) -> String {
    words
        .iter()
        .map(|word| (*word).to_owned())
        .chain(args.iter().cloned())
        .map(|word| shell_quote(&word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a worker question is herdr's worker policy asking about a
/// command the sandbox would refuse: a Bash command naming a path outside
/// the worker's directories.
pub(super) fn is_sandbox_question(question: &serde_json::Value) -> bool {
    question["reason"]
        .as_str()
        .is_some_and(|reason| reason.starts_with("a Bash command that names "))
}

/// The protocol problem a reply of `todo wait`, `worker wait` or `worker
/// verify` shows, as a report's kind and summary: a verify that could not
/// run a check (`unavailable` is never a pass, and says no next step), a
/// worker the server lost, or herdr's worker policy asking about a Bash
/// command the sandbox would refuse.
pub(super) fn problem_in_reply(result: &serde_json::Value) -> Option<(&'static str, String)> {
    let verification = [&result["verification"], &result["event"]["verification"]]
        .into_iter()
        .find(|verification| verification.is_object());
    if let Some(verification) = verification {
        if verification["verdict"] == "unavailable" {
            let check = verification["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|check| check["outcome"] == "unavailable");
            let named = check
                .map(|check| {
                    let name = check["name"]
                        .as_str()
                        .or(check["check"].as_str())
                        .unwrap_or("?");
                    let detail = check["detail"]
                        .as_str()
                        .and_then(|detail| detail.lines().next())
                        .unwrap_or_default();
                    format!("check {name} could not run: {detail}")
                })
                .unwrap_or_else(|| "a check could not run".into());
            return Some((
                "verify-unavailable",
                format!("the verify was unavailable, no verdict: {named}"),
            ));
        }
    }
    let worker = [&result["worker"], &result["event"]["worker"]]
        .into_iter()
        .find(|worker| worker.is_object());
    if worker.is_some_and(|worker| worker["state"] == "lost") {
        return Some((
            "worker-lost",
            "a worker wait ended with the worker lost: its server ended while it ran, no verdict"
                .into(),
        ));
    }
    let questions = [&result["questions"], &result["event"]["questions"]]
        .into_iter()
        .filter_map(serde_json::Value::as_array)
        .flatten();
    for question in questions {
        if is_sandbox_question(question) {
            return Some((
                "worker-sandbox-refusal",
                "herdr's worker policy asked about a headless worker's Bash command that names a path outside its directory and temp dir, which the sandbox refuses"
                    .into(),
            ));
        }
    }
    None
}

/// Prints the hint for the problem `response` shows, if any.
pub(super) fn hint_reply(response: &serde_json::Value, command: &str) {
    if let Some((kind, summary)) = problem_in_reply(&response["result"]) {
        print_hint(kind, &summary, command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn report_and_close_parse_their_options() {
        let Ok(Some((Method::ReportRecord(params), json))) = parse_report(&args(&[
            "--kind",
            "todo-wait",
            "--summary",
            "gave up",
            "--uncertain",
            "--command",
            "herdr todo wait r-1",
            "--json",
        ])) else {
            panic!("report did not parse");
        };
        assert!(json && params.uncertain);
        assert_eq!(
            (params.kind.as_str(), params.summary.as_str()),
            ("todo-wait", "gave up")
        );
        assert_eq!(params.command.as_deref(), Some("herdr todo wait r-1"));
        assert!(params.cwd.is_some());
        assert!(parse_report(&args(&["--kind", "x"])).is_err());
        assert!(parse_report(&args(&["--kind", "x", "--summary", "y", "extra"])).is_err());
        assert!(parse_report(&args(&[
            "--kind",
            "x",
            "--summary",
            "y",
            "--evidence",
            "/no/such"
        ]))
        .is_err());

        let Ok(Some((Method::ReportClose(close), _))) =
            parse_report(&args(&["close", "r-3", "--fix", "t-abcd2345"]))
        else {
            panic!("close did not parse");
        };
        assert_eq!(
            (close.report_id.as_str(), close.fix.as_deref()),
            ("r-3", Some("t-abcd2345"))
        );
        let Ok(Some((Method::ReportClose(close), _))) =
            parse_report(&args(&["close", "r-3", "--not-reproducible"]))
        else {
            panic!("close --not-reproducible did not parse");
        };
        assert!(close.not_reproducible);
        assert!(parse_report(&args(&["close", "r-3"])).is_err());
        assert!(
            parse_report(&args(&["close", "r-3", "--fix", "x", "--not-reproducible"])).is_err()
        );
        assert!(matches!(parse_report(&args(&["--help"])), Ok(None)));
    }

    #[test]
    fn evidence_is_read_up_to_the_bound() {
        let dir = std::env::temp_dir().join(format!("herdr-cli-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        std::fs::write(&path, vec![b'x'; EVIDENCE_MAX_BYTES + 10]).unwrap();
        let evidence = read_evidence(&path.display().to_string()).unwrap();
        assert_eq!(evidence.content.len(), EVIDENCE_MAX_BYTES);
        assert_eq!(evidence.bytes, EVIDENCE_MAX_BYTES as u64 + 10);
        assert!(evidence.truncated);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_hint_is_a_command_a_shell_runs_as_printed() {
        let hint = hint(
            "todo-wait-gave-up",
            "todo wait r-1 gave up:\n it's gone",
            &command_line(&["herdr", "todo", "wait"], &args(&["r-1", "--after", "7"])),
        );
        assert!(
            hint.ends_with(
                "herdr report --kind todo-wait-gave-up --summary 'todo wait r-1 gave up: \
                 it'\\''s gone' --command 'herdr todo wait r-1 --after 7'"
            ),
            "{hint}"
        );
        assert!(is_sandbox_question(&serde_json::json!({
            "reason": "a Bash command that names /etc, outside the worker's directory and its temp dir"
        })));
        assert!(!is_sandbox_question(
            &serde_json::json!({"reason": "a question for the user"})
        ));
    }

    #[test]
    fn replies_without_a_next_step_name_their_problem() {
        let unavailable = serde_json::json!({
            "type": "todo_event",
            "event": {"kind": "verify_failed", "verification": {"verdict": "unavailable",
                "checks": [{"check": "commits", "outcome": "passed"},
                    {"check": "command", "name": "tests", "outcome": "unavailable",
                     "detail": "cargo: not found\nmore"}]}},
        });
        assert_eq!(
            problem_in_reply(&unavailable),
            Some((
                "verify-unavailable",
                "the verify was unavailable, no verdict: check tests could not run: cargo: not found"
                    .into()
            ))
        );
        let lost = serde_json::json!({"type": "worker_info", "worker": {"state": "lost"}});
        assert_eq!(
            problem_in_reply(&lost).map(|(kind, _)| kind),
            Some("worker-lost")
        );
        let sandbox = serde_json::json!({"questions": [{"reason":
            "a Bash command that names /etc, outside the worker's directory and its temp dir"}]});
        assert_eq!(
            problem_in_reply(&sandbox).map(|(kind, _)| kind),
            Some("worker-sandbox-refusal")
        );
        for fine in [
            serde_json::json!({"verification": {"verdict": "verified"}}),
            serde_json::json!({"worker": {"state": "finished"}}),
            serde_json::json!({"event": {"questions": [{"reason": "a question for the user"}]}}),
        ] {
            assert_eq!(problem_in_reply(&fine), None, "{fine}");
        }
    }

    #[test]
    fn reports_list_each_occurrence() {
        let report = ReportInfo {
            report_id: "r-1".into(),
            kind: "wait".into(),
            summary: "w1 lost".into(),
            normalized: "<id> lost".into(),
            fingerprint: "0123456789abcdef".into(),
            state: ReportState::Closed,
            uncertain: false,
            first_ms: 0,
            last_ms: 60_000,
            count: 2,
            reopened: 0,
            closed_ms: Some(60_000),
            closed_by: Some("c-abcdefgh".into()),
            fix: Some("t-abcd2345".into()),
            not_reproducible: false,
            occurrences: vec![
                crate::api::schema::ReportOccurrence {
                    number: 1,
                    ts_ms: 0,
                    summary: "w1 lost".into(),
                    uncertain: false,
                    repo: Some("/repo".into()),
                    pane_id: Some("w1:p2".into()),
                    session_id: None,
                    coordinator_id: None,
                    command: Some("herdr worker wait w1".into()),
                    evidence: None,
                    evidence_source: None,
                    evidence_truncated: false,
                    reopened: None,
                },
                crate::api::schema::ReportOccurrence {
                    number: 2,
                    ts_ms: 60_000,
                    summary: "w7 lost".into(),
                    uncertain: true,
                    repo: None,
                    pane_id: None,
                    session_id: Some("s-1".into()),
                    coordinator_id: None,
                    command: None,
                    evidence: None,
                    evidence_source: None,
                    evidence_truncated: false,
                    reopened: None,
                },
            ],
        };
        assert_eq!(
            reports_text(&[report], 0),
            "r-1  closed, fix t-abcd2345  wait  2 occurrences\n    w1 lost\n\
             \x20   1970-01-01 00:00  #1  repo /repo  pane w1:p2\n\
             \x20       command: herdr worker wait w1\n\
             \x20   1970-01-01 00:01  #2  session s-1  uncertain\n\
             \x20       w7 lost\n"
        );
        assert_eq!(reports_text(&[], 0), "no reports\n");
    }
}
