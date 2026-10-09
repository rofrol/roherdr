//! `herdr history`: the life of each TODO item as herdr's server recorded
//! it (`history.list`, `history.item`, `history.reconcile`), printed as a
//! timeline or, with `--json`, as the server's reply.

use crate::api::schema::{
    CoordinatorOverride, HistoryEvent, HistoryEventKind, HistoryItem, HistoryItemParams,
    HistoryItemSummary, HistoryListParams, HistoryOverridesParams, HistoryReconcile,
    HistoryReconcileParams, Method, Request, TodoRunStatus, WorkerVerdict,
};

use super::worker::take_string_option;

const USAGE: &str = "usage:
  herdr history [--repo DIR] [--json]
      The TODO items herdr recorded (of DIR's repository with --repo), the
      most recent first, each with its last record: claimed (a run started on
      it), noted, closed, aborted or blocked.
  herdr history --item <item-id> [--repo DIR] [--json]
      The item's timeline: its claims with the item's text then, each run and
      attempt with its verdict, the coordinator's decision and the commit it
      landed, the notes, the close with its decision, and the follow-up items
      (in TODO.md after the close, not at the claim).
  herdr history reconcile [--repo DIR] [--json]
      Compares DIR's (default: the current directory's) TODO.md with the
      records: claims no note, close, abort or block followed, and items seen
      at a claim that left TODO.md without a closed record. It only reports;
      it changes no record. Run it when a coordinator starts.
  herdr history overrides [--repo DIR] [--json]
      The exceptions to a coordinator tab's command allowlist, oldest first:
      each command that carried `# herdr-override: <reason>` and ran, with
      its time, reason, pane and the coordinator's tenure and item.
The records live in herdr's worker store on this machine, outside the
repository; times are in local time.";

enum Command {
    List(HistoryListParams),
    Item(HistoryItemParams),
    Reconcile(HistoryReconcileParams),
    Overrides(HistoryOverridesParams),
}

pub(super) fn run_history_command(args: &[String]) -> std::io::Result<i32> {
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
    let method = match command {
        Command::List(params) => Method::HistoryList(params),
        Command::Item(params) => Method::HistoryItem(params),
        Command::Reconcile(params) => Method::HistoryReconcile(params),
        Command::Overrides(params) => Method::HistoryOverrides(params),
    };
    let response = super::send_request(&Request {
        id: "cli:history".into(),
        method,
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    let result = &response["result"];
    let printed = match result["type"].as_str() {
        Some("history_list") => serde_json::from_value(result["items"].clone())
            .map(|items: Vec<HistoryItemSummary>| print_list(&items)),
        Some("history_item") => serde_json::from_value(result["item"].clone())
            .map(|item: HistoryItem| print_item(&item)),
        Some("history_reconcile") => serde_json::from_value(result["reconcile"].clone())
            .map(|reconcile: HistoryReconcile| print_reconcile(&reconcile)),
        Some("history_overrides") => serde_json::from_value(result["overrides"].clone())
            .map(|overrides: Vec<CoordinatorOverride>| print_overrides(&overrides)),
        _ => return super::print_response(&response),
    };
    match printed {
        Ok(()) => Ok(0),
        // A reply this client cannot read as a timeline is printed as it is.
        Err(_) => super::print_response(&response),
    }
}

fn parse(args: &[String]) -> Result<Option<(Command, bool)>, String> {
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
    let (repo, rest) = take_string_option(&rest, "--repo")?;
    let (item, rest) = take_string_option(&rest, "--item")?;
    let absolute = |dir: String| {
        std::path::absolute(&dir)
            .map(|dir| dir.display().to_string())
            .map_err(|error| format!("--repo {dir}: {error}"))
    };
    let repo = repo.map(absolute).transpose()?;
    let command = match (rest.as_slice(), item) {
        ([], None) => Command::List(HistoryListParams { repo }),
        ([], Some(item)) => Command::Item(HistoryItemParams { item, repo }),
        ([word], None) if word == "reconcile" => Command::Reconcile(HistoryReconcileParams {
            repo: match repo {
                Some(repo) => repo,
                None => absolute(".".to_owned())?,
            },
        }),
        ([word], Some(_)) if word == "reconcile" => return Err("reconcile takes no --item".into()),
        ([word], None) if word == "overrides" => {
            Command::Overrides(HistoryOverridesParams { repo })
        }
        ([word], Some(_)) if word == "overrides" => return Err("overrides takes no --item".into()),
        (other, _) => return Err(format!("unexpected arguments: {}", other.join(" "))),
    };
    Ok(Some((command, json)))
}

/// Unix milliseconds as `YYYY-MM-DD HH:MM` in local time, `offset` seconds
/// east of UTC.
fn when(ms: u64, offset: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(((ms / 1000) as i64).saturating_add(offset)) {
        Ok(at) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            at.year(),
            at.month() as u8,
            at.day(),
            at.hour(),
            at.minute()
        ),
        Err(_) => ms.to_string(),
    }
}

fn kind_name(kind: HistoryEventKind) -> &'static str {
    match kind {
        HistoryEventKind::Claimed => "claimed",
        HistoryEventKind::Noted => "noted",
        HistoryEventKind::Closed => "closed",
        HistoryEventKind::Aborted => "aborted",
        HistoryEventKind::Blocked => "blocked",
        HistoryEventKind::Unknown => "unknown",
    }
}

