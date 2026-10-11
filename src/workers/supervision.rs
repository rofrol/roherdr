//! Supervision findings over the coordinators and their workers
//! (`supervise.report`, `herdr supervise --report`).
//!
//! A read-only sweep, every [`SWEEP_INTERVAL`] in the server and when a
//! coordinator's pane reports a turn or changes state, joins what the server
//! already knows (the panes' agent states and turn reports, the coordination
//! tenures, the workers, the repositories' runnable state and decision
//! ledger, free disk space) and checks named predicates against it. Each
//! predicate that holds is a finding: a positive fact and the event that did
//! not follow it ([`SupervisionPredicate`]). Quiet output or low activity is
//! never a finding; a coordinator without one is reported as "cannot
//! establish progress", not as progressing.
//!
//! The sweep is a derived view for humans: nothing waits on it, and it never
//! waits, retries, prompts, kills, restarts, clears a state or marks work
//! done. Its findings are kept append-only in the worker store
//! (`supervision_findings`, rows never deleted), deduplicated by fingerprint
//! (the predicate and its subject): while a finding holds, later sweeps only
//! move its `last_seen_ms` and evidence; once a sweep no longer sees it, it is
//! resolved for good, and the same fingerprint later opens a new finding.
//! Every change is a `supervision.finding` event in the store. A new or
//! escalated finding sends one notification, pointing at the herdr
//! repository's coordinator pane as a `herdr report` does (to the user when
//! no pane coordinates herdr), and lists one `?` entry for the user, until
//! it is acknowledged or resolves. The table is that coordinator's durable
//! inbox: `supervise.report` read from its pane marks the findings that hold
//! seen by its tenure, and an escalation clears the mark.
//!
//! Only the user (a click in the `?` list of their own client) or the herdr
//! repository's coordinator (`supervise.acknowledge` from its tenure's pane)
//! acknowledges a finding; a worker's or another agent's `worker.answer` is
//! refused with `finding_ack_refused`, since the agent that stalled could
//! otherwise silence its own finding.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::warn;

use super::coordinators::store_error;
use super::reports::is_herdr_repository;
use super::store::{EventRow, Recorded, StoreResult, Tx};
use super::{lock, notify_clients, notify_user, now_ms, Direction, PendingWorkerQuestion};
use super::{UserNotice, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    AgentStatus, CoordinatorInfo, SupervisedCoordinator, SupervisionEvidence, SupervisionFinding,
    SupervisionPredicate, SupervisionReport, SupervisionSeverity, SupervisionStatus,
    TodoRunnableState, WorkerChoiceQuestion, WorkerDecision, WorkerInfo, WorkerQuestion,
    WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionState, WorkerState,
};

/// The findings table.
pub(super) const SUPERVISION_MIGRATION: &str = r#"
-- Supervision findings (`supervise.report`): one row per predicate that
-- held for one subject, from its first sweep to the sweep that no longer saw
-- it. At most one row per fingerprint holds (open or acknowledged); a
-- resolved row is final, and the fingerprint seen again opens a new row.
-- `job_ids` and `evidence` are JSON. Times are Unix milliseconds. Rows are
-- never deleted.
CREATE TABLE supervision_findings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    fingerprint TEXT NOT NULL,
    predicate TEXT NOT NULL,
    severity TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'acknowledged', 'resolved')),
    summary TEXT NOT NULL,
    expected_event TEXT NOT NULL,
    repo TEXT,
    coordinator_id TEXT,
    pane_id TEXT,
    run_id TEXT,
    job_ids TEXT NOT NULL,
    evidence TEXT NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL,
    acknowledged_ms INTEGER,
    resolved_ms INTEGER,
    resolution TEXT
);
CREATE UNIQUE INDEX supervision_findings_holding
    ON supervision_findings (fingerprint) WHERE status != 'resolved';
CREATE TRIGGER supervision_findings_no_delete BEFORE DELETE ON supervision_findings
BEGIN SELECT RAISE(ABORT, 'supervision findings are never deleted'); END;
CREATE TRIGGER supervision_findings_resolved_final BEFORE UPDATE ON supervision_findings
WHEN OLD.status = 'resolved'
BEGIN SELECT RAISE(ABORT, 'a resolved supervision finding is final'); END;
"#;

/// Who acknowledged a finding, and the herdr coordinator's reads of it.
pub(super) const SUPERVISION_INBOX_MIGRATION: &str = r#"
-- Who acknowledged a finding: `user` (their own client) or `coordinator
-- <id>` (the herdr repository's tenure). The herdr coordinator's inbox:
-- when its tenure (`coordinator_seen_by`) last read the finding in
-- `supervise.report`; an escalation clears both.
ALTER TABLE supervision_findings ADD COLUMN acknowledged_by TEXT;
ALTER TABLE supervision_findings ADD COLUMN coordinator_seen_ms INTEGER;
ALTER TABLE supervision_findings ADD COLUMN coordinator_seen_by TEXT;
"#;

/// How often the server sweeps without an event: a reconciliation for the
/// user's view, decided with the findings' design (TODO t-7zfjbcrf); no
/// wait, retry or state depends on it.
// delay: not a wait, the decided interval of the read-only supervision sweep.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

const GIB: u64 = 1024 * 1024 * 1024;
/// The build guard's floor (`scripts/target_sweep.py`, `just guard`): below
/// it `just test` refuses to build.
const DISK_GUARD_BYTES: u64 = 15 * GIB;
/// Below this the finding is critical: one cold build of a worktree's
/// `target/` no longer fits.
const DISK_CRITICAL_BYTES: u64 = 5 * GIB;

/// What a coordinator without a finding is reported as.
pub(crate) const CANNOT_ESTABLISH_PROGRESS: &str = "cannot establish progress";

/// The request id of a finding's `?` entry: this and the finding's id.
const REQUEST_PREFIX: &str = "herdr-finding-";
/// The stand-in worker id a finding's `?` entry is listed under.
pub(crate) const WORKER_PREFIX: &str = "supervision-";
const ACKNOWLEDGE: &str = "Acknowledge";

/// What the server's app knows of one agent pane when a sweep starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneFacts {
    /// Its canonical public id.
    pub(crate) pane_id: String,
    pub(crate) agent: Option<String>,
    pub(crate) status: AgentStatus,
    /// Its last state change's sequence number, the event the status came
    /// from.
    pub(crate) state_seq: Option<u64>,
    /// Its integration reported a turn end with no background task running
    /// (Claude's `Stop`) and no turn start since (`UserPromptSubmit`).
    pub(crate) turn_closed: bool,
    /// The background tasks its last reported turn end still ran; `None`
    /// while unknown.
    pub(crate) background_tasks: Option<u32>,
    /// How many of those are herdr-job waits with a live record (its
    /// `bg_waits` metadata token, which `herdr-bg-badge` reports from
    /// herdr-job's wait edges); `None` while unknown.
    pub(crate) wait_edges: Option<u32>,
    /// It asked the user a question that is still open.
    pub(crate) asks_user: bool,
    /// Its turn ended on a usage or credit limit.
    pub(crate) limited: bool,
    /// The herdr-job jobs it owns whose tabs show them running.
    pub(crate) running_jobs: Vec<String>,
}

/// One active tenure with what the sweep found of it.
#[derive(Debug, Clone)]
struct CoordinatorFacts {
    tenure: CoordinatorInfo,
    /// Its pane, when the server has it.
    pane: Option<PaneFacts>,
    /// Its repository's runnable state, or why it could not be read.
    runnable: Result<TodoRunnableState, String>,
    /// Its workers whose process runs.
    workers: Vec<String>,
}

/// The free space of one repository's file system.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiskFacts {
    repo: String,
    free_bytes: u64,
}

/// Everything one sweep evaluates.
#[derive(Debug, Clone)]
struct Facts {
    observed_ms: u64,
    coordinators: Vec<CoordinatorFacts>,
    panes: Vec<PaneFacts>,
    disks: Vec<DiskFacts>,
}

/// A predicate that holds now, before it is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    fingerprint: String,
    predicate: SupervisionPredicate,
    severity: SupervisionSeverity,
    summary: String,
    expected_event: String,
    repo: Option<String>,
    coordinator_id: Option<String>,
    pane_id: Option<String>,
    run_id: Option<String>,
    job_ids: Vec<String>,
    evidence: SupervisionEvidence,
}

fn predicate_name(predicate: SupervisionPredicate) -> &'static str {
    match predicate {
        SupervisionPredicate::CoordinatorIdle => "coordinator_idle",
        SupervisionPredicate::WorkingAfterStop => "working_after_stop",
        SupervisionPredicate::UntrackedShell => "untracked_shell",
        SupervisionPredicate::DiskLow => "disk_low",
        SupervisionPredicate::Unknown => "unknown",
    }
}

fn parse_predicate(name: &str) -> SupervisionPredicate {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .unwrap_or(SupervisionPredicate::Unknown)
}

fn severity_name(severity: SupervisionSeverity) -> &'static str {
    match severity {
        SupervisionSeverity::Warning => "warning",
        SupervisionSeverity::Critical => "critical",
        SupervisionSeverity::Unknown => "unknown",
    }
}

fn parse_severity(name: &str) -> SupervisionSeverity {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .unwrap_or(SupervisionSeverity::Unknown)
}

/// Escalation compares ranks; an unknown severity ranks lowest.
fn severity_rank(severity: SupervisionSeverity) -> u8 {
    match severity {
        SupervisionSeverity::Unknown => 0,
        SupervisionSeverity::Warning => 1,
        SupervisionSeverity::Critical => 2,
    }
}

fn parse_status(name: &str) -> SupervisionStatus {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .unwrap_or(SupervisionStatus::Unknown)
}

