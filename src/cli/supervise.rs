//! `herdr supervise --report`: the server's supervision findings over the
//! coordinators and their workers (`supervise.report`), as text or, with
//! `--json`, as the server's reply; `herdr supervise --ack`
//! (`supervise.acknowledge`) for the herdr repository's coordinator; `herdr
//! supervise ack` (`supervise.ack`) for a coordinator a finding's notice was
//! addressed to; `herdr supervise replay` for a candidate rule replayed
//! against a predicate's recorded cases (`supervise.fixtures`).

use crate::api::schema::{
    AgentStatus, Method, Request, SuperviseAckParams, SuperviseAcknowledgeParams,
    SuperviseAnnotateParams, SuperviseFixturesParams, SuperviseReportParams, SupervisionAnnotation,
    SupervisionDelivery, SupervisionFinding, SupervisionFixtures, SupervisionMissing,
    SupervisionMissingRequest, SupervisionPredicate, SupervisionReport, SupervisionSeverity,
    SupervisionStatus,
};
use crate::workers::supervision_replay::{self, ReplayReport};

const USAGE: &str = "usage:
  herdr supervise --report [--all] [--json]
  herdr supervise --ack FINDING_ID [--json]
  herdr supervise ack FINDING_ID [--json]
  herdr supervise annotate FINDING_ID --missing <fact|rule> --evidence TEXT [--note TEXT] [--json]
  herdr supervise fixtures PREDICATE [--version N]
  herdr supervise replay --candidate FILE [--fixtures FILE] [--hold-out FILE] [--json]
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
      finding, as --ack does.
  fixtures PREDICATE [--version N]
      Prints JSON, read only: the holds recorded for a predicate
      (stopped_without_verdict or unaccounted_assignment) at its rule version
      (the current one by default): `retracted`, the cases that must not
      fire, `delivered`, the known-true cases that must still fire, and
      `annotated`, the cases the predicate cannot express, each with its
      fingerprint, request id, job ids, counter-evidence, opened and ended
      times and the reason it ended (an annotation: its evidence handle, note
      and when it was added). A `predicate_review_due` finding points here.
  annotate FINDING_ID --missing <fact|rule> --evidence TEXT [--note TEXT]
      Records that the predicate that raised the finding cannot express a
      case: a fact the sweep cannot see (`fact`) or a rule it has none of
      (`rule`). `--evidence` names where the case can be seen (a job id, a
      pane, a log path, a commit), `--note` adds free text. It changes
      nothing of the finding: not its state, its delivery, its
      acknowledgement or the holds, and it is not a dispute; `fixtures`
      lists it under `annotated`, at the rule version the finding was raised
      at (a finding recorded before herdr stored that version records none).
      One annotation per finding (`already_annotated`); an id that names no
      finding is `finding_not_found`.
  replay --candidate FILE [--fixtures FILE] [--hold-out FILE] [--json]
      Replays a predicate's recorded cases against the rule a fix proposes,
      before the fix is accepted; it reads the cases and the candidate only,
      and touches no pane, job or ledger. FILE holds the candidate as data,
      from the rule table's own typed vocabulary: predicate, version,
      counter_evidence (each entry with a kind, an owner and a completion)
      and expected_event, every one a value of that vocabulary; an unknown
      predicate, kind, owner, completion or expected event is refused naming
      the field, and no field defaults. A candidate whose predicate differs
      from the fixtures' is refused, and so is one at the rule version the
      cases were recorded at: a fix is a new version. --fixtures names a
      `fixtures` reply (the default: the server's current one for the
      candidate's predicate) and --hold-out a second set the fixer was not
      shown.
      A recorded case is a hold, not a finding that fired: its
      counter-evidence held it. It was retracted when its expected event
      happened (a false positive prevented) and delivered when the
      continuation ended without it (the finding fired after the hold). The
      candidate holds a case exactly when one of its entries names the
      case's recorded counter-evidence, so: every retracted case must still
      be held and the candidate must expect its recorded event, or the false
      positive comes back; every delivered case must have the candidate
      expect its recorded event, or the same continuation would not deliver
      the known-true finding again; and the set must have a retracted and a
      delivered case it can decide, or it cannot discriminate and fails.
      Annotated cases are listed and decide nothing, and so does a recorded
      text the vocabulary cannot read: it is reported as unrepresentable,
      never as delivered. The hold-out runs the same checks, its result
      joins the exit status and its cases stay out of the primary totals;
      without it the replay is a quick look, not an acceptance. Exits 0 when
      every check passes on the fixtures and the hold-out, 1 when a case
      fails, 2 for a bad candidate or file, and 3 when every check passes
      but no --hold-out set was replayed, so nothing was accepted.";

pub(super) fn run_supervise_command(args: &[String]) -> std::io::Result<i32> {
    if args.first().map(String::as_str) == Some("replay") {
        return match parse_replay(&args[1..]) {
            Ok(Some((replay, json))) => replay_command(
                &replay.candidate,
                replay.fixtures.as_deref(),
                replay.hold_out.as_deref(),
                json,
            ),
            Ok(None) => {
                println!("{USAGE}");
                Ok(0)
            }
            Err(message) => {
                eprintln!("{message}");
                eprintln!("{USAGE}");
                Ok(2)
            }
        };
    }
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
    let fixtures = matches!(command, Supervise::Fixtures { .. });
    let annotating = matches!(command, Supervise::Annotate { .. });
    let method = match command {
        Supervise::Fixtures { predicate, version } => {
            Method::SuperviseFixtures(SuperviseFixturesParams { predicate, version })
        }
        Supervise::Annotate {
            finding_id,
            missing,
            evidence,
            note,
        } => Method::SuperviseAnnotate(SuperviseAnnotateParams {
            finding_id,
            missing,
            evidence,
            note,
        }),
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
    if json || fixtures || response.get("error").is_some() {
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
    if annotating {
        return match serde_json::from_value::<SupervisionAnnotation>(
            response["result"]["annotation"].clone(),
        ) {
            Ok(annotation) => {
                println!("{}", annotation_text(&annotation));
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
    /// `fixtures PREDICATE [--version N]`.
    Fixtures {
        predicate: SupervisionPredicate,
        version: Option<u32>,
    },
    /// `annotate FINDING_ID --missing <fact|rule> --evidence TEXT [--note TEXT]`.
    Annotate {
        finding_id: String,
        missing: SupervisionMissingRequest,
        evidence: String,
        note: Option<String>,
    },
}

/// What `herdr supervise replay` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayArgs {
    candidate: String,
    fixtures: Option<String>,
    hold_out: Option<String>,
}

/// `replay --candidate FILE [--fixtures FILE] [--hold-out FILE] [--json]`: `Ok(None)` asks for
/// help, the flag says `--json`.
fn parse_replay(args: &[String]) -> Result<Option<(ReplayArgs, bool)>, String> {
    let mut candidate = None;
    let mut fixtures = None;
    let mut hold_out = None;
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--candidate" => match rest.next() {
                Some(value) if !value.starts_with("--") => candidate = Some(value.clone()),
                _ => return Err("--candidate takes a file".into()),
            },
            "--fixtures" => match rest.next() {
                Some(value) if !value.starts_with("--") => fixtures = Some(value.clone()),
                _ => return Err("--fixtures takes a file".into()),
            },
            "--hold-out" => match rest.next() {
                Some(value) if !value.starts_with("--") => hold_out = Some(value.clone()),
                _ => return Err("--hold-out takes a file".into()),
            },
            "--json" => json = true,
            "help" | "--help" | "-h" => return Ok(None),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let Some(candidate) = candidate else {
        return Err("replay takes --candidate FILE".into());
    };
    Ok(Some((
        ReplayArgs {
            candidate,
            fixtures,
            hold_out,
        },
        json,
    )))
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
    if args.first().map(String::as_str) == Some("annotate") {
        let mut finding = None;
        let mut missing = None;
        let mut evidence = None;
        let mut note = None;
        let mut json = false;
        let mut rest = args[1..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--missing" => match rest.next().map(String::as_str) {
                    Some("fact") => missing = Some(SupervisionMissingRequest::Fact),
                    Some("rule") => missing = Some(SupervisionMissingRequest::Rule),
                    _ => return Err("--missing takes fact or rule".into()),
                },
                "--evidence" => match rest.next() {
                    Some(value) if !value.starts_with("--") => evidence = Some(value.clone()),
                    _ => return Err("--evidence takes where the missing case can be seen".into()),
                },
                "--note" => match rest.next() {
                    Some(value) if !value.starts_with("--") => note = Some(value.clone()),
                    _ => return Err("--note takes text".into()),
                },
                "--json" => json = true,
                "help" | "--help" | "-h" => return Ok(None),
                other if other.starts_with("--") => {
                    return Err(format!("unexpected argument: {other}"))
                }
                other if finding.is_none() => finding = Some(other.to_owned()),
                other => return Err(format!("unexpected argument: {other}")),
            }
        }
        let Some(finding_id) = finding else {
            return Err("annotate takes a finding id (f-N)".into());
        };
        let Some(missing) = missing else {
            return Err("annotate takes --missing fact or --missing rule".into());
        };
        let Some(evidence) = evidence else {
            return Err("annotate takes --evidence, where the missing case can be seen".into());
        };
        return Ok(Some((
            Supervise::Annotate {
                finding_id,
                missing,
                evidence,
                note,
            },
            json,
        )));
    }
    if args.first().map(String::as_str) == Some("fixtures") {
        let mut predicate = None;
        let mut version = None;
        let mut rest = args[1..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--version" => match rest.next().and_then(|value| value.parse::<u32>().ok()) {
                    Some(value) => version = Some(value),
                    None => return Err("--version takes a rule version number".into()),
                },
                "help" | "--help" | "-h" => return Ok(None),
                other if other.starts_with("--") => {
                    return Err(format!("unexpected argument: {other}"))
                }
                other if predicate.is_none() => predicate = Some(other.to_owned()),
                other => return Err(format!("unexpected argument: {other}")),
            }
        }
        let Some(name) = predicate else {
            return Err("fixtures takes a predicate name".into());
        };
        let predicate: SupervisionPredicate =
            serde_json::from_value(serde_json::Value::String(name.clone()))
                .unwrap_or(SupervisionPredicate::Unknown);
        if predicate == SupervisionPredicate::Unknown {
            return Err(format!("unknown predicate: {name}"));
        }
        return Ok(Some((Supervise::Fixtures { predicate, version }, true)));
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

/// One file the replay reads, refused with what it was for.
fn replay_file(path: &str, what: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("cannot read the {what} {path}: {error}"))
}

/// The cases the replay reads: the file's reply, or the server's current fixtures for the
/// candidate's predicate.
fn recorded_fixtures(
    predicate: SupervisionPredicate,
    path: Option<&str>,
) -> Result<SupervisionFixtures, String> {
    match path {
        Some(path) => {
            replay_file(path, "fixtures").and_then(|text| supervision_replay::parse_fixtures(&text))
        }
        None => fixtures_from_server(predicate),
    }
}

fn fixtures_from_server(predicate: SupervisionPredicate) -> Result<SupervisionFixtures, String> {
    let response = super::send_request(&Request {
        id: "cli:supervise:replay".into(),
        method: Method::SuperviseFixtures(SuperviseFixturesParams {
            predicate,
            version: None,
        }),
    })
    .map_err(|error| format!("cannot read the fixtures from the server: {error}"))?;
    if let Some(error) = response.get("error") {
        return Err(format!("the server refused the fixtures: {error}"));
    }
    serde_json::from_value::<SupervisionFixtures>(response["result"]["fixtures"].clone())
        .map_err(|error| format!("the server's fixtures reply is not readable: {error}"))
}

/// `herdr supervise replay`: replays a predicate's recorded cases against the rule a fix
/// proposes, before the fix is accepted. It reads the candidate and the cases only: no pane,
/// job, finding or hold is touched. Exit 0 when every check passes on the primary set and the
/// hold-out, 1 when a case fails, 2 for a bad candidate or file, and 3 when every check passes
/// but no hold-out set was replayed, which is not an acceptance.
fn replay_command(
    candidate_file: &str,
    fixtures_file: Option<&str>,
    hold_out_file: Option<&str>,
    json: bool,
) -> std::io::Result<i32> {
    let candidate = match replay_file(candidate_file, "candidate")
        .and_then(|text| supervision_replay::parse_candidate(&text))
    {
        Ok(candidate) => candidate,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let fixtures = match recorded_fixtures(candidate.predicate(), fixtures_file) {
        Ok(fixtures) => fixtures,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let mut report = match ReplayReport::new(&candidate, &fixtures) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    if let Some(path) = hold_out_file {
        let hold_out = match replay_file(path, "hold-out")
            .and_then(|text| supervision_replay::parse_fixtures(&text))
        {
            Ok(hold_out) => hold_out,
            Err(message) => {
                eprintln!("{message}");
                return Ok(2);
            }
        };
        report = match report.with_hold_out(&candidate, &hold_out) {
            Ok(report) => report,
            Err(message) => {
                eprintln!("{message}");
                return Ok(2);
            }
        };
    }
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.text());
    }
    Ok(replay_exit(&report))
}

/// The exit code of a replay: 0 when it accepted the candidate, 3 when every check passed but
/// no hold-out set was replayed (a quick look, not an acceptance), and 1 when a case failed.
fn replay_exit(report: &ReplayReport) -> i32 {
    if report.accepted() {
        0
    } else if report.passed() {
        3
    } else {
        1
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

/// What an annotation says is missing, as `herdr supervise annotate` prints it.
fn missing_text(missing: SupervisionMissing) -> &'static str {
    match missing {
        SupervisionMissing::Fact => "a fact the sweep cannot see",
        SupervisionMissing::Rule => "a rule the predicate has none of",
        SupervisionMissing::Unknown => "unknown",
    }
}

/// The wire name of a value, for the text output.
fn wire_name<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// One annotation on one line: the finding, the predicate and the rule version it was raised
/// at, what was missing and the evidence handle.
fn annotation_text(annotation: &SupervisionAnnotation) -> String {
    let version = annotation
        .rule_version
        .map(|version| format!("v{version}"))
        .unwrap_or_else(|| "version unknown".into());
    format!(
        "{}  {} rule {}  missing: {}  evidence: {}",
        annotation.finding_id,
        wire_name(annotation.predicate),
        version,
        missing_text(annotation.missing),
        annotation.evidence
    )
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
        assert_eq!(
            parse(&args(&[
                "fixtures",
                "stopped_without_verdict",
                "--version",
                "2"
            ])),
            Ok(Some((
                Supervise::Fixtures {
                    predicate: SupervisionPredicate::StoppedWithoutVerdict,
                    version: Some(2)
                },
                true
            )))
        );
        assert_eq!(
            parse(&args(&[
                "annotate",
                "f-3",
                "--missing",
                "fact",
                "--evidence",
                "job j-1",
                "--note",
                "no job id",
                "--json"
            ])),
            Ok(Some((
                Supervise::Annotate {
                    finding_id: "f-3".into(),
                    missing: SupervisionMissingRequest::Fact,
                    evidence: "job j-1".into(),
                    note: Some("no job id".into())
                },
                true
            )))
        );
        assert_eq!(
            parse(&args(&[
                "annotate",
                "f-3",
                "--missing",
                "rule",
                "--evidence",
                "log reports/j-1.log"
            ])),
            Ok(Some((
                Supervise::Annotate {
                    finding_id: "f-3".into(),
                    missing: SupervisionMissingRequest::Rule,
                    evidence: "log reports/j-1.log".into(),
                    note: None
                },
                false
            )))
        );
        for bad in [
            &["fixtures"][..],
            &["fixtures", "nonsense"],
            &["fixtures", "disk_low", "--version"],
            &["fixtures", "disk_low", "--version", "x"],
            &["annotate"][..],
            &["annotate", "f-3", "--evidence", "job j-1"],
            &[
                "annotate",
                "f-3",
                "--missing",
                "nonsense",
                "--evidence",
                "x",
            ],
            &["annotate", "f-3", "--missing", "fact"],
            &["annotate", "--missing", "fact", "--evidence", "x"],
            &[
                "annotate",
                "f-3",
                "--missing",
                "fact",
                "--evidence",
                "--json",
            ],
            &[
                "annotate",
                "f-3",
                "--missing",
                "fact",
                "--evidence",
                "x",
                "--note",
            ],
            &[
                "annotate",
                "f-3",
                "--missing",
                "fact",
                "--evidence",
                "x",
                "f-4",
            ],
            &[
                "annotate",
                "f-3",
                "--missing",
                "fact",
                "--evidence",
                "x",
                "--bogus",
            ],
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

    /// An annotation prints as one line: the finding, the predicate and the rule version it was
    /// raised at, what was missing and the evidence handle.
    #[test]
    fn an_annotation_prints_the_finding_what_is_missing_and_where_it_shows() {
        let annotation = SupervisionAnnotation {
            finding_id: "f-7".into(),
            fingerprint: Some("0123456789abcdef".into()),
            predicate: SupervisionPredicate::StoppedWithoutVerdict,
            rule_version: Some(1),
            job_ids: vec!["j-1".into()],
            pane_id: Some("w1:p2".into()),
            missing: SupervisionMissing::Fact,
            evidence: "job j-1".into(),
            note: Some("the sweep sees no job id".into()),
            added_ms: 0,
        };
        assert_eq!(
            annotation_text(&annotation),
            "f-7  stopped_without_verdict rule v1  missing: a fact the sweep cannot see  \
             evidence: job j-1"
        );
        let rule = SupervisionAnnotation {
            missing: SupervisionMissing::Rule,
            ..annotation
        };
        assert!(
            annotation_text(&rule).contains("missing: a rule the predicate has none of"),
            "{}",
            annotation_text(&rule)
        );
        // A finding recorded before herdr stored its rule version records none.
        let unknown = SupervisionAnnotation {
            rule_version: None,
            ..rule
        };
        assert!(
            annotation_text(&unknown).contains("rule version unknown"),
            "{}",
            annotation_text(&unknown)
        );
    }

    #[test]
    fn replay_takes_a_candidate_and_optional_recorded_sets() {
        assert_eq!(
            parse_replay(&args(&["--candidate", "candidate.json"])),
            Ok(Some((
                ReplayArgs {
                    candidate: "candidate.json".into(),
                    fixtures: None,
                    hold_out: None,
                },
                false,
            )))
        );
        assert_eq!(
            parse_replay(&args(&[
                "--candidate",
                "candidate.json",
                "--fixtures",
                "fixtures.json",
                "--hold-out",
                "hold-out.json",
                "--json",
            ])),
            Ok(Some((
                ReplayArgs {
                    candidate: "candidate.json".into(),
                    fixtures: Some("fixtures.json".into()),
                    hold_out: Some("hold-out.json".into()),
                },
                true,
            )))
        );
        assert_eq!(parse_replay(&args(&["--help"])), Ok(None));
        for bad in [
            &[][..],
            &["--fixtures", "fixtures.json"],
            &["--candidate"],
            &["--candidate", "--json"],
            &["--candidate", "candidate.json", "--hold-out"],
            &["--candidate", "candidate.json", "candidate.json"],
            &["--candidate", "candidate.json", "--bogus"],
        ] {
            assert!(parse_replay(&args(bad)).is_err(), "{bad:?}");
        }
    }

    /// A replay accepts only with a hold-out set: without one it exits 3 and says so, with one
    /// it exits 0, and a failed case exits 1.
    #[test]
    fn a_replay_without_a_hold_out_exits_three_instead_of_accepting() {
        let candidate = crate::workers::supervision_replay::parse_candidate(
            &serde_json::json!({
                "predicate": "stopped_without_verdict",
                "version": 2,
                "counter_evidence": [{
                    "kind": "live_supervised_job",
                    "owner": "worker_assignment_pane",
                    "completion": "worker_live_pane"
                }],
                "expected_event": "worker_next_turn_on_assignment"
            })
            .to_string(),
        )
        .unwrap();
        let recorded = |fingerprint: &str| {
            serde_json::json!({
                "fingerprint": fingerprint,
                "request_id": format!("req-{fingerprint}"),
                "counter_evidence":
                    "live_supervised_job/worker_assignment_pane/worker_live_pane",
                "expected_event": "worker_next_turn_on_assignment",
                "opened_ms": 1000,
            })
        };
        let set = crate::workers::supervision_replay::parse_fixtures(
            &serde_json::json!({
                "predicate": "stopped_without_verdict",
                "rule_version": 1,
                "retracted": [recorded("fp-retracted")],
                "delivered": [recorded("fp-delivered")],
            })
            .to_string(),
        )
        .unwrap();
        let report = ReplayReport::new(&candidate, &set).unwrap();
        assert_eq!(replay_exit(&report), 3);
        assert!(
            report.text().contains("not an acceptance"),
            "{}",
            report.text()
        );
        let accepted = ReplayReport::new(&candidate, &set)
            .unwrap()
            .with_hold_out(&candidate, &set)
            .unwrap();
        assert_eq!(replay_exit(&accepted), 0);
        let failing = ReplayReport::new(
            &crate::workers::supervision_replay::parse_candidate(
                &serde_json::json!({
                    "predicate": "stopped_without_verdict",
                    "version": 2,
                    "counter_evidence": [],
                    "expected_event": "worker_next_turn_on_assignment"
                })
                .to_string(),
            )
            .unwrap(),
            &set,
        )
        .unwrap();
        assert_eq!(replay_exit(&failing), 1);
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