fn status_name(status: TodoRunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn verdict_name(verdict: WorkerVerdict) -> String {
    serde_json::to_value(verdict)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// `text`'s lines, each indented by `indent`.
fn indented(text: &str, indent: &str) -> String {
    text.trim_end()
        .lines()
        .map(|line| format!("{indent}{line}\n"))
        .collect()
}

fn print_list(items: &[HistoryItemSummary]) {
    print!(
        "{}",
        list_text(items, crate::usage::local_utc_offset_secs())
    );
}

fn list_text(items: &[HistoryItemSummary], offset: i64) -> String {
    if items.is_empty() {
        return "no recorded items\n".into();
    }
    items
        .iter()
        .map(|summary| {
            format!(
                "{}  {:<8} {}  {}  ({})\n",
                summary.item,
                kind_name(summary.last.kind),
                when(summary.last.ts_ms, offset),
                summary.title.as_deref().unwrap_or("-"),
                summary.repo
            )
        })
        .collect()
}

fn print_overrides(overrides: &[CoordinatorOverride]) {
    print!(
        "{}",
        overrides_text(overrides, crate::usage::local_utc_offset_secs())
    );
}

fn overrides_text(overrides: &[CoordinatorOverride], offset: i64) -> String {
    if overrides.is_empty() {
        return "no recorded overrides\n".into();
    }
    overrides
        .iter()
        .map(|record| {
            let mut out = format!(
                "{}  {}  pane {}",
                when(record.ts_ms, offset),
                record.reason,
                record.pane_id
            );
            if let Some(coordinator) = &record.coordinator_id {
                out.push_str(&format!("  {coordinator}"));
            }
            if let Some(item) = &record.item {
                out.push_str(&format!("  {item}"));
            }
            if let Some(repo) = &record.repo {
                out.push_str(&format!("  ({repo})"));
            }
            out.push('\n');
            out.push_str(&indented(
                &format!("{}: {}", record.tool, record.command),
                "    ",
            ));
            out
        })
        .collect()
}

fn print_item(item: &HistoryItem) {
    print!("{}", item_text(item, crate::usage::local_utc_offset_secs()));
}

fn event_text(event: &HistoryEvent, offset: i64) -> String {
    let mut out = format!("{}  {}", when(event.ts_ms, offset), kind_name(event.kind));
    if let Some(run_id) = &event.run_id {
        out.push_str(&format!("  run {run_id}"));
        if let Some(attempt) = event.attempt {
            out.push_str(&format!(" attempt {attempt}"));
        }
    }
    out.push('\n');
    match event.kind {
        HistoryEventKind::Claimed => {
            if let Some(text) = &event.item_text {
                out.push_str(&indented(text, "    | "));
            }
        }
        HistoryEventKind::Closed => {
            if let Some(text) = &event.text {
                out.push_str("    decision:\n");
                out.push_str(&indented(text, "    | "));
            }
            if let Some(text) = &event.item_text {
                out.push_str("    the item's last text:\n");
                out.push_str(&indented(text, "    | "));
            }
            if !event.follow_ups.is_empty() {
                out.push_str(&format!(
                    "    follow-ups: {}\n",
                    event.follow_ups.join(", ")
                ));
            }
        }
        _ => {
            if let Some(text) = &event.text {
                out.push_str(&indented(text, "    | "));
            }
        }
    }
    out
}

fn item_text(item: &HistoryItem, offset: i64) -> String {
    let mut out = match &item.title {
        Some(title) => format!("{}  {title}\n\n", item.item),
        None => format!("{}\n\n", item.item),
    };
    for event in &item.events {
        out.push_str(&event_text(event, offset));
    }
    if !item.runs.is_empty() {
        out.push_str("\nruns:\n");
    }
    for run in &item.runs {
        out.push_str(&format!(
            "  {}  {}  started {}",
            run.run_id,
            status_name(run.status),
            when(run.created_ms, offset)
        ));
        if let Some(commit) = &run.todo_commit {
            out.push_str(&format!("  todo commit {}", short(commit)));
        }
        out.push('\n');
        for attempt in &run.attempts {
            out.push_str(&format!("    attempt {}", attempt.attempt));
            if let Some(worker) = &attempt.worker_id {
                out.push_str(&format!("  worker {worker}"));
            }
            if let Some(verdict) = attempt.verdict {
                out.push_str(&format!("  {}", verdict_name(verdict)));
            }
            if let Some(decision) = &attempt.decision {
                out.push_str(&format!("  {decision}"));
            }
            if let Some(commit) = &attempt.commit {
                out.push_str(&format!("  commit {}", short(commit)));
            }
            if let Some(landed) = &attempt.landed_sha {
                out.push_str(&format!("  landed {}", short(landed)));
            }
            out.push('\n');
        }
    }
    out
}

fn print_reconcile(reconcile: &HistoryReconcile) {
    print!(
        "{}",
        reconcile_text(reconcile, crate::usage::local_utc_offset_secs())
    );
}

fn reconcile_text(reconcile: &HistoryReconcile, offset: i64) -> String {
    let mut out = format!("{}\n", reconcile.repo);
    if reconcile.open_claims.is_empty() && reconcile.deleted_without_close.is_empty() {
        out.push_str("every claim has an end and every item that left TODO.md was closed\n");
        return out;
    }
    if !reconcile.open_claims.is_empty() {
        out.push_str("claims without an end:\n");
        for claim in &reconcile.open_claims {
            let status = match claim.run_status {
                Some(TodoRunStatus::Running | TodoRunStatus::Waiting) => "in progress".to_owned(),
                Some(status) => format!("run {}", status_name(status)),
                None => "run unknown".to_owned(),
            };
            out.push_str(&format!(
                "  {}  run {}  claimed {}  ({status})\n",
                claim.item,
                claim.run_id,
                when(claim.claimed_ms, offset)
            ));
        }
    }
    if !reconcile.deleted_without_close.is_empty() {
        out.push_str("items gone from TODO.md without a closed record:\n");
        for deleted in &reconcile.deleted_without_close {
            out.push_str(&format!(
                "  {}  {}",
                deleted.item,
                deleted.title.as_deref().unwrap_or("-")
            ));
            if let Some(last) = &deleted.last {
                out.push_str(&format!(
                    "  (last: {} {})",
                    kind_name(last.kind),
                    when(last.ts_ms, offset)
                ));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{HistoryAttempt, HistoryOpenClaim, HistoryRun};

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn the_command_picks_the_list_the_item_or_the_reconcile() {
        let Ok(Some((Command::List(list), false))) = parse(&args(&[])) else {
            panic!("a bare history lists");
        };
        assert_eq!(list.repo, None);
        let Ok(Some((Command::Item(item), true))) =
            parse(&args(&["--item", "t-abcd2345", "--json", "--repo", "/r"]))
        else {
            panic!("--item shows the item");
        };
        assert_eq!(
            (item.item.as_str(), item.repo.as_deref()),
            ("t-abcd2345", Some("/r"))
        );
        let Ok(Some((Command::Reconcile(reconcile), false))) = parse(&args(&["reconcile"])) else {
            panic!("reconcile reconciles");
        };
        assert!(std::path::Path::new(&reconcile.repo).is_absolute());
        assert!(parse(&args(&["reconcile", "--item", "t-abcd2345"])).is_err());
        assert!(parse(&args(&["other"])).is_err());
        assert!(matches!(parse(&args(&["--help"])), Ok(None)));
    }

    fn event(kind: HistoryEventKind) -> HistoryEvent {
        HistoryEvent {
            id: 1,
            repo: "/r".into(),
            item: "t-abcd2345".into(),
            kind,
            ts_ms: 1_800_000_000_000,
            run_id: Some("r-aaaaaaaa".into()),
            attempt: Some(1),
            text: None,
            item_text: None,
            follow_ups: Vec::new(),
        }
    }

    #[test]
    fn a_timeline_shows_the_claim_the_runs_and_the_close() {
        let item = HistoryItem {
            item: "t-abcd2345".into(),
            title: Some("The item".into()),
            events: vec![
                HistoryEvent {
                    item_text: Some("- [ ] The item [t-abcd2345]\n  text\n".into()),
                    ..event(HistoryEventKind::Claimed)
                },
                HistoryEvent {
                    text: Some("## Decided\n- the driver\n".into()),
                    item_text: Some("- [ ] The item [t-abcd2345]\n".into()),
                    follow_ups: vec!["t-bcde3456".into()],
                    ..event(HistoryEventKind::Closed)
                },
            ],
            runs: vec![HistoryRun {
                run_id: "r-aaaaaaaa".into(),
                status: TodoRunStatus::Done,
                created_ms: 1_800_000_000_000,
                todo_commit: Some("0123456789abcdef".into()),
                attempts: vec![HistoryAttempt {
                    attempt: 1,
                    worker_id: Some("w1".into()),
                    branch: None,
                    commit: Some("fedcba9876543210".into()),
                    decision: Some("approve".into()),
                    verdict: Some(WorkerVerdict::Verified),
                    landed_sha: Some("aaaabbbbccccdddd".into()),
                }],
            }],
        };
        let text = item_text(&item, 0);
        assert!(text.starts_with("t-abcd2345  The item\n"), "{text}");
        assert!(
            text.contains(
                "2027-01-15 08:00  claimed  run r-aaaaaaaa attempt 1\n    | - [ ] The item"
            ),
            "{text}"
        );
        assert!(text.contains("    decision:\n    | ## Decided\n"), "{text}");
        assert!(text.contains("    follow-ups: t-bcde3456\n"), "{text}");
        assert!(text.contains(
            "    attempt 1  worker w1  verified  approve  commit fedcba987654  landed aaaabbbbcccc\n"
        ), "{text}");
    }

    #[test]
    fn overrides_are_listed_with_their_reason_pane_tenure_and_command() {
        assert!(matches!(
            parse(&args(&["overrides", "--json"])),
            Ok(Some((
                Command::Overrides(HistoryOverridesParams { repo: None }),
                true
            )))
        ));
        assert!(parse(&args(&["overrides", "--item", "t-abcd2345"])).is_err());
        let record = CoordinatorOverride {
            id: 1,
            ts_ms: 1_800_000_000_000,
            pane_id: "w1:p2".into(),
            tool: "Bash".into(),
            command: "just clean-install TODO.md # herdr-override: asked".into(),
            reason: "asked".into(),
            repo: Some("/repo".into()),
            coordinator_id: Some("c-abcdefgh".into()),
            item: None,
            session_id: None,
        };
        assert_eq!(
            overrides_text(&[record], 0),
            "2027-01-15 08:00  asked  pane w1:p2  c-abcdefgh  (/repo)\n    \
             Bash: just clean-install TODO.md # herdr-override: asked\n"
        );
        assert_eq!(overrides_text(&[], 0), "no recorded overrides\n");
    }

    #[test]
    fn the_reconcile_names_open_claims_and_deleted_items() {
        let text = reconcile_text(
            &HistoryReconcile {
                repo: "/r".into(),
                open_claims: vec![HistoryOpenClaim {
                    item: "t-abcd2345".into(),
                    run_id: "r-aaaaaaaa".into(),
                    claimed_ms: 1_800_000_000_000,
                    run_status: Some(TodoRunStatus::Waiting),
                }],
                deleted_without_close: vec![crate::api::schema::HistoryDeletedItem {
                    item: "t-bcde3456".into(),
                    title: None,
                    last: None,
                }],
            },
            0,
        );
        assert!(
            text.contains("claims without an end:\n  t-abcd2345  run r-aaaaaaaa"),
            "{text}"
        );
        assert!(text.contains("(in progress)"), "{text}");
        assert!(
            text.contains("without a closed record:\n  t-bcde3456  -\n"),
            "{text}"
        );
    }

    #[test]
    fn times_are_printed_in_local_time() {
        // 2027-01-15 08:00 UTC.
        let at = 1_800_000_000_000;
        assert_eq!(when(at, 0), "2027-01-15 08:00");
        assert_eq!(when(at, 2 * 3600), "2027-01-15 10:00");
        assert_eq!(when(at, -9 * 3600), "2027-01-14 23:00");
        let text = list_text(
            &[HistoryItemSummary {
                repo: "/r".into(),
                item: "t-abcd2345".into(),
                title: None,
                last: event(HistoryEventKind::Claimed),
            }],
            2 * 3600,
        );
        assert!(text.contains("claimed  2027-01-15 10:00"), "{text}");
    }

    #[test]
    fn an_item_without_a_title_shows_its_id_alone() {
        let item = HistoryItem {
            item: "t-abcd2345".into(),
            title: None,
            events: Vec::new(),
            runs: Vec::new(),
        };
        assert_eq!(item_text(&item, 0), "t-abcd2345\n\n");
    }
}