/// 16 hex characters of the SHA-256 of the predicate and its subject.
fn fingerprint(predicate: SupervisionPredicate, subject: &str) -> String {
    let digest = Sha256::digest(format!("{}\n{subject}", predicate_name(predicate)));
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn id_of(number: i64) -> String {
    format!("f-{number}")
}

fn finding_number(finding_id: &str) -> Option<i64> {
    finding_id
        .strip_prefix("f-")
        .unwrap_or(finding_id)
        .parse()
        .ok()
        .filter(|number| *number > 0)
}

fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// A pane's facts as evidence lines, and its state change as an event id.
fn pane_evidence(pane: &PaneFacts, facts: &mut Vec<String>, event_ids: &mut Vec<String>) {
    facts.push(format!("pane: {}", pane.pane_id));
    if let Some(agent) = &pane.agent {
        facts.push(format!("agent: {agent}"));
    }
    facts.push(format!("agent_status: {}", status_text(pane.status)));
    facts.push(format!("turn_closed: {}", pane.turn_closed));
    facts.push(format!(
        "background_tasks: {}",
        pane.background_tasks
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    ));
    facts.push(format!(
        "wait_edges: {}",
        pane.wait_edges
            .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    ));
    if let Some(seq) = pane.state_seq {
        event_ids.push(format!("pane {} state change {seq}", pane.pane_id));
    }
}

fn tenure_evidence(tenure: &CoordinatorInfo, event_ids: &mut Vec<String>) {
    event_ids.push(format!(
        "coordinator {} started at {} (epoch {})",
        tenure.coordinator_id, tenure.started_ms, tenure.epoch
    ));
}

/// Whether the agent's turn has ended: it rests (idle or done).
fn resting(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Idle | AgentStatus::Done)
}

/// The predicates that hold for `facts`. Pure: the sweep gathers the facts
/// first, so every predicate is tested without a server.
fn evaluate(facts: &Facts) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for coordinator in &facts.coordinators {
        candidates.extend(coordinator_idle(coordinator, facts.observed_ms));
        candidates.extend(untracked_shell(coordinator, facts.observed_ms));
    }
    for pane in &facts.panes {
        candidates.extend(working_after_stop(pane, facts.observed_ms));
    }
    for disk in &facts.disks {
        candidates.extend(disk_low(disk, facts.observed_ms));
    }
    candidates
}

/// A coordinator's agent rests while its repository has runnable items
/// (nothing runs, queue mode is off), no open decision, no question of its
/// own to the user and no wait the server knows of: no running worker of
/// its tenure, no running job of its pane, no background task at its last
/// turn end.
fn coordinator_idle(coordinator: &CoordinatorFacts, observed_ms: u64) -> Option<Candidate> {
    let tenure = &coordinator.tenure;
    let pane = coordinator.pane.as_ref()?;
    let runnable = coordinator.runnable.as_ref().ok()?;
    let holds = !tenure.headless
        && resting(pane.status)
        && !pane.asks_user
        && !pane.limited
        && runnable.stalled
        && runnable.open_decisions.is_empty()
        && coordinator.workers.is_empty()
        && pane.running_jobs.is_empty()
        && pane.background_tasks.is_none_or(|count| count == 0);
    if !holds {
        return None;
    }
    let mut facts = Vec::new();
    let mut event_ids = Vec::new();
    tenure_evidence(tenure, &mut event_ids);
    pane_evidence(pane, &mut facts, &mut event_ids);
    facts.push(format!(
        "runnable_items: {}",
        runnable.runnable_items.join(", ")
    ));
    facts.push("open_decisions: none".into());
    facts.push("running_workers: none".into());
    facts.push("running_jobs: none".into());
    facts.push(format!("runnable_state: {}", runnable.reason));
    Some(Candidate {
        fingerprint: fingerprint(
            SupervisionPredicate::CoordinatorIdle,
            &tenure.coordinator_id,
        ),
        predicate: SupervisionPredicate::CoordinatorIdle,
        severity: SupervisionSeverity::Warning,
        summary: format!(
            "coordinator {} of {} is {} while {} item(s) can run, the first {}, with no open \
             decision and no registered wait",
            tenure.coordinator_id,
            tenure.repo,
            status_text(pane.status),
            runnable.runnable_items.len(),
            runnable.runnable_items.first().map_or("?", String::as_str)
        ),
        expected_event: "the coordinator starts the next item (`herdr todo run`), asks the user \
                         or registers a wait (a herdr-job job or a worker)"
            .into(),
        repo: Some(tenure.repo.clone()),
        coordinator_id: Some(tenure.coordinator_id.clone()),
        pane_id: Some(pane.pane_id.clone()),
        run_id: None,
        job_ids: Vec::new(),
        evidence: SupervisionEvidence {
            observed_ms,
            event_ids,
            facts,
        },
    })
}

/// A coordinator's turn ended with background tasks still running while
/// the server knows of no wait edge for them: more tasks than herdr-job
/// waits with a live record (a wait on another pane's job is one), no
/// running job of its pane, no running worker of its tenure, no run of its
/// repository in progress.
fn untracked_shell(coordinator: &CoordinatorFacts, observed_ms: u64) -> Option<Candidate> {
    let tenure = &coordinator.tenure;
    let pane = coordinator.pane.as_ref()?;
    let tasks = pane.background_tasks.filter(|count| *count > 0)?;
    let untracked = tasks.saturating_sub(pane.wait_edges.unwrap_or(0));
    if untracked == 0 {
        return None;
    }
    let active_run = coordinator
        .runnable
        .as_ref()
        .ok()
        .and_then(|runnable| runnable.active_run.clone());
    let holds = !tenure.headless
        && resting(pane.status)
        && pane.running_jobs.is_empty()
        && coordinator.workers.is_empty()
        && active_run.is_none();
    if !holds {
        return None;
    }
    let mut facts = Vec::new();
    let mut event_ids = Vec::new();
    tenure_evidence(tenure, &mut event_ids);
    pane_evidence(pane, &mut facts, &mut event_ids);
    facts.push("running_jobs: none".into());
    facts.push("running_workers: none".into());
    facts.push("active_run: none".into());
    Some(Candidate {
        fingerprint: fingerprint(SupervisionPredicate::UntrackedShell, &tenure.coordinator_id),
        predicate: SupervisionPredicate::UntrackedShell,
        severity: SupervisionSeverity::Warning,
        summary: format!(
            "coordinator {} of {} ended its turn with {untracked} of {tasks} background task(s) \
             running and no wait edge herdr knows of",
            tenure.coordinator_id, tenure.repo
        ),
        expected_event: "a herdr-job wait on the shell (`herdr-job watch --pid <its pid>`), or \
                         its end and the coordinator's next turn"
            .into(),
        repo: Some(tenure.repo.clone()),
        coordinator_id: Some(tenure.coordinator_id.clone()),
        pane_id: Some(pane.pane_id.clone()),
        run_id: None,
        job_ids: Vec::new(),
        evidence: SupervisionEvidence {
            observed_ms,
            event_ids,
            facts,
        },
    })
}

/// A pane's agent shows `working` after its integration reported the turn's
/// end with no background task, and no turn started since.
fn working_after_stop(pane: &PaneFacts, observed_ms: u64) -> Option<Candidate> {
    if !(pane.turn_closed && pane.status == AgentStatus::Working) {
        return None;
    }
    let mut facts = Vec::new();
    let mut event_ids = Vec::new();
    pane_evidence(pane, &mut facts, &mut event_ids);
    Some(Candidate {
        fingerprint: fingerprint(SupervisionPredicate::WorkingAfterStop, &pane.pane_id),
        predicate: SupervisionPredicate::WorkingAfterStop,
        severity: SupervisionSeverity::Warning,
        summary: format!(
            "pane {} shows working after its agent reported the turn's end (Stop) with no new \
             turn start (UserPromptSubmit)",
            pane.pane_id
        ),
        expected_event: "a turn start (`pane.report_turn` started) or the pane's state turning \
                         idle"
            .into(),
        repo: None,
        coordinator_id: None,
        pane_id: Some(pane.pane_id.clone()),
        run_id: None,
        job_ids: pane.running_jobs.clone(),
        evidence: SupervisionEvidence {
            observed_ms,
            event_ids,
            facts,
        },
    })
}

/// A coordinator's repository sits on a file system with less free space
/// than the build guard needs.
fn disk_low(disk: &DiskFacts, observed_ms: u64) -> Option<Candidate> {
    if disk.free_bytes >= DISK_GUARD_BYTES {
        return None;
    }
    let severity = if disk.free_bytes < DISK_CRITICAL_BYTES {
        SupervisionSeverity::Critical
    } else {
        SupervisionSeverity::Warning
    };
    Some(Candidate {
        fingerprint: fingerprint(SupervisionPredicate::DiskLow, &disk.repo),
        predicate: SupervisionPredicate::DiskLow,
        severity,
        summary: format!(
            "{} free under {}, below the build guard's {}",
            gib(disk.free_bytes),
            disk.repo,
            gib(DISK_GUARD_BYTES)
        ),
        expected_event: format!(
            "free space back at {} or more (`just sweep` in the repository)",
            gib(DISK_GUARD_BYTES)
        ),
        repo: Some(disk.repo.clone()),
        coordinator_id: None,
        pane_id: None,
        run_id: None,
        job_ids: Vec::new(),
        evidence: SupervisionEvidence {
            observed_ms,
            event_ids: Vec::new(),
            facts: vec![
                format!("free_bytes: {}", disk.free_bytes),
                format!("guard_bytes: {DISK_GUARD_BYTES}"),
            ],
        },
    })
}

const COLUMNS: &str = "id, fingerprint, predicate, severity, status, summary, expected_event, \
                       repo, coordinator_id, pane_id, run_id, job_ids, evidence, first_seen_ms, \
                       last_seen_ms, acknowledged_ms, resolved_ms, resolution, acknowledged_by, \
                       coordinator_seen_ms, coordinator_seen_by";

