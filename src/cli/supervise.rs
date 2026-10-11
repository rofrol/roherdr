//! `herdr supervise --report`: the server's supervision findings over the
//! coordinators and their workers (`supervise.report`), as text or, with
//! `--json`, as the server's reply.

use crate::api::schema::{
    AgentStatus, Method, Request, SuperviseReportParams, SupervisionFinding, SupervisionReport,
    SupervisionSeverity, SupervisionStatus,
};

const USAGE: &str = "usage:
  herdr supervise --report [--all] [--json]
      What herdr's supervision sweep (on the server's timer, and when a
      coordinator's pane reports a turn or changes state) saw of each active
      coordinator, and the findings that hold (open or acknowledged; with
      --all also the resolved ones), the latest first. A finding names a
      positive fact and the event that did not follow: a coordinator idle
      with runnable items, no open decision and no registered wait; a pane
      working after its agent's Stop with no new UserPromptSubmit; a
      coordinator whose turn ended with background tasks and no wait edge; a
      repository's disk below the build guard's 15 GiB. A coordinator without
      one shows \"cannot establish progress\": quiet output is never a
      finding. The sweep only reports: it never waits, retries, prompts,
      kills, restarts, clears a state or marks work done.";

pub(super) fn run_supervise_command(args: &[String]) -> std::io::Result<i32> {
    let (params, json) = match parse(args) {
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
        id: "cli:supervise".into(),
        method: Method::SuperviseReport(params),
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    match serde_json::from_value::<SupervisionReport>(response["result"]["report"].clone()) {
        Ok(report) => {
            print!(
                "{}",
                report_text(&report, crate::usage::local_utc_offset_secs())
            );
            Ok(0)
        }
        Err(_) => super::print_response(&response),
    }
}

/// `Ok(None)` asks for help; the flag says `--json`.
fn parse(args: &[String]) -> Result<Option<(SuperviseReportParams, bool)>, String> {
    let mut report = false;
    let mut all = false;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--report" => report = true,
            "--all" => all = true,
            "--json" => json = true,
            "help" | "--help" | "-h" => return Ok(None),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    if !report {
        return Err("supervise takes --report".into());
    }
    Ok(Some((SuperviseReportParams { all }, json)))
}

fn agent_status(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

fn finding_text(finding: &SupervisionFinding, when: &impl Fn(u64) -> String) -> String {
    let status = match finding.status {
        SupervisionStatus::Open => "open",
        SupervisionStatus::Acknowledged => "acknowledged",
        SupervisionStatus::Resolved => "resolved",
        SupervisionStatus::Unknown => "unknown",
    };
    let severity = match finding.severity {
        SupervisionSeverity::Warning => "warning",
        SupervisionSeverity::Critical => "critical",
        SupervisionSeverity::Unknown => "unknown",
    };
    let predicate = serde_json::to_value(finding.predicate)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    let mut out = format!(
        "{}  {status}  {severity}  {predicate}  since {}, last seen {}\n    {}\n    expected: {}\n",
        finding.finding_id,
        when(finding.first_seen_ms),
        when(finding.last_seen_ms),
        finding.summary,
        finding.expected_event
    );
    for id in &finding.evidence.event_ids {
        out.push_str(&format!("    event: {id}\n"));
    }
    for fact in &finding.evidence.facts {
        out.push_str(&format!("    {fact}\n"));
    }
    if let Some(resolution) = &finding.resolution {
        out.push_str(&format!("    resolved: {resolution}\n"));
    }
    out
}

fn report_text(report: &SupervisionReport, offset: i64) -> String {
    let when = |ms: u64| super::history::when(ms, offset);
    let mut out = match (report.swept_ms, &report.trigger) {
        (Some(ms), Some(trigger)) => format!("swept {} ({trigger})\n", when(ms)),
        (Some(ms), None) => format!("swept {}\n", when(ms)),
        _ => "no sweep yet on this server\n".to_owned(),
    };
    if report.coordinators.is_empty() {
        out.push_str("no active coordinator\n");
    }
    for coordinator in &report.coordinators {
        let mut line = format!("{}  {}", coordinator.coordinator_id, coordinator.repo);
        if let Some(pane) = &coordinator.pane_id {
            line.push_str(&format!("  pane {pane}"));
        }
        if let Some(status) = coordinator.agent_status {
            line.push_str(&format!("  {}", agent_status(status)));
        }
        if !coordinator.workers.is_empty() {
            line.push_str(&format!("  workers {}", coordinator.workers.join(" ")));
        }
        if let Some(run) = &coordinator.active_run {
            line.push_str(&format!("  run {run}"));
        }
        if !coordinator.findings.is_empty() {
            line.push_str(&format!("  findings {}", coordinator.findings.join(" ")));
        }
        if let Some(note) = &coordinator.note {
            line.push_str(&format!("  {note}"));
        }
        out.push_str(&line);
        out.push('\n');
    }
    if report.findings.is_empty() {
        out.push_str("no findings\n");
    }
    for finding in &report.findings {
        out.push_str(&finding_text(finding, &when));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{SupervisedCoordinator, SupervisionEvidence, SupervisionPredicate};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn supervise_needs_report() {
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["--report", "--bogus"])).is_err());
        assert_eq!(parse(&args(&["--help"])), Ok(None));
        assert_eq!(
            parse(&args(&["--report", "--all", "--json"])),
            Ok(Some((SuperviseReportParams { all: true }, true)))
        );
    }

    #[test]
    fn the_report_lists_coordinators_then_findings() {
        let report = SupervisionReport {
            swept_ms: Some(0),
            trigger: Some("timer".into()),
            coordinators: vec![SupervisedCoordinator {
                coordinator_id: "c-aaaaaaaa".into(),
                repo: "/repo".into(),
                pane_id: Some("w1:p1".into()),
                agent_status: Some(AgentStatus::Working),
                workers: Vec::new(),
                active_run: None,
                findings: Vec::new(),
                note: Some("cannot establish progress".into()),
            }],
            findings: vec![SupervisionFinding {
                finding_id: "f-1".into(),
                fingerprint: "0123456789abcdef".into(),
                predicate: SupervisionPredicate::DiskLow,
                severity: SupervisionSeverity::Warning,
                status: SupervisionStatus::Open,
                summary: "9.0 GiB free under /repo".into(),
                expected_event: "free space back".into(),
                repo: Some("/repo".into()),
                coordinator_id: None,
                pane_id: None,
                run_id: None,
                job_ids: Vec::new(),
                evidence: SupervisionEvidence {
                    observed_ms: 0,
                    event_ids: Vec::new(),
                    facts: vec!["free_bytes: 1".into()],
                },
                first_seen_ms: 0,
                last_seen_ms: 0,
                acknowledged_ms: None,
                resolved_ms: None,
                resolution: None,
            }],
        };
        let text = report_text(&report, 0);
        assert!(
            text.contains("c-aaaaaaaa  /repo  pane w1:p1  working  cannot establish progress"),
            "{text}"
        );
        assert!(text.contains("f-1  open  warning  disk_low"), "{text}");
        assert!(text.contains("    free_bytes: 1"), "{text}");
    }
}
