//! `herdr supervise --report`: the server's supervision findings over the
//! coordinators and their workers (`supervise.report`), as text or, with
//! `--json`, as the server's reply; `herdr supervise --ack`
//! (`supervise.acknowledge`) for the herdr repository's coordinator; `herdr
//! supervise ack` (`supervise.ack`) for a coordinator a finding's notice was
//! addressed to.

use crate::api::schema::{
    AgentStatus, Method, Request, SuperviseAckParams, SuperviseAcknowledgeParams,
    SuperviseReportParams, SupervisionDelivery, SupervisionFinding, SupervisionReport,
    SupervisionSeverity, SupervisionStatus,
};

const USAGE: &str = "usage:
  herdr supervise --report [--all] [--json]
  herdr supervise --ack FINDING_ID [--json]
  herdr supervise ack FINDING_ID [--json]
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
      finding. The sweep only reports: it never waits, retries, kills,
      restarts, clears a state or marks work done.
      Supervision makes no sound and no notification: findings show in the
      Supervision tab and the `?` list. This report is the herdr repository's
      coordinator's inbox: read from that pane, it marks the findings that
      hold seen by its tenure, and shows `new` beside those it had not seen.
      Herdr also types a notice of each new or escalated finding (a fixed
      template starting with `herdr supervision`; the finding's text quoted
      as data) into the pane of the coordinator that owns it, and a digest
      of every owner's findings, or the unowned ones, into the herdr
      repository's coordinator's pane, only when that agent rests (a turn
      boundary) and no client has its pane focused; the notices wait while
      it works or while you are in its pane.
  --ack FINDING_ID
      Acknowledges an open finding: only from the pane that holds the herdr
      repository's coordination tenure, refused otherwise with
      finding_ack_refused. You acknowledge yours in the `?` list; a worker
      or another agent never does.
  ack FINDING_ID
      Acknowledges the notices of a finding addressed to your coordination
      tenure, from the pane that holds it (reading the report does not). If
      your turn after a notice ends without it, the finding's `?` entry says
      so.
      From the herdr repository's coordinator it also acknowledges the
      finding, as --ack does.";