fn finding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SupervisionFinding> {
    let job_ids: String = row.get(11)?;
    let evidence: String = row.get(12)?;
    Ok(SupervisionFinding {
        finding_id: id_of(row.get(0)?),
        fingerprint: row.get(1)?,
        predicate: parse_predicate(&row.get::<_, String>(2)?),
        severity: parse_severity(&row.get::<_, String>(3)?),
        status: parse_status(&row.get::<_, String>(4)?),
        summary: row.get(5)?,
        expected_event: row.get(6)?,
        repo: row.get(7)?,
        coordinator_id: row.get(8)?,
        pane_id: row.get(9)?,
        run_id: row.get(10)?,
        job_ids: serde_json::from_str(&job_ids).unwrap_or_default(),
        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
        first_seen_ms: row.get::<_, i64>(13)? as u64,
        last_seen_ms: row.get::<_, i64>(14)? as u64,
        acknowledged_ms: row.get::<_, Option<i64>>(15)?.map(|ms| ms as u64),
        resolved_ms: row.get::<_, Option<i64>>(16)?.map(|ms| ms as u64),
        resolution: row.get(17)?,
        acknowledged_by: row.get(18)?,
        coordinator_seen_ms: row.get::<_, Option<i64>>(19)?.map(|ms| ms as u64),
        coordinator_seen_by: row.get(20)?,
    })
}

fn finding_by_number(conn: &Connection, number: i64) -> StoreResult<Option<SupervisionFinding>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM supervision_findings WHERE id = ?1"),
        [number],
        finding_from_row,
    )
    .optional()
}

/// The findings that hold (open or acknowledged), or all of them; the
/// latest seen first.
fn list_findings(conn: &Connection, all: bool) -> StoreResult<Vec<SupervisionFinding>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM supervision_findings
         WHERE ?1 OR status != 'resolved'
         ORDER BY last_seen_ms DESC, id DESC"
    ))?;
    let rows = statement.query_map([all], finding_from_row)?;
    rows.collect()
}

fn open_findings(conn: &Connection) -> StoreResult<Vec<SupervisionFinding>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM supervision_findings WHERE status = 'open' ORDER BY id"
    ))?;
    let rows = statement.query_map([], finding_from_row)?;
    rows.collect()
}

/// Appends a `supervision.finding` event for one change of finding `number`.
fn finding_event(
    tx: &Tx<'_>,
    finding: &SupervisionFinding,
    change: &str,
    at_ms: u64,
) -> StoreResult<i64> {
    let event = json!({
        "type": "supervision.finding",
        "change": change,
        "finding_id": finding.finding_id,
        "fingerprint": finding.fingerprint,
        "predicate": finding.predicate,
        "severity": finding.severity,
        "status": finding.status,
        "summary": finding.summary,
        "repo": finding.repo,
        "coordinator_id": finding.coordinator_id,
        "pane_id": finding.pane_id,
        "resolution": finding.resolution,
        "acknowledged_by": finding.acknowledged_by,
        "coordinator_seen_by": finding.coordinator_seen_by,
    });
    tx.event(&EventRow {
        worker_id: &format!("supervision:{}", finding.finding_id),
        direction: Direction::Herdr,
        record: &Recorded::Event(&event),
        ts_ms: at_ms,
    })
}

/// What recording one sweep changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Changes {
    opened: Vec<SupervisionFinding>,
    escalated: Vec<SupervisionFinding>,
    resolved: Vec<SupervisionFinding>,
}

/// Records one sweep's candidates: a new fingerprint opens a finding, one
/// that holds already moves its `last_seen_ms` and evidence (and is
/// escalated, open again, when its severity rose), and every holding
/// finding the sweep no longer saw is resolved.
fn record_sweep(tx: &Tx<'_>, candidates: &[Candidate], at_ms: u64) -> StoreResult<Changes> {
    let conn = tx.connection();
    let mut changes = Changes::default();
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.fingerprint.clone()) {
            continue;
        }
        let job_ids = serde_json::to_string(&candidate.job_ids).unwrap_or_else(|_| "[]".into());
        let evidence = serde_json::to_string(&candidate.evidence).unwrap_or_else(|_| "{}".into());
        let holding: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, severity FROM supervision_findings
                 WHERE fingerprint = ?1 AND status != 'resolved'",
                [&candidate.fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match holding {
            None => {
                conn.execute(
                    "INSERT INTO supervision_findings (fingerprint, predicate, severity, status,
                         summary, expected_event, repo, coordinator_id, pane_id, run_id, job_ids,
                         evidence, first_seen_ms, last_seen_ms)
                     VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                    params![
                        candidate.fingerprint,
                        predicate_name(candidate.predicate),
                        severity_name(candidate.severity),
                        candidate.summary,
                        candidate.expected_event,
                        candidate.repo,
                        candidate.coordinator_id,
                        candidate.pane_id,
                        candidate.run_id,
                        job_ids,
                        evidence,
                        at_ms as i64,
                    ],
                )?;
                let number = conn.last_insert_rowid();
                if let Some(finding) = finding_by_number(conn, number)? {
                    finding_event(tx, &finding, "opened", at_ms)?;
                    changes.opened.push(finding);
                }
            }
            Some((number, severity)) => {
                let escalated =
                    severity_rank(candidate.severity) > severity_rank(parse_severity(&severity));
                conn.execute(
                    "UPDATE supervision_findings SET last_seen_ms = ?2, evidence = ?3,
                         summary = ?4, job_ids = ?5, run_id = ?6,
                         severity = CASE WHEN ?7 THEN ?8 ELSE severity END,
                         status = CASE WHEN ?7 THEN 'open' ELSE status END,
                         acknowledged_ms = CASE WHEN ?7 THEN NULL ELSE acknowledged_ms END,
                         acknowledged_by = CASE WHEN ?7 THEN NULL ELSE acknowledged_by END,
                         coordinator_seen_ms =
                             CASE WHEN ?7 THEN NULL ELSE coordinator_seen_ms END,
                         coordinator_seen_by =
                             CASE WHEN ?7 THEN NULL ELSE coordinator_seen_by END
                     WHERE id = ?1",
                    params![
                        number,
                        at_ms as i64,
                        evidence,
                        candidate.summary,
                        job_ids,
                        candidate.run_id,
                        escalated,
                        severity_name(candidate.severity),
                    ],
                )?;
                if escalated {
                    if let Some(finding) = finding_by_number(conn, number)? {
                        finding_event(tx, &finding, "escalated", at_ms)?;
                        changes.escalated.push(finding);
                    }
                }
            }
        }
    }
    let holding: Vec<(i64, String)> = {
        let mut statement = conn.prepare(
            "SELECT id, fingerprint FROM supervision_findings WHERE status != 'resolved'",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<StoreResult<_>>()?
    };
    for (number, fingerprint) in holding {
        if seen.contains(&fingerprint) {
            continue;
        }
        conn.execute(
            "UPDATE supervision_findings SET status = 'resolved', resolved_ms = ?2,
                 resolution = ?3
             WHERE id = ?1",
            params![
                number,
                at_ms as i64,
                format!("the sweep at {at_ms} no longer saw its predicate holding")
            ],
        )?;
        if let Some(finding) = finding_by_number(conn, number)? {
            finding_event(tx, &finding, "resolved", at_ms)?;
            changes.resolved.push(finding);
        }
    }
    Ok(changes)
}

/// Who acknowledges a finding: the only two who may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Acknowledger {
    /// The user, from their own client's `?` list.
    User,
    /// The herdr repository's coordination tenure, by its id.
    Coordinator(String),
}

impl Acknowledger {
    fn text(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::Coordinator(id) => format!("coordinator {id}"),
        }
    }
}

/// The refusal of an acknowledgement from anyone else.
fn ack_refused(finding: &str, why: &str) -> WorkerError {
    WorkerError::FindingAckRefused(format!(
        "{finding} is a supervision finding: only the user acknowledges it, with a click in the \
         `?` list of their own client, or the herdr repository's coordinator, with `herdr \
         supervise --ack {finding}` from the pane that holds its tenure; a worker or another \
         agent never does ({why})"
    ))
}

/// Acknowledges an open finding; `None` when there is no such finding,
/// `Some(false)` when it was not open.
fn acknowledge(
    tx: &Tx<'_>,
    number: i64,
    by: &Acknowledger,
    at_ms: u64,
) -> StoreResult<Option<bool>> {
    let conn = tx.connection();
    let Some(finding) = finding_by_number(conn, number)? else {
        return Ok(None);
    };
    if finding.status != SupervisionStatus::Open {
        return Ok(Some(false));
    }
    conn.execute(
        "UPDATE supervision_findings SET status = 'acknowledged', acknowledged_ms = ?2,
             acknowledged_by = ?3
         WHERE id = ?1 AND status = 'open'",
        params![number, at_ms as i64, by.text()],
    )?;
    if let Some(finding) = finding_by_number(conn, number)? {
        finding_event(tx, &finding, "acknowledged", at_ms)?;
    }
    Ok(Some(true))
}