pub(super) fn run_supervise_command(args: &[String]) -> std::io::Result<i32> {
    let (command, json) = match parse(args) {
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
    let pane_id = super::target::caller_pane_id();
    let method = match command {
        Supervise::Report { all } => Method::SuperviseReport(SuperviseReportParams {
            all,
            pane_id: pane_id.clone(),
        }),
        Supervise::Ack(finding_id) => Method::SuperviseAcknowledge(SuperviseAcknowledgeParams {
            finding_id,
            pane_id,
        }),
        Supervise::AckNotice(finding_id) => Method::SuperviseAck(SuperviseAckParams {
            finding_id,
            pane_id,
        }),
    };
    let ack = matches!(
        method,
        Method::SuperviseAcknowledge(_) | Method::SuperviseAck(_)
    );
    let response = super::send_request(&Request {
        id: "cli:supervise".into(),
        method,
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    if ack {
        return match serde_json::from_value::<SupervisionFinding>(
            response["result"]["finding"].clone(),
        ) {
            Ok(finding) => {
                println!("{} {}", finding.finding_id, status_text(finding.status));
                Ok(0)
            }
            Err(_) => super::print_response(&response),
        };
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

/// What `herdr supervise` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Supervise {
    Report {
        all: bool,
    },
    Ack(String),
    /// `ack FINDING_ID`: the notices addressed to the caller's tenure.
    AckNotice(String),
}

/// `Ok(None)` asks for help; the flag says `--json`.
fn parse(args: &[String]) -> Result<Option<(Supervise, bool)>, String> {
    if args.first().map(String::as_str) == Some("ack") {
        let mut finding = None;
        let mut json = false;
        for arg in &args[1..] {
            match arg.as_str() {
                "--json" => json = true,
                "help" | "--help" | "-h" => return Ok(None),
                other if other.starts_with("--") => {
                    return Err(format!("unexpected argument: {other}"))
                }
                other if finding.is_none() => finding = Some(other.to_owned()),
                other => return Err(format!("unexpected argument: {other}")),
            }
        }
        return match finding {
            Some(finding) => Ok(Some((Supervise::AckNotice(finding), json))),
            None => Err("ack takes a finding id (f-N)".into()),
        };
    }
    let mut report = false;
    let mut ack = None;
    let mut all = false;
    let mut json = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--report" => report = true,
            "--ack" => match args.next() {
                Some(finding) if !finding.starts_with("--") => ack = Some(finding.clone()),
                _ => return Err("--ack takes a finding id (f-N)".into()),
            },
            "--all" => all = true,
            "--json" => json = true,
            "help" | "--help" | "-h" => return Ok(None),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    match (report, ack) {
        (true, None) => Ok(Some((Supervise::Report { all }, json))),
        (false, Some(finding)) if !all => Ok(Some((Supervise::Ack(finding), json))),
        (false, Some(_)) => Err("--all goes with --report, not --ack".into()),
        (true, Some(_)) => Err("supervise takes --report or --ack, not both".into()),
        (false, None) => Err("supervise takes --report or --ack".into()),
    }
}

fn status_text(status: SupervisionStatus) -> &'static str {
    match status {
        SupervisionStatus::Open => "open",
        SupervisionStatus::Acknowledged => "acknowledged",
        SupervisionStatus::Resolved => "resolved",
        SupervisionStatus::Unknown => "unknown",
    }
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

/// One finding; `herdr` is the herdr repository's active tenure, whose
/// inbox it is: one that tenure has not read shows `new`.
fn finding_text(
    finding: &SupervisionFinding,
    herdr: Option<&str>,
    when: &impl Fn(u64) -> String,
) -> String {
    let status = status_text(finding.status);
    let severity = match finding.severity {
        SupervisionSeverity::Warning => "warning",
        SupervisionSeverity::Critical => "critical",
        SupervisionSeverity::Unknown => "unknown",
    };
    let predicate = serde_json::to_value(finding.predicate)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    let new = match herdr {
        Some(herdr)
            if finding.status != SupervisionStatus::Resolved
                && finding.coordinator_seen_by.as_deref() != Some(herdr) =>
        {
            "  new"
        }
        _ => "",
    };
    let mut out = format!(
        "{}{new}  {status}  {severity}  {predicate}  since {}, last seen {}\n    {}\n    expected: \
         {}\n",
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
    if let (Some(by), Some(ms)) = (&finding.acknowledged_by, finding.acknowledged_ms) {
        out.push_str(&format!("    acknowledged by {by} {}\n", when(ms)));
    }
    match (&finding.coordinator_seen_by, finding.coordinator_seen_ms) {
        (Some(by), Some(ms)) => {
            out.push_str(&format!("    seen by coordinator {by} {}\n", when(ms)));
        }
        _ if finding.status != SupervisionStatus::Resolved => {
            out.push_str("    not seen by the herdr coordinator yet\n");
        }
        _ => {}
    }
    if let Some(resolution) = &finding.resolution {
        out.push_str(&format!("    resolved: {resolution}\n"));
    }
    for delivery in &finding.deliveries {
        out.push_str(&format!("    notice: {}\n", delivery_text(delivery, when)));
    }
    out
}

/// Where one notice of a finding stands.
fn delivery_text(delivery: &SupervisionDelivery, when: &impl Fn(u64) -> String) -> String {
    let role = serde_json::to_value(delivery.role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    let who = format!("{} ({role})", delivery.coordinator_id);
    if let (Some(ms), Some(reason)) = (delivery.dropped_ms, &delivery.drop_reason) {
        return format!("{who} dropped {}: {reason}", when(ms));
    }
    let mut text = match delivery.dispatched_ms {
        Some(ms) => format!("{who} delivered {}", when(ms)),
        None => format!("{who} queued {}, not delivered", when(delivery.queued_ms)),
    };
    if let Some(error) = &delivery.dispatch_error {
        text.push_str(&format!(", prompt failed: {error}"));
    }
    match (delivery.acknowledged_ms, delivery.alerted_ms) {
        (Some(ms), _) => text.push_str(&format!(", acknowledged {}", when(ms))),
        (None, Some(ms)) => text.push_str(&format!(
            ", its next turn ended without an ack (user alerted {})",
            when(ms)
        )),
        (None, None) if delivery.dispatched_ms.is_some() => text.push_str(", not acknowledged"),
        _ => {}
    }
    text
}

fn report_text(report: &SupervisionReport, offset: i64) -> String {
    let when = |ms: u64| super::history::when(ms, offset);
    let mut out = match (report.swept_ms, &report.trigger) {
        (Some(ms), Some(trigger)) => format!("swept {} ({trigger})\n", when(ms)),
        (Some(ms), None) => format!("swept {}\n", when(ms)),
        _ => "no sweep yet on this server\n".to_owned(),
    };
    match &report.herdr_coordinator_id {
        Some(herdr) => out.push_str(&format!("herdr coordinator {herdr}\n")),
        None => out.push_str("no herdr coordinator: findings notify you\n"),
    }
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
    for count in &report.hold_counts {
        out.push_str(&format!(
            "holds {:?} rule v{}: retracted {}, delivered {}, resolved {}\n",
            count.predicate, count.rule_version, count.retracted, count.delivered, count.resolved
        ));
    }
    for hold in &report.pending_holds {
        out.push_str(&format!(
            "pending {:?} rule v{} on {}  jobs {}  since {}\n",
            hold.predicate,
            hold.rule_version,
            hold.request_id,
            hold.job_ids.join(" "),
            when(hold.opened_ms)
        ));
    }
    if report.findings.is_empty() {
        out.push_str("no findings\n");
    }
    for finding in &report.findings {
        out.push_str(&finding_text(
            finding,
            report.herdr_coordinator_id.as_deref(),
            &when,
        ));
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
            Ok(Some((Supervise::Report { all: true }, true)))
        );
        assert_eq!(
            parse(&args(&["--ack", "f-3"])),
            Ok(Some((Supervise::Ack("f-3".into()), false)))
        );
        assert_eq!(
            parse(&args(&["ack", "f-3", "--json"])),
            Ok(Some((Supervise::AckNotice("f-3".into()), true)))
        );
        for bad in [
            &["ack"][..],
            &["ack", "f-3", "f-4"],
            &["ack", "f-3", "--all"],
            &["--ack"][..],
            &["--ack", "--json"],
            &["--ack", "f-3", "--report"],
            &["--ack", "f-3", "--all"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_report_lists_coordinators_then_findings() {
        let report = SupervisionReport {
            swept_ms: Some(0),
            trigger: Some("timer".into()),
            herdr_coordinator_id: Some("c-aaaaaaaa".into()),
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
                acknowledged_by: None,
                coordinator_seen_ms: None,
                coordinator_seen_by: None,
                resolved_ms: None,
                resolution: None,
                deliveries: vec![SupervisionDelivery {
                    coordinator_id: "c-bbbbbbbb".into(),
                    role: crate::api::schema::SupervisionDeliveryRole::Unowned,
                    state_key: "f-1:warning".into(),
                    queued_ms: 0,
                    dispatched_ms: None,
                    pane_id: None,
                    prompt_request_id: None,
                    dispatch_error: None,
                    turn_started_ms: None,
                    acknowledged_ms: None,
                    alerted_ms: None,
                    dropped_ms: None,
                    drop_reason: None,
                }],
            }],
            hold_counts: vec![crate::api::schema::SupervisionHoldCount {
                predicate: SupervisionPredicate::StoppedWithoutVerdict,
                rule_version: 1,
                retracted: 2,
                delivered: 1,
                resolved: 0,
            }],
            pending_holds: Vec::new(),
        };
        let text = report_text(&report, 0);
        assert!(
            text.contains("rule v1: retracted 2, delivered 1, resolved 0"),
            "{text}"
        );
        assert!(
            text.contains("c-aaaaaaaa  /repo  pane w1:p1  working  cannot establish progress"),
            "{text}"
        );
        assert!(text.contains("herdr coordinator c-aaaaaaaa"), "{text}");
        assert!(text.contains("f-1  new  open  warning  disk_low"), "{text}");
        assert!(text.contains("    free_bytes: 1"), "{text}");
        assert!(
            text.contains("    notice: c-bbbbbbbb (unowned) queued ")
                && text.contains(", not delivered\n"),
            "{text}"
        );
        assert!(
            text.contains("not seen by the herdr coordinator yet"),
            "{text}"
        );

        // Once the herdr coordinator's tenure read it, it is no longer new.
        let mut seen = report.clone();
        seen.findings[0].coordinator_seen_by = Some("c-aaaaaaaa".into());
        seen.findings[0].coordinator_seen_ms = Some(0);
        seen.findings[0].status = SupervisionStatus::Acknowledged;
        seen.findings[0].acknowledged_by = Some("user".into());
        seen.findings[0].acknowledged_ms = Some(0);
        let text = report_text(&seen, 0);
        assert!(
            text.contains("f-1  acknowledged  warning  disk_low"),
            "{text}"
        );
        assert!(text.contains("seen by coordinator c-aaaaaaaa"), "{text}");
        assert!(text.contains("acknowledged by user"), "{text}");
    }
}