/// Marks the findings that hold seen by the herdr coordinator's tenure
/// `coordinator_id`, those it had not seen yet, with an event each.
fn mark_seen(tx: &Tx<'_>, coordinator_id: &str, at_ms: u64) -> StoreResult<Vec<String>> {
    let conn = tx.connection();
    let unseen: Vec<i64> = {
        let mut statement = conn.prepare(
            "SELECT id FROM supervision_findings
             WHERE status != 'resolved' AND coordinator_seen_by IS NOT ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([coordinator_id], |row| row.get(0))?;
        rows.collect::<StoreResult<_>>()?
    };
    let mut marked = Vec::new();
    for number in unseen {
        conn.execute(
            "UPDATE supervision_findings SET coordinator_seen_ms = ?2, coordinator_seen_by = ?3
             WHERE id = ?1",
            params![number, at_ms as i64, coordinator_id],
        )?;
        if let Some(finding) = finding_by_number(conn, number)? {
            finding_event(tx, &finding, "seen", at_ms)?;
            marked.push(finding.finding_id);
        }
    }
    Ok(marked)
}

/// The notification a new or escalated finding sends: to the herdr
/// repository's coordinator pane, as a `herdr report` does, else to the
/// user.
fn notice(
    finding: &SupervisionFinding,
    escalated: bool,
    herdr: Option<&CoordinatorInfo>,
) -> UserNotice {
    let severity = match finding.severity {
        SupervisionSeverity::Critical => " (critical)",
        _ => "",
    };
    let change = if escalated { " escalated" } else { "" };
    let pane_id = herdr
        .filter(|coordinator| !coordinator.headless)
        .and_then(|coordinator| coordinator.pane_id.clone());
    let nobody = if pane_id.is_none() {
        " (no herdr coordinator)"
    } else {
        ""
    };
    UserNotice {
        title: format!(
            "Supervision finding {}{change}{severity}{nobody}",
            finding.finding_id
        ),
        body: format!("{}; expected: {}", finding.summary, finding.expected_event),
        pane_id,
    }
}

/// What the latest sweep saw, for `supervise.report`.
#[derive(Debug, Clone, Default)]
struct LastSweep {
    swept_ms: Option<u64>,
    trigger: Option<String>,
    coordinators: Vec<SupervisedCoordinator>,
}

static LAST_SWEEP: Mutex<Option<LastSweep>> = Mutex::new(None);
/// The open findings, which the `?` list shows; refreshed after each change
/// so a snapshot never reads the store.
static OPEN_FINDINGS: Mutex<Vec<SupervisionFinding>> = Mutex::new(Vec::new());
/// The findings that hold (open or acknowledged), which the clients'
/// Supervision tab shows; refreshed with [`OPEN_FINDINGS`].
static HOLDING_FINDINGS: Mutex<Vec<SupervisionFinding>> = Mutex::new(Vec::new());
/// The panes active tenures were bound to at the latest sweep: their turn
/// reports and state changes start a sweep.
static COORDINATOR_PANES: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
/// The event that asked for a sweep, until the server starts it.
static REQUESTED: Mutex<Option<String>> = Mutex::new(None);
/// Why the latest sweep failed, until one succeeds.
static SWEEP_ERROR: Mutex<Option<String>> = Mutex::new(None);
/// A sweep runs.
static SWEEPING: AtomicBool = AtomicBool::new(false);

/// Asks for a sweep when `pane_id` was bound to a coordination tenure at
/// the latest sweep: `what` happened there (`turn`, `state`). The server
/// starts it at its next loop pass ([`take_sweep_request`]).
pub(crate) fn request_sweep_for_pane(pane_id: &str, what: &str) {
    if !lock(&COORDINATOR_PANES).contains(pane_id) {
        return;
    }
    let mut requested = lock(&REQUESTED);
    if requested.is_none() {
        *requested = Some(format!("pane {pane_id} {what}"));
    }
}

/// The event that asked for a sweep since the last call, if any; left in
/// place while a sweep runs, so the next one starts after it.
pub(crate) fn take_sweep_request() -> Option<String> {
    if SWEEPING.load(Ordering::Acquire) {
        return None;
    }
    lock(&REQUESTED).take()
}

/// Starts a sweep in a thread of its own (it reads the repositories' git
/// state) with the app's pane facts; `false` when one runs already or no
/// supervisor holds the tenures.
pub(crate) fn start_sweep(trigger: String, panes: Vec<PaneFacts>) -> bool {
    let Some(supervisor) = super::coordinators() else {
        return false;
    };
    if SWEEPING.swap(true, Ordering::AcqRel) {
        return false;
    }
    let supervisor = supervisor.clone();
    let spawned = crate::thread_spawn::spawn_named("herdr-supervision", move || {
        let failed = supervisor.supervision_sweep(&trigger, panes).err();
        if let Some(error) = &failed {
            warn!(%error, "the supervision sweep failed");
        }
        let failed = failed.map(|error| error.to_string());
        let changed = {
            let mut last = lock(&SWEEP_ERROR);
            let changed = *last != failed;
            *last = failed;
            changed
        };
        if changed {
            notify_clients();
        }
        SWEEPING.store(false, Ordering::Release);
    });
    if let Err(error) = spawned {
        warn!(%error, "cannot start the supervision sweep");
        SWEEPING.store(false, Ordering::Release);
        return false;
    }
    true
}

/// What the clients' Supervision tab shows: the latest sweep and the
/// findings that hold, from memory (a snapshot never reads the store); none
/// on a server without a supervisor.
pub(crate) fn client_shell_supervision() -> Option<crate::protocol::ClientShellSupervision> {
    super::coordinators()?;
    let last = lock(&LAST_SWEEP).clone().unwrap_or_default();
    let findings = lock(&HOLDING_FINDINGS)
        .iter()
        .map(client_shell_finding)
        .collect();
    Some(crate::protocol::ClientShellSupervision {
        swept_ms: last.swept_ms,
        trigger: last.trigger,
        sweep_error: lock(&SWEEP_ERROR).clone(),
        sweep_interval_ms: SWEEP_INTERVAL.as_millis().try_into().unwrap_or(u64::MAX),
        coordinators: last
            .coordinators
            .into_iter()
            .map(
                |coordinator| crate::protocol::ClientShellSupervisedCoordinator {
                    coordinator_id: coordinator.coordinator_id,
                    repo: coordinator.repo,
                    pane_id: coordinator.pane_id,
                    agent_status: coordinator
                        .agent_status
                        .map(|status| status_text(status).to_owned()),
                    workers: coordinator.workers,
                    active_run: coordinator.active_run,
                    findings: coordinator.findings,
                    note: coordinator.note,
                },
            )
            .collect(),
        findings,
    })
}

fn client_shell_finding(
    finding: &SupervisionFinding,
) -> crate::protocol::ClientShellSupervisionFinding {
    let status = match finding.status {
        SupervisionStatus::Open => "open",
        SupervisionStatus::Acknowledged => "acknowledged",
        SupervisionStatus::Resolved => "resolved",
        SupervisionStatus::Unknown => "unknown",
    };
    crate::protocol::ClientShellSupervisionFinding {
        finding_id: finding.finding_id.clone(),
        predicate: predicate_name(finding.predicate).to_owned(),
        severity: severity_name(finding.severity).to_owned(),
        status: status.to_owned(),
        summary: finding.summary.clone(),
        expected_event: finding.expected_event.clone(),
        repo: finding.repo.clone(),
        coordinator_id: finding.coordinator_id.clone(),
        pane_id: finding.pane_id.clone(),
        run_id: finding.run_id.clone(),
        event_ids: finding.evidence.event_ids.clone(),
        facts: finding.evidence.facts.clone(),
        first_seen_ms: finding.first_seen_ms,
        last_seen_ms: finding.last_seen_ms,
        acknowledged_by: finding.acknowledged_by.clone(),
    }
}

fn worker_id_of(finding_id: &str) -> String {
    format!("{WORKER_PREFIX}{finding_id}")
}

fn request_id_of(finding_id: &str) -> String {
    format!("{REQUEST_PREFIX}{finding_id}")
}

/// Whether `request_id` names a finding's `?` entry.
pub(crate) fn is_finding_question(request_id: &str) -> bool {
    request_id.starts_with(REQUEST_PREFIX)
}

fn question_of(finding: &SupervisionFinding) -> WorkerQuestion {
    WorkerQuestion {
        request_id: request_id_of(&finding.finding_id),
        kind: WorkerQuestionKind::Choice,
        tool_name: "herdr supervise".into(),
        text: format!("{}: {}", finding.finding_id, finding.summary),
        reason: Some(format!("expected: {}", finding.expected_event)),
        questions: vec![WorkerChoiceQuestion {
            question: finding.summary.clone(),
            header: Some(format!("Finding {}", finding.finding_id)),
            options: vec![ACKNOWLEDGE.into()],
            multi_select: false,
        }],
        since_ms: finding.first_seen_ms,
        state: WorkerQuestionState::Pending,
        escalated: Some(
            "a supervision finding herdr's sweep saw; herdr acts on none: Acknowledge (yours, or \
             the herdr coordinator's) takes it off this list, and it resolves on its own once \
             the sweep no longer sees it"
                .into(),
        ),
    }
}

/// The open findings as the `?` list shows a worker's questions.
pub(super) fn pending_questions() -> Vec<PendingWorkerQuestion> {
    lock(&OPEN_FINDINGS)
        .iter()
        .map(|finding| PendingWorkerQuestion {
            worker_id: worker_id_of(&finding.finding_id),
            cwd: finding.repo.clone().unwrap_or_default(),
            question: question_of(finding),
            quiet: false,
            owner_seen_ms: None,
        })
        .collect()
}

fn find_open(worker_id: &str, request_id: &str) -> Result<SupervisionFinding, WorkerError> {
    lock(&OPEN_FINDINGS)
        .iter()
        .find(|finding| {
            worker_id_of(&finding.finding_id) == worker_id
                && request_id_of(&finding.finding_id) == request_id
        })
        .cloned()
        .ok_or_else(|| {
            WorkerError::QuestionGone(format!(
                "question {request_id} of {worker_id} is no longer pending: the finding was \
                 acknowledged or resolved meanwhile"
            ))
        })
}

fn finding_reply(worker_id: &str, repo: Option<&str>) -> Result<WorkerInfo, WorkerError> {
    serde_json::from_value(json!({
        "worker_id": worker_id,
        "state": WorkerState::Exited,
        "cwd": repo.unwrap_or_default(),
        "name": format!("supervision finding {}", worker_id.trim_start_matches(WORKER_PREFIX)),
        "turns": 0,
        "journal_path": "",
    }))
    .map_err(|error| WorkerError::Invalid(error.to_string()))
}

impl WorkerSupervisor {
    fn supervision_store(&self) -> Result<&super::store::Store, WorkerError> {
        self.shared
            .store
            .as_ref()
            .map_err(|error| WorkerError::Io(std::io::Error::other(error.clone())))
    }

    /// One sweep: gathers the store's facts beside the app's `panes`,
    /// evaluates the predicates, records the findings and sends the
    /// notifications of new and escalated ones.
    pub(crate) fn supervision_sweep(
        &self,
        trigger: &str,
        panes: Vec<PaneFacts>,
    ) -> Result<(), WorkerError> {
        let observed_ms = now_ms();
        let tenures = self.coordinator_status(None)?;
        let by_pane: BTreeMap<&str, &PaneFacts> = panes
            .iter()
            .map(|pane| (pane.pane_id.as_str(), pane))
            .collect();
        let mut coordinators = Vec::new();
        let mut repos = BTreeSet::new();
        for tenure in tenures {
            repos.insert(tenure.repo.clone());
            let pane = tenure
                .pane_id
                .as_deref()
                .and_then(|pane| by_pane.get(pane))
                .map(|pane| (*pane).clone());
            let runnable = self
                .todo_runnable_state(&tenure.repo)
                .map_err(|error| error.to_string());
            let workers = self.running_workers_of(&tenure);
            coordinators.push(CoordinatorFacts {
                tenure,
                pane,
                runnable,
                workers,
            });
        }
        let disks = repos
            .into_iter()
            .filter_map(|repo| {
                let free_bytes =
                    crate::platform::available_disk_bytes(std::path::Path::new(&repo))?;
                Some(DiskFacts { repo, free_bytes })
            })
            .collect();
        let facts = Facts {
            observed_ms,
            coordinators,
            panes,
            disks,
        };
        let candidates = evaluate(&facts);
        let changes = self
            .supervision_store()?
            .transaction(|tx| record_sweep(tx, &candidates, observed_ms))
            .map_err(store_error)?;
        *lock(&COORDINATOR_PANES) = facts
            .coordinators
            .iter()
            .filter_map(|coordinator| coordinator.tenure.pane_id.clone())
            .collect();
        let holding = self.supervision_findings(false)?;
        *lock(&LAST_SWEEP) = Some(LastSweep {
            swept_ms: Some(observed_ms),
            trigger: Some(trigger.to_owned()),
            coordinators: facts
                .coordinators
                .iter()
                .map(|coordinator| supervised(coordinator, &holding))
                .collect(),
        });
        self.refresh_open_findings();
        // The Supervision tab shows the sweep's time and what it saw.
        notify_clients();
        if changes.opened.is_empty() && changes.escalated.is_empty() {
            return Ok(());
        }
        let herdr = facts
            .coordinators
            .iter()
            .map(|coordinator| &coordinator.tenure)
            .find(|tenure| is_herdr_repository(&tenure.repo));
        for finding in &changes.opened {
            notify_user(notice(finding, false, herdr));
        }
        for finding in &changes.escalated {
            notify_user(notice(finding, true, herdr));
        }
        Ok(())
    }

    /// The workers of `tenure` (or of its pane, for one started before
    /// tenures owned workers) whose process runs.
    fn running_workers_of(&self, tenure: &CoordinatorInfo) -> Vec<String> {
        let registry = lock(&self.shared.registry);
        registry
            .workers
            .values()
            .filter(|entry| !entry.status.is_gone())
            .filter(|entry| {
                entry.status.owner_coordinator.as_deref() == Some(tenure.coordinator_id.as_str())
                    || (entry.status.owner_coordinator.is_none()
                        && tenure.pane_id.is_some()
                        && entry.status.owner_pane == tenure.pane_id)
            })
            .map(|entry| entry.status.worker_id.clone())
            .collect()
    }

    fn supervision_findings(&self, all: bool) -> Result<Vec<SupervisionFinding>, WorkerError> {
        self.supervision_store()?
            .read(|conn| list_findings(conn, all))
            .map_err(store_error)
    }

    /// Reloads the open findings the `?` list shows and the findings that
    /// hold, which the Supervision tab shows, and tells the clients when
    /// they changed.
    fn refresh_open_findings(&self) {
        let read = match self.supervision_store() {
            Ok(store) => store.read(|conn| Ok((open_findings(conn)?, list_findings(conn, false)?))),
            Err(_) => return,
        };
        let (open, holding) = match read {
            Ok(read) => read,
            Err(error) => {
                warn!(%error, "cannot read the open supervision findings");
                return;
            }
        };
        let open_changed = {
            let mut listed = lock(&OPEN_FINDINGS);
            let changed = *listed != open;
            *listed = open;
            changed
        };
        let holding_changed = {
            let mut listed = lock(&HOLDING_FINDINGS);
            let changed = *listed != holding;
            *listed = holding;
            changed
        };
        if open_changed || holding_changed {
            notify_clients();
        }
    }

    /// `supervise.report`: the findings that hold, or all, and what the
    /// latest sweep saw of each coordinator. Read from `reader_pane` (its
    /// public id) while it holds the herdr repository's tenure, it then
    /// marks the findings that hold seen by that tenure; the reply shows the
    /// marks as they were before.
    pub(crate) fn supervision_report(
        &self,
        all: bool,
        reader_pane: Option<&str>,
    ) -> Result<SupervisionReport, WorkerError> {
        let findings = self.supervision_findings(all)?;
        let herdr = self.herdr_coordinator();
        let reader = reader_pane
            .and_then(|pane| self.coordinator_of_pane(pane))
            .filter(|tenure| {
                herdr
                    .as_ref()
                    .is_some_and(|herdr| herdr.coordinator_id == tenure.coordinator_id)
            });
        if let Some(reader) = reader {
            self.supervision_store()?
                .transaction(|tx| mark_seen(tx, &reader.coordinator_id, now_ms()))
                .map_err(store_error)?;
        }
        let last = lock(&LAST_SWEEP).clone().unwrap_or_default();
        Ok(SupervisionReport {
            swept_ms: last.swept_ms,
            trigger: last.trigger,
            herdr_coordinator_id: herdr.map(|herdr| herdr.coordinator_id),
            coordinators: last.coordinators,
            findings,
        })
    }

    /// Acknowledges finding `finding_id` as `by`: it leaves the `?` list
    /// and stays in the report until it resolves. Callers check that `by`
    /// may ([`Self::supervision_acknowledge_from_pane`], the user's client).
    fn supervision_acknowledge(
        &self,
        finding_id: &str,
        by: &Acknowledger,
    ) -> Result<SupervisionFinding, WorkerError> {
        let number = finding_number(finding_id).ok_or_else(|| {
            WorkerError::NotFound(format!("{finding_id} is not a supervision finding id"))
        })?;
        let store = self.supervision_store()?;
        let outcome = store
            .transaction(|tx| acknowledge(tx, number, by, now_ms()))
            .map_err(store_error)?;
        self.refresh_open_findings();
        let missing = || WorkerError::NotFound(format!("no supervision finding {}", id_of(number)));
        match outcome {
            None => Err(missing()),
            Some(_) => store
                .read(|conn| finding_by_number(conn, number))
                .map_err(store_error)?
                .ok_or_else(missing),
        }
    }

    /// `supervise.acknowledge`: only from `pane_id` (its public id) while it
    /// holds the herdr repository's coordination tenure. Acknowledging a
    /// finding that is not open changes nothing and answers it as it is.
    pub(crate) fn supervision_acknowledge_from_pane(
        &self,
        finding_id: &str,
        pane_id: Option<&str>,
    ) -> Result<SupervisionFinding, WorkerError> {
        let Some(pane) = pane_id else {
            return Err(ack_refused(finding_id, "HERDR_PANE_ID is not set"));
        };
        let tenure = self.coordinator_of_pane(pane).ok_or_else(|| {
            ack_refused(
                finding_id,
                &format!("pane {pane} holds no coordination tenure"),
            )
        })?;
        if !is_herdr_repository(&tenure.repo) {
            return Err(ack_refused(
                finding_id,
                &format!(
                    "pane {pane} coordinates {} ({})",
                    tenure.repo, tenure.coordinator_id
                ),
            ));
        }
        self.supervision_acknowledge(
            finding_id,
            &Acknowledger::Coordinator(tenure.coordinator_id),
        )
    }

    /// The finding's `?` entry as the answer dialog shows it.
    pub(super) fn finding_question_detail(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerQuestionDetail, WorkerError> {
        let finding = find_open(worker_id, request_id)?;
        let mut lines = vec![
            finding.summary.clone(),
            String::new(),
            format!("expected: {}", finding.expected_event),
            format!("first seen: {}", finding.first_seen_ms),
            format!("last seen: {}", finding.last_seen_ms),
        ];
        lines.extend(
            finding
                .evidence
                .event_ids
                .iter()
                .map(|id| format!("event: {id}")),
        );
        lines.extend(finding.evidence.facts.iter().cloned());
        Ok(WorkerQuestionDetail {
            worker_id: worker_id_of(&finding.finding_id),
            name: format!("supervision finding {}", finding.finding_id),
            cwd: finding.repo.clone().unwrap_or_default(),
            state: WorkerState::Exited,
            question: question_of(&finding),
            input_text: lines.join("\n"),
            owner_pane_id: None,
            owner_coordinator_id: None,
            quiet: false,
        })
    }

    /// The user's answer to a finding's `?` entry from their own client,
    /// whatever it says (Acknowledge, or a denial from the dialog):
    /// acknowledges the finding.
    pub(crate) fn answer_finding_question_from_client(
        &self,
        worker_id: &str,
        request_id: &str,
        _decision: Option<WorkerDecision>,
    ) -> Result<WorkerInfo, WorkerError> {
        let finding = find_open(worker_id, request_id)?;
        self.supervision_acknowledge(&finding.finding_id, &Acknowledger::User)?;
        finding_reply(worker_id, finding.repo.as_deref())
    }

    /// An answer to a finding's `?` entry from anywhere but the user's
    /// client (`worker.answer`, `worker.deny_and_stop` on the API): refused,
    /// naming who may acknowledge it.
    pub(super) fn refuse_finding_answer(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> Result<WorkerInfo, WorkerError> {
        let finding = find_open(worker_id, request_id)?;
        Err(ack_refused(
            &finding.finding_id,
            "an answer through the API is not the user's click",
        ))
    }

    /// What escalating a finding's `?` entry replies: it is the user's
    /// already.
    pub(super) fn finding_reply_of(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        finding_reply(worker_id, None)
    }
}

/// What the report shows of one coordinator: its findings, else that
/// progress cannot be established.
fn supervised(
    coordinator: &CoordinatorFacts,
    holding: &[SupervisionFinding],
) -> SupervisedCoordinator {
    let tenure = &coordinator.tenure;
    let findings: Vec<String> = holding
        .iter()
        .filter(|finding| {
            finding.coordinator_id.as_deref() == Some(tenure.coordinator_id.as_str())
                || (finding.coordinator_id.is_none()
                    && finding.pane_id.is_some()
                    && finding.pane_id == tenure.pane_id)
                || (finding.predicate == SupervisionPredicate::DiskLow
                    && finding.repo.as_deref() == Some(tenure.repo.as_str()))
        })
        .map(|finding| finding.finding_id.clone())
        .collect();
    SupervisedCoordinator {
        coordinator_id: tenure.coordinator_id.clone(),
        repo: tenure.repo.clone(),
        pane_id: tenure.pane_id.clone(),
        agent_status: coordinator.pane.as_ref().map(|pane| pane.status),
        workers: coordinator.workers.clone(),
        active_run: coordinator
            .runnable
            .as_ref()
            .ok()
            .and_then(|runnable| runnable.active_run.clone()),
        note: findings
            .is_empty()
            .then(|| CANNOT_ESTABLISH_PROGRESS.to_owned()),
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(pane_id: &str, status: AgentStatus) -> PaneFacts {
        PaneFacts {
            pane_id: pane_id.into(),
            agent: Some("claude".into()),
            status,
            state_seq: Some(41),
            turn_closed: false,
            background_tasks: Some(0),
            wait_edges: Some(0),
            asks_user: false,
            limited: false,
            running_jobs: Vec::new(),
        }
    }

    fn tenure(id: &str, pane_id: &str) -> CoordinatorInfo {
        CoordinatorInfo {
            coordinator_id: id.into(),
            repo: "/repo".into(),
            item: None,
            started_ms: 1,
            ended_ms: None,
            end_reason: None,
            epoch: 1,
            pane_id: Some(pane_id.into()),
            session_id: None,
            headless: false,
            worker_id: None,
        }
    }

    fn stalled() -> TodoRunnableState {
        TodoRunnableState {
            repo: "/repo".into(),
            queue_mode: None,
            pause_reason: None,
            active_run: None,
            active_coordinator: None,
            runnable_items: vec!["t-abcdefgh".into()],
            open_decisions: Vec::new(),
            stalled: true,
            reason: "nothing runs".into(),
        }
    }

    fn coordinator(status: AgentStatus) -> CoordinatorFacts {
        CoordinatorFacts {
            tenure: tenure("c-aaaaaaaa", "w1:p1"),
            pane: Some(pane("w1:p1", status)),
            runnable: Ok(stalled()),
            workers: Vec::new(),
        }
    }

    fn facts(coordinators: Vec<CoordinatorFacts>, panes: Vec<PaneFacts>) -> Facts {
        Facts {
            observed_ms: 1000,
            coordinators,
            panes,
            disks: Vec::new(),
        }
    }

    fn predicates(facts: &Facts) -> Vec<SupervisionPredicate> {
        evaluate(facts)
            .into_iter()
            .map(|candidate| candidate.predicate)
            .collect()
    }

    #[test]
    fn an_idle_coordinator_with_runnable_items_and_nothing_to_wait_on_is_a_finding() {
        let found = evaluate(&facts(vec![coordinator(AgentStatus::Idle)], Vec::new()));
        assert_eq!(found.len(), 1, "{found:?}");
        let finding = &found[0];
        assert_eq!(finding.predicate, SupervisionPredicate::CoordinatorIdle);
        assert_eq!(finding.coordinator_id.as_deref(), Some("c-aaaaaaaa"));
        assert!(
            finding.summary.contains("t-abcdefgh"),
            "{}",
            finding.summary
        );
        assert!(finding
            .evidence
            .event_ids
            .iter()
            .any(|id| id == "pane w1:p1 state change 41"));
        // Done (idle, not seen yet) rests too.
        assert_eq!(
            predicates(&facts(vec![coordinator(AgentStatus::Done)], Vec::new())),
            vec![SupervisionPredicate::CoordinatorIdle]
        );
    }

    #[test]
    fn a_coordinator_is_not_idle_while_it_works_waits_or_asks() {
        let quiet = |coordinator: CoordinatorFacts| {
            assert!(
                predicates(&facts(vec![coordinator.clone()], Vec::new())).is_empty(),
                "{coordinator:?}"
            );
        };
        quiet(coordinator(AgentStatus::Working));
        quiet(coordinator(AgentStatus::Blocked));
        let mut asks = coordinator(AgentStatus::Idle);
        asks.pane.as_mut().unwrap().asks_user = true;
        quiet(asks);
        let mut limited = coordinator(AgentStatus::Idle);
        limited.pane.as_mut().unwrap().limited = true;
        quiet(limited);
        let mut decision = coordinator(AgentStatus::Idle);
        decision.runnable.as_mut().unwrap().open_decisions = vec!["d-abcdefgh".into()];
        quiet(decision);
        let mut not_stalled = coordinator(AgentStatus::Idle);
        not_stalled.runnable.as_mut().unwrap().stalled = false;
        quiet(not_stalled);
        let mut unreadable = coordinator(AgentStatus::Idle);
        unreadable.runnable = Err("no TODO.md on master".into());
        quiet(unreadable);
        let mut worker = coordinator(AgentStatus::Idle);
        worker.workers = vec!["w-1".into()];
        quiet(worker);
        let mut job = coordinator(AgentStatus::Idle);
        job.pane.as_mut().unwrap().running_jobs = vec!["20261011-1".into()];
        quiet(job);
        let mut headless = coordinator(AgentStatus::Idle);
        headless.tenure.headless = true;
        quiet(headless);
        let mut gone = coordinator(AgentStatus::Idle);
        gone.pane = None;
        quiet(gone);
    }

    #[test]
    fn a_turn_end_with_background_tasks_and_no_wait_edge_is_an_untracked_shell() {
        let mut shell = coordinator(AgentStatus::Idle);
        shell.pane.as_mut().unwrap().background_tasks = Some(2);
        let found = evaluate(&facts(vec![shell.clone()], Vec::new()));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].predicate, SupervisionPredicate::UntrackedShell);
        assert!(found[0].summary.contains("2 background task"));

        // A running job of its pane, a worker of its tenure or a run of its
        // repository is a wait edge herdr knows of.
        let mut job = shell.clone();
        job.pane.as_mut().unwrap().running_jobs = vec!["20261011-1".into()];
        assert!(predicates(&facts(vec![job], Vec::new())).is_empty());
        let mut worker = shell.clone();
        worker.workers = vec!["w-1".into()];
        assert!(predicates(&facts(vec![worker], Vec::new())).is_empty());
        let mut run = shell.clone();
        run.runnable.as_mut().unwrap().active_run = Some("r-abcdefgh".into());
        run.runnable.as_mut().unwrap().stalled = false;
        assert!(predicates(&facts(vec![run], Vec::new())).is_empty());
        // While it works its turn has not ended.
        let mut working = shell;
        working.pane.as_mut().unwrap().status = AgentStatus::Working;
        assert!(predicates(&facts(vec![working], Vec::new())).is_empty());
    }

    #[test]
    fn working_after_a_closed_turn_is_a_finding_for_any_pane() {
        let mut stuck = pane("w2:p3", AgentStatus::Working);
        stuck.turn_closed = true;
        let found = evaluate(&facts(Vec::new(), vec![stuck.clone()]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].predicate, SupervisionPredicate::WorkingAfterStop);
        assert_eq!(found[0].pane_id.as_deref(), Some("w2:p3"));

        // Working in an open turn, or idle after a closed one, is not.
        let mut open_turn = stuck.clone();
        open_turn.turn_closed = false;
        assert!(predicates(&facts(Vec::new(), vec![open_turn])).is_empty());
        let mut idle = stuck;
        idle.status = AgentStatus::Idle;
        assert!(predicates(&facts(Vec::new(), vec![idle])).is_empty());
    }

    #[test]
    fn free_space_below_the_build_guard_is_a_finding_and_critical_far_below() {
        let disk = |free_bytes| {
            let mut facts = facts(Vec::new(), Vec::new());
            facts.disks = vec![DiskFacts {
                repo: "/repo".into(),
                free_bytes,
            }];
            evaluate(&facts)
        };
        assert!(disk(DISK_GUARD_BYTES).is_empty());
        let low = disk(DISK_GUARD_BYTES - 1);
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].predicate, SupervisionPredicate::DiskLow);
        assert_eq!(low[0].severity, SupervisionSeverity::Warning);
        assert_eq!(
            disk(DISK_CRITICAL_BYTES - 1)[0].severity,
            SupervisionSeverity::Critical
        );
        assert_eq!(low[0].fingerprint, disk(1)[0].fingerprint);
    }

    #[test]
    fn quiet_output_is_no_finding_only_cannot_establish_progress() {
        // A coordinator that works, however long and however quiet, has no
        // finding; the report says progress cannot be established.
        let working = coordinator(AgentStatus::Working);
        assert!(evaluate(&facts(vec![working.clone()], Vec::new())).is_empty());
        let shown = supervised(&working, &[]);
        assert_eq!(shown.note.as_deref(), Some(CANNOT_ESTABLISH_PROGRESS));
        assert!(shown.findings.is_empty());
    }

    fn supervisor(name: &str) -> (WorkerSupervisor, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("herdr-supervision-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (
            WorkerSupervisor::open(root.join("workers"), "claude".into()),
            root,
        )
    }

    fn record(supervisor: &WorkerSupervisor, candidates: &[Candidate], at_ms: u64) -> Changes {
        supervisor
            .supervision_store()
            .unwrap()
            .transaction(|tx| record_sweep(tx, candidates, at_ms))
            .unwrap()
    }

    #[test]
    fn a_finding_is_recorded_once_per_fingerprint_while_it_holds() {
        let (supervisor, root) = supervisor("dedupe");
        let idle = evaluate(&facts(vec![coordinator(AgentStatus::Idle)], Vec::new()));
        let first = record(&supervisor, &idle, 1000);
        assert_eq!(first.opened.len(), 1);
        let id = first.opened[0].finding_id.clone();

        // The same fingerprint again only moves last_seen.
        let again = record(&supervisor, &idle, 2000);
        assert_eq!(again, Changes::default());
        let holding = supervisor.supervision_findings(false).unwrap();
        assert_eq!(holding.len(), 1);
        assert_eq!(holding[0].finding_id, id);
        assert_eq!(holding[0].first_seen_ms, 1000);
        assert_eq!(holding[0].last_seen_ms, 2000);
        assert_eq!(holding[0].status, SupervisionStatus::Open);

        // Acknowledged, it stays acknowledged while it holds.
        supervisor
            .supervision_acknowledge(&id, &Acknowledger::User)
            .unwrap();
        assert_eq!(record(&supervisor, &idle, 3000), Changes::default());
        let holding = supervisor.supervision_findings(false).unwrap();
        assert_eq!(holding[0].status, SupervisionStatus::Acknowledged);

        // No longer seen: resolved for good; seen again: a new finding.
        let gone = record(&supervisor, &[], 4000);
        assert_eq!(gone.resolved.len(), 1);
        assert_eq!(gone.resolved[0].status, SupervisionStatus::Resolved);
        assert!(gone.resolved[0].resolution.is_some());
        assert!(supervisor.supervision_findings(false).unwrap().is_empty());
        let back = record(&supervisor, &idle, 5000);
        assert_eq!(back.opened.len(), 1);
        assert_ne!(back.opened[0].finding_id, id);
        assert_eq!(supervisor.supervision_findings(true).unwrap().len(), 2);

        // Every change is a `supervision.finding` event.
        let changes: Vec<String> = supervisor
            .supervision_store()
            .unwrap()
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT body FROM events WHERE type = 'supervision.finding' ORDER BY seq",
                )?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<StoreResult<Vec<String>>>()
            })
            .unwrap()
            .iter()
            .map(|body| {
                serde_json::from_str::<serde_json::Value>(body).unwrap()["change"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(changes, ["opened", "acknowledged", "resolved", "opened"]);

        // Rows are never deleted, and a resolved one never changes.
        let store = supervisor.supervision_store().unwrap();
        let conn = store.connection();
        assert!(conn
            .execute("DELETE FROM supervision_findings", [])
            .is_err());
        assert!(conn
            .execute(
                "UPDATE supervision_findings SET status = 'open' WHERE status = 'resolved'",
                []
            )
            .is_err());
        drop(conn);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_rising_severity_escalates_and_reopens_an_acknowledged_finding() {
        let (supervisor, root) = supervisor("escalate");
        let disk = |free_bytes| {
            let mut facts = facts(Vec::new(), Vec::new());
            facts.disks = vec![DiskFacts {
                repo: "/repo".into(),
                free_bytes,
            }];
            evaluate(&facts)
        };
        let opened = record(&supervisor, &disk(10 * GIB), 1000);
        let id = opened.opened[0].finding_id.clone();
        supervisor
            .supervision_acknowledge(&id, &Acknowledger::User)
            .unwrap();
        // Lower again but still a warning: nothing new.
        assert_eq!(
            record(&supervisor, &disk(9 * GIB), 2000),
            Changes::default()
        );
        let escalated = record(&supervisor, &disk(GIB), 3000);
        assert_eq!(escalated.escalated.len(), 1);
        let finding = &escalated.escalated[0];
        assert_eq!(finding.finding_id, id);
        assert_eq!(finding.severity, SupervisionSeverity::Critical);
        assert_eq!(finding.status, SupervisionStatus::Open);
        assert_eq!(finding.acknowledged_by, None);
        // Back to a warning does not lower it.
        assert_eq!(
            record(&supervisor, &disk(9 * GIB), 4000),
            Changes::default()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_open_finding_is_one_question_in_the_list_until_acknowledged() {
        let (supervisor, root) = supervisor("question");
        let idle = evaluate(&facts(vec![coordinator(AgentStatus::Idle)], Vec::new()));
        record(&supervisor, &idle, 1000);
        supervisor.refresh_open_findings();
        let listed: Vec<PendingWorkerQuestion> = pending_questions()
            .into_iter()
            .filter(|pending| pending.cwd == "/repo")
            .collect();
        assert_eq!(listed.len(), 1);
        let entry = &listed[0];
        assert!(is_finding_question(&entry.question.request_id));
        assert!(!entry.quiet);
        let detail = supervisor
            .finding_question_detail(&entry.worker_id, &entry.question.request_id)
            .unwrap();
        assert!(
            detail.input_text.contains("expected:"),
            "{}",
            detail.input_text
        );

        supervisor
            .answer_finding_question_from_client(&entry.worker_id, &entry.question.request_id, None)
            .unwrap();
        assert!(pending_questions()
            .iter()
            .all(|pending| pending.question.request_id != entry.question.request_id));
        let gone = supervisor
            .answer_finding_question_from_client(&entry.worker_id, &entry.question.request_id, None)
            .unwrap_err();
        assert!(matches!(gone, WorkerError::QuestionGone(_)), "{gone:?}");
        let holding = supervisor.supervision_findings(false).unwrap();
        assert_eq!(holding[0].status, SupervisionStatus::Acknowledged);
        assert_eq!(holding[0].acknowledged_by.as_deref(), Some("user"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_sweep_records_what_it_sees_notifies_once_and_reports_each_coordinator() {
        let (supervisor, root) = supervisor("sweep");
        // A repository that is not one: its runnable state and its disk
        // cannot be read, so only the pane's own facts can hold.
        let repo = root.join("not-a-repo").display().to_string();
        supervisor.coordinator_start(&repo, "w1:p1", None).unwrap();
        let mut stuck = pane("w1:p1", AgentStatus::Working);
        stuck.turn_closed = true;
        let _ = super::super::take_user_notices();

        supervisor
            .supervision_sweep("timer", vec![stuck.clone()])
            .unwrap();
        let notices = super::super::take_user_notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].title.starts_with("Supervision finding f-1"));
        let report = supervisor.supervision_report(false, None).unwrap();
        assert_eq!(report.trigger.as_deref(), Some("timer"));
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].predicate,
            SupervisionPredicate::WorkingAfterStop
        );
        assert_eq!(report.coordinators.len(), 1);
        assert_eq!(report.coordinators[0].findings, vec!["f-1".to_owned()]);
        assert_eq!(report.coordinators[0].note, None);
        // Its turn reports now start sweeps.
        request_sweep_for_pane("w1:p1", "turn");
        assert_eq!(take_sweep_request().as_deref(), Some("pane w1:p1 turn"));

        // The same facts again notify nothing; once the pane rests the
        // finding resolves and the coordinator cannot be judged.
        supervisor
            .supervision_sweep("pane w1:p1 turn", vec![stuck.clone()])
            .unwrap();
        assert!(super::super::take_user_notices().is_empty());
        stuck.status = AgentStatus::Idle;
        supervisor.supervision_sweep("timer", vec![stuck]).unwrap();
        assert!(super::super::take_user_notices().is_empty());
        let report = supervisor.supervision_report(false, None).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(
            report.coordinators[0].note.as_deref(),
            Some(CANNOT_ESTABLISH_PROGRESS)
        );
        assert_eq!(
            supervisor
                .supervision_report(true, None)
                .unwrap()
                .findings
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_clients_see_the_latest_sweep_and_the_findings_that_hold() {
        let (supervisor, root) = supervisor("client-shell");
        let repo = root.join("not-a-repo").display().to_string();
        supervisor.coordinator_start(&repo, "w1:p1", None).unwrap();
        crate::workers::set_test_coordinators(supervisor);
        let supervisor = crate::workers::coordinators().unwrap();
        let mut stuck = pane("w1:p1", AgentStatus::Working);
        stuck.turn_closed = true;
        supervisor
            .supervision_sweep("timer", vec![stuck.clone()])
            .unwrap();
        let shown = client_shell_supervision().expect("a server with a supervisor sends it");
        assert!(shown.swept_ms.is_some());
        assert_eq!(shown.trigger.as_deref(), Some("timer"));
        assert_eq!(shown.sweep_error, None);
        assert_eq!(
            u128::from(shown.sweep_interval_ms),
            SWEEP_INTERVAL.as_millis()
        );
        assert_eq!(shown.coordinators.len(), 1);
        assert_eq!(shown.coordinators[0].pane_id.as_deref(), Some("w1:p1"));
        assert_eq!(
            shown.coordinators[0].agent_status.as_deref(),
            Some("working")
        );
        assert_eq!(shown.findings.len(), 1);
        let finding = &shown.findings[0];
        assert_eq!(finding.predicate, "working_after_stop");
        assert_eq!(finding.status, "open");
        assert!(!finding.facts.is_empty(), "{finding:?}");

        // An acknowledged finding still holds and still shows.
        supervisor
            .supervision_acknowledge(&finding.finding_id, &Acknowledger::User)
            .unwrap();
        let shown = client_shell_supervision().unwrap();
        assert_eq!(shown.findings[0].status, "acknowledged");
        assert_eq!(shown.findings[0].acknowledged_by.as_deref(), Some("user"));

        // A resolved one does not.
        stuck.status = AgentStatus::Idle;
        supervisor.supervision_sweep("timer", vec![stuck]).unwrap();
        assert!(client_shell_supervision().unwrap().findings.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_wait_on_another_panes_job_is_a_wait_edge_not_an_untracked_shell() {
        // Two background tasks, both herdr-job waits with a live record
        // (`bg_waits`): one on a job in a worker's tab, which the pane's own
        // running jobs do not show.
        let mut waiting = coordinator(AgentStatus::Idle);
        waiting.pane.as_mut().unwrap().background_tasks = Some(2);
        waiting.pane.as_mut().unwrap().wait_edges = Some(2);
        assert!(predicates(&facts(vec![waiting.clone()], Vec::new())).is_empty());

        // One of them is a plain shell: one untracked task.
        let mut one = waiting.clone();
        one.pane.as_mut().unwrap().wait_edges = Some(1);
        let found = evaluate(&facts(vec![one], Vec::new()));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].predicate, SupervisionPredicate::UntrackedShell);
        assert!(
            found[0].summary.contains("1 of 2 background task"),
            "{}",
            found[0].summary
        );
        assert!(found[0]
            .evidence
            .facts
            .contains(&"wait_edges: 1".to_owned()));

        // Unknown edges (no `bg_waits` reported) count as none.
        let mut unknown = waiting;
        unknown.pane.as_mut().unwrap().wait_edges = None;
        assert_eq!(
            predicates(&facts(vec![unknown], Vec::new())),
            vec![SupervisionPredicate::UntrackedShell]
        );
    }

    /// A directory whose `Cargo.toml` names the package `herdr`.
    fn herdr_checkout(root: &std::path::Path) -> String {
        let dir = root.join("herdr");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"herdr\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        dir.display().to_string()
    }

    #[test]
    fn only_the_user_or_the_herdr_coordinator_acknowledges_a_finding() {
        use crate::api::schema::{WorkerAnswerParams, WorkerDenyAndStopParams};
        let (supervisor, root) = supervisor("authority");
        let idle = evaluate(&facts(vec![coordinator(AgentStatus::Idle)], Vec::new()));
        record(&supervisor, &idle, 1000);
        supervisor.refresh_open_findings();
        let entry = pending_questions()
            .into_iter()
            .find(|pending| pending.cwd == "/repo")
            .unwrap();
        let id = entry.worker_id.trim_start_matches(WORKER_PREFIX).to_owned();
        let answer = WorkerAnswerParams {
            worker_id: entry.worker_id.clone(),
            request_id: Some(entry.question.request_id.clone()),
            decision: None,
            answers: vec![ACKNOWLEDGE.into()],
            message: None,
            command_id: None,
        };

        // A worker's or another agent's answer through the API: refused,
        // naming who may.
        let refused = supervisor.answer(&answer).unwrap_err();
        assert_eq!(refused.code(), "finding_ack_refused", "{refused}");
        assert!(refused.to_string().contains("only the user"), "{refused}");
        assert!(
            refused.to_string().contains("herdr supervise --ack"),
            "{refused}"
        );
        let stop = WorkerDenyAndStopParams {
            worker_id: entry.worker_id.clone(),
            request_id: entry.question.request_id.clone(),
            message: None,
        };
        assert_eq!(
            supervisor.deny_and_stop(&stop).unwrap_err().code(),
            "finding_ack_refused"
        );

        // `supervise.acknowledge` without a pane, from a pane without a
        // tenure and from another repository's coordinator: refused.
        for pane in [None, Some("w9:p9")] {
            assert_eq!(
                supervisor
                    .supervision_acknowledge_from_pane(&id, pane)
                    .unwrap_err()
                    .code(),
                "finding_ack_refused",
                "{pane:?}"
            );
        }
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        supervisor
            .coordinator_start(&other.display().to_string(), "w2:p1", None)
            .unwrap();
        let refused = supervisor
            .supervision_acknowledge_from_pane(&id, Some("w2:p1"))
            .unwrap_err();
        assert_eq!(refused.code(), "finding_ack_refused");
        assert!(
            refused.to_string().contains(&other.display().to_string()),
            "{refused}"
        );
        assert_eq!(
            supervisor.supervision_findings(false).unwrap()[0].status,
            SupervisionStatus::Open
        );

        // The herdr repository's coordinator acknowledges it.
        let herdr = herdr_checkout(&root);
        let tenure = supervisor.coordinator_start(&herdr, "w3:p1", None).unwrap();
        let acknowledged = supervisor
            .supervision_acknowledge_from_pane(&id, Some("w3:p1"))
            .unwrap();
        assert_eq!(acknowledged.status, SupervisionStatus::Acknowledged);
        assert_eq!(
            acknowledged.acknowledged_by,
            Some(format!("coordinator {}", tenure.coordinator_id))
        );
        assert!(pending_questions()
            .iter()
            .all(|pending| pending.question.request_id != entry.question.request_id));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_user_acknowledges_from_their_own_client() {
        use crate::api::schema::WorkerDenyAndStopParams;
        let (supervisor, root) = supervisor("client");
        let idle = evaluate(&facts(vec![coordinator(AgentStatus::Idle)], Vec::new()));
        record(&supervisor, &idle, 1000);
        supervisor.refresh_open_findings();
        let entry = pending_questions()
            .into_iter()
            .find(|pending| pending.cwd == "/repo")
            .unwrap();
        // The dialog's Stop on a finding: no worker to stop, acknowledged.
        supervisor
            .deny_and_stop_from_client(&WorkerDenyAndStopParams {
                worker_id: entry.worker_id.clone(),
                request_id: entry.question.request_id.clone(),
                message: None,
            })
            .unwrap();
        let holding = supervisor.supervision_findings(false).unwrap();
        assert_eq!(holding[0].status, SupervisionStatus::Acknowledged);
        assert_eq!(holding[0].acknowledged_by.as_deref(), Some("user"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn findings_reach_the_herdr_coordinators_inbox_and_its_reads_mark_them_seen() {
        let (supervisor, root) = supervisor("inbox");
        let herdr = herdr_checkout(&root);
        let tenure = supervisor.coordinator_start(&herdr, "w1:p1", None).unwrap();
        let mut stuck = pane("w4:p2", AgentStatus::Working);
        stuck.turn_closed = true;
        let _ = super::super::take_user_notices();

        supervisor
            .supervision_sweep("timer", vec![stuck.clone()])
            .unwrap();
        // The herdr checkout's disk may be low too: look at the pane's.
        let finding = supervisor
            .supervision_findings(false)
            .unwrap()
            .into_iter()
            .find(|finding| finding.predicate == SupervisionPredicate::WorkingAfterStop)
            .unwrap();
        let notices = super::super::take_user_notices();
        let notice = notices
            .iter()
            .find(|notice| {
                notice
                    .title
                    .starts_with(&format!("Supervision finding {} ", finding.finding_id))
                    || notice.title == format!("Supervision finding {}", finding.finding_id)
            })
            .unwrap_or_else(|| panic!("{notices:?}"));
        // Pointed at the herdr coordinator's pane, as a report is.
        assert_eq!(notice.pane_id.as_deref(), Some("w1:p1"));
        assert!(!notice.title.contains("no herdr coordinator"), "{notice:?}");

        // Another pane's read marks nothing.
        let report = supervisor.supervision_report(false, Some("w4:p2")).unwrap();
        assert_eq!(
            report.herdr_coordinator_id.as_deref(),
            Some(tenure.coordinator_id.as_str())
        );
        let unseen = |report: &SupervisionReport| {
            report
                .findings
                .iter()
                .find(|shown| shown.finding_id == finding.finding_id)
                .unwrap()
                .coordinator_seen_by
                .clone()
        };
        assert_eq!(unseen(&report), None);
        // The herdr coordinator's read shows it unseen, then marks it.
        let first = supervisor.supervision_report(false, Some("w1:p1")).unwrap();
        assert_eq!(unseen(&first), None);
        let second = supervisor.supervision_report(false, None).unwrap();
        assert_eq!(unseen(&second), Some(tenure.coordinator_id.clone()));
        let seen = second
            .findings
            .iter()
            .find(|shown| shown.finding_id == finding.finding_id)
            .unwrap();
        assert!(seen.coordinator_seen_ms.is_some());
        // A seen event was recorded, once: a later read marks nothing new.
        supervisor.supervision_report(false, Some("w1:p1")).unwrap();
        let seen_events: i64 = supervisor
            .supervision_store()
            .unwrap()
            .read(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM events WHERE type = 'supervision.finding'
                     AND worker_id = ?1 AND body LIKE '%\"change\":\"seen\"%'",
                    [format!("supervision:{}", finding.finding_id)],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(seen_events, 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_escalation_clears_the_coordinators_seen_mark() {
        let (supervisor, root) = supervisor("escalate-seen");
        let disk = |free_bytes| {
            let mut facts = facts(Vec::new(), Vec::new());
            facts.disks = vec![DiskFacts {
                repo: "/repo".into(),
                free_bytes,
            }];
            evaluate(&facts)
        };
        let id = record(&supervisor, &disk(10 * GIB), 1000).opened[0]
            .finding_id
            .clone();
        supervisor
            .supervision_store()
            .unwrap()
            .transaction(|tx| mark_seen(tx, "c-herdr", 1500))
            .unwrap();
        assert_eq!(
            supervisor.supervision_findings(false).unwrap()[0]
                .coordinator_seen_by
                .as_deref(),
            Some("c-herdr")
        );
        let escalated = record(&supervisor, &disk(GIB), 2000);
        assert_eq!(escalated.escalated[0].finding_id, id);
        assert_eq!(escalated.escalated[0].coordinator_seen_by, None);
        assert_eq!(escalated.escalated[0].coordinator_seen_ms, None);
        // A notice without a herdr coordinator goes to the user.
        let notice = notice(&escalated.escalated[0], true, None);
        assert_eq!(notice.pane_id, None);
        assert!(
            notice.title.ends_with("(no herdr coordinator)"),
            "{notice:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_wire_name_parses_back() {
        for predicate in [
            SupervisionPredicate::CoordinatorIdle,
            SupervisionPredicate::WorkingAfterStop,
            SupervisionPredicate::UntrackedShell,
            SupervisionPredicate::DiskLow,
        ] {
            assert_eq!(
                serde_json::to_value(predicate).unwrap(),
                serde_json::Value::String(predicate_name(predicate).into())
            );
            assert_eq!(parse_predicate(predicate_name(predicate)), predicate);
        }
        for severity in [SupervisionSeverity::Warning, SupervisionSeverity::Critical] {
            assert_eq!(parse_severity(severity_name(severity)), severity);
        }
        for (name, status) in [
            ("open", SupervisionStatus::Open),
            ("acknowledged", SupervisionStatus::Acknowledged),
            ("resolved", SupervisionStatus::Resolved),
        ] {
            assert_eq!(parse_status(name), status);
            assert_eq!(serde_json::to_value(status).unwrap(), name);
        }
    }
}
