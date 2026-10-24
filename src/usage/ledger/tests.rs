use std::io::Write;
use std::path::{Path, PathBuf};

use super::*;

/// 2026-10-23T12:00:00Z.
const NOON_MS: u64 = 1_792_756_800_000;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-usage-ledger-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn utc(_: i64) -> i64 {
    0
}

fn plus_two_hours(_: i64) -> i64 {
    2 * 3_600
}

fn ledger(path: Option<&Path>, now_ms: u64) -> UsageLedger {
    UsageLedger::open_with(path, now_ms, utc)
}

fn claude(id: &str, timestamp: &str, input: u64, output: u64) -> String {
    serde_json::json!({
        "type": "assistant",
        "timestamp": timestamp,
        "requestId": format!("req-{id}"),
        "message": {
            "id": id,
            "model": "claude-opus-5-5",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_input_tokens": 100,
                "cache_creation_input_tokens": 10,
            },
        },
    })
    .to_string()
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open");
    file.write_all(text.as_bytes()).expect("write");
}

fn line(text: String) -> String {
    text + "\n"
}

fn totals(report: &UsageLedgerReport) -> UsageLedgerTokens {
    let mut sum = UsageLedgerTokens::default();
    for space in &report.spaces {
        add(&mut sum, &space.tokens);
    }
    sum
}

#[test]
fn books_only_what_was_appended_since_the_offset() {
    let dir = temp_dir("delta");
    let path = dir.join("s1.jsonl");
    let mut ledger = ledger(None, NOON_MS);
    append(&path, &line(claude("m1", "2026-10-23T10:00:00Z", 1, 2)));
    let transcripts = vec![path.clone()];
    assert_eq!(
        ledger.book_session("claude", "s1", Some("w1"), &transcripts, NOON_MS),
        1
    );
    // Nothing new: nothing booked.
    assert_eq!(
        ledger.book_session("claude", "s1", Some("w1"), &transcripts, NOON_MS),
        0
    );
    // A line still being written waits for its newline.
    let next = claude("m2", "2026-10-23T10:01:00Z", 3, 4);
    let (head, tail) = next.split_at(20);
    append(&path, head);
    assert_eq!(
        ledger.book_session("claude", "s1", Some("w1"), &transcripts, NOON_MS),
        0
    );
    append(&path, &line(tail.to_string()));
    assert_eq!(
        ledger.book_session("claude", "s1", Some("w1"), &transcripts, NOON_MS),
        1
    );
    let report = ledger.report(None, &[], NOON_MS);
    assert_eq!(
        totals(&report),
        UsageLedgerTokens {
            input: 4,
            output: 6,
            cache_read: 200,
            cache_write: 20,
        }
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_response_written_as_several_lines_counts_once_with_its_largest_counts() {
    let dir = temp_dir("blocks");
    let path = dir.join("s1.jsonl");
    let mut ledger = ledger(None, NOON_MS);
    append(&path, &line(claude("m1", "2026-10-23T10:00:00Z", 5, 1)));
    append(&path, &line(claude("m1", "2026-10-23T10:00:00Z", 5, 9)));
    assert_eq!(
        ledger.book_session(
            "claude",
            "s1",
            Some("w1"),
            std::slice::from_ref(&path),
            NOON_MS
        ),
        1
    );
    let report = ledger.report(None, &[], NOON_MS);
    assert_eq!(totals(&report).input, 5);
    assert_eq!(totals(&report).output, 9);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_restart_goes_on_from_the_stored_offset_without_counting_twice() {
    let dir = temp_dir("restart");
    let db = dir.join(LEDGER_FILE);
    let path = dir.join("s1.jsonl");
    append(&path, &line(claude("m1", "2026-10-23T10:00:00Z", 1, 1)));
    {
        let mut first = ledger(Some(&db), NOON_MS);
        first.book_session(
            "claude",
            "s1",
            Some("w1"),
            std::slice::from_ref(&path),
            NOON_MS,
        );
    }
    append(&path, &line(claude("m2", "2026-10-23T11:00:00Z", 2, 2)));
    // A later server: its coverage start is the first one's.
    let mut second = ledger(Some(&db), NOON_MS + 1_000);
    assert_eq!(second.coverage_start_ms(), Some(NOON_MS));
    assert_eq!(
        second.stored_transcripts("claude", "s1"),
        vec![path.clone()]
    );
    assert_eq!(
        second.book_session(
            "claude",
            "s1",
            Some("w1"),
            std::slice::from_ref(&path),
            NOON_MS
        ),
        1
    );
    assert_eq!(
        second.book_session(
            "claude",
            "s1",
            Some("w1"),
            std::slice::from_ref(&path),
            NOON_MS
        ),
        0
    );
    assert_eq!(totals(&second.report(None, &[], NOON_MS)).input, 3);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn server_start_catches_up_a_session_no_pane_names() {
    let dir = temp_dir("catch-up");
    let db = dir.join(LEDGER_FILE);
    let path = dir.join("s1.jsonl");
    append(&path, &line(claude("m1", "2026-10-23T10:00:00Z", 1, 1)));
    {
        let mut first = ledger(Some(&db), NOON_MS);
        first.book_session(
            "claude",
            "s1",
            Some("w1"),
            std::slice::from_ref(&path),
            NOON_MS,
        );
    }
    // Written after the last booking, while no server read it.
    append(&path, &line(claude("m2", "2026-10-23T11:00:00Z", 2, 2)));
    let mut second = ledger(Some(&db), NOON_MS);
    // The cursor's own record is older than the write.
    assert_eq!(second.catch_up(NOON_MS + 60_000), 1);
    assert_eq!(second.catch_up(NOON_MS + 120_000), 0);
    let report = second.report(None, &[], NOON_MS);
    assert_eq!(report.spaces.len(), 1);
    assert_eq!(report.spaces[0].workspace_id.as_deref(), Some("w1"));
    assert_eq!(report.spaces[0].tokens.input, 3);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_truncated_or_replaced_transcript_neither_loses_nor_repeats_tokens() {
    let dir = temp_dir("rewrite");
    let path = dir.join("s1.jsonl");
    let mut ledger = ledger(None, NOON_MS);
    let first = line(claude("m1", "2026-10-23T10:00:00Z", 1, 1));
    let second = line(claude("m2", "2026-10-23T10:01:00Z", 2, 2));
    append(&path, &first);
    append(&path, &second);
    let transcripts = vec![path.clone()];
    assert_eq!(
        ledger.book_session("claude", "s1", None, &transcripts, NOON_MS),
        2
    );

    // Truncated to its first response, then a new one written over the old offsets.
    std::fs::write(&path, &first).expect("truncate");
    append(&path, &line(claude("m3", "2026-10-23T10:02:00Z", 4, 4)));
    assert_eq!(
        ledger.book_session("claude", "s1", None, &transcripts, NOON_MS),
        1
    );
    assert_eq!(totals(&ledger.report(None, &[], NOON_MS)).input, 7);

    // Replaced by a file of the same length whose bytes differ.
    let replaced = line(claude("m4", "2026-10-23T10:03:00Z", 8, 8));
    let old_len = std::fs::metadata(&path).expect("len").len() as usize;
    let mut text = replaced.clone();
    while text.len() < old_len {
        text.insert(0, '\n');
    }
    std::fs::write(&path, &text).expect("replace");
    assert_eq!(
        ledger.book_session("claude", "s1", None, &transcripts, NOON_MS),
        1
    );
    assert_eq!(totals(&ledger.report(None, &[], NOON_MS)).input, 15);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_response_copied_into_a_resumed_session_counts_once() {
    let dir = temp_dir("resumed");
    let mut ledger = ledger(None, NOON_MS);
    let original = dir.join("s1.jsonl");
    let resumed = dir.join("s2.jsonl");
    append(&original, &line(claude("m1", "2026-10-23T10:00:00Z", 1, 1)));
    append(&resumed, &line(claude("m1", "2026-10-23T10:00:00Z", 1, 1)));
    append(&resumed, &line(claude("m2", "2026-10-23T11:00:00Z", 2, 2)));
    ledger.book_session("claude", "s1", Some("w1"), &[original], NOON_MS);
    assert_eq!(
        ledger.book_session("claude", "s2", Some("w1"), &[resumed], NOON_MS),
        1
    );
    assert_eq!(totals(&ledger.report(None, &[], NOON_MS)).input, 3);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn days_split_in_local_time_with_models_per_day() {
    let dir = temp_dir("days");
    let path = dir.join("s1.jsonl");
    let mut ledger = UsageLedger::open_with(None, NOON_MS, plus_two_hours);
    // 21:59 UTC is 23:59 local on the 22nd; 22:00 UTC is the 23rd's midnight.
    append(&path, &line(claude("m1", "2026-10-22T21:59:00Z", 1, 1)));
    append(&path, &line(claude("m2", "2026-10-22T22:00:00Z", 2, 2)));
    append(&path, &line(claude("m3", "2026-10-23T09:00:00Z", 4, 4)));
    ledger.book_session("claude", "s1", Some("w1"), &[path], NOON_MS);
    let report = ledger.report(None, &[], NOON_MS);
    let days: Vec<_> = report.spaces[0]
        .days
        .iter()
        .map(|day| (day.day.as_str(), day.tokens.input))
        .collect();
    assert_eq!(days, vec![("2026-10-22", 1), ("2026-10-23", 6)]);
    assert_eq!(report.spaces[0].days[1].models[0].model, "claude-opus-5-5");
    assert_eq!(report.spaces[0].days[1].models[0].agent, "claude");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn aggregates_per_space_and_rolling_window_with_the_coverage_start() {
    let dir = temp_dir("spaces");
    let mut ledger = ledger(None, NOON_MS - 30 * 24 * 3_600_000);
    let one = dir.join("one.jsonl");
    let two = dir.join("two.jsonl");
    let none = dir.join("none.jsonl");
    // 30 min, 5 h and 2 days before noon.
    append(&one, &line(claude("a1", "2026-10-23T11:30:00Z", 1, 0)));
    append(&one, &line(claude("a2", "2026-10-23T07:00:00Z", 10, 0)));
    append(&two, &line(claude("b1", "2026-10-21T12:00:00Z", 100, 0)));
    append(&none, &line(claude("c1", "2026-10-23T11:59:00Z", 1_000, 0)));
    ledger.book_session("claude", "s1", Some("w1"), &[one], NOON_MS);
    ledger.book_session("claude", "s2", Some("w2"), &[two], NOON_MS);
    ledger.book_session("claude", "s3", None, &[none], NOON_MS);
    let labels = vec![
        ("w2".to_string(), "two".to_string()),
        ("w1".to_string(), "one".to_string()),
    ];
    let report = ledger.report(None, &labels, NOON_MS);
    assert_eq!(
        report.coverage_start,
        Some((NOON_MS - 30 * 24 * 3_600_000) / 1_000)
    );
    assert_eq!(report.as_of, Some(NOON_MS / 1_000));
    let spaces: Vec<_> = report
        .spaces
        .iter()
        .map(|space| {
            (
                space.workspace_id.as_deref(),
                space.label.as_deref(),
                space.tokens.input,
            )
        })
        .collect();
    assert_eq!(
        spaces,
        vec![
            (Some("w2"), Some("two"), 100),
            (Some("w1"), Some("one"), 11),
            (None, None, 1_000)
        ]
    );
    let windows = |windows: &[UsageLedgerWindow]| -> Vec<(String, u64)> {
        windows
            .iter()
            .map(|window| (window.id.clone(), window.tokens.input))
            .collect()
    };
    assert_eq!(
        windows(&report.spaces[1].windows),
        vec![
            ("1h".into(), 1),
            ("6h".into(), 11),
            ("24h".into(), 11),
            ("7d".into(), 11)
        ]
    );
    assert_eq!(
        windows(&report.windows),
        vec![
            ("1h".into(), 1_001),
            ("6h".into(), 1_011),
            ("24h".into(), 1_011),
            ("7d".into(), 1_111)
        ]
    );
    assert_eq!(report.windows[3].agents[0].agent, "claude");
    assert_eq!(
        report.spaces[0].first_at,
        Some(1_792_584_000) // 2026-10-21T12:00:00Z
    );
    // One space only; the windows over every space stay.
    let only = ledger.report(Some("w1"), &labels, NOON_MS);
    assert_eq!(only.spaces.len(), 1);
    assert_eq!(only.spaces[0].tokens.input, 11);
    assert_eq!(only.windows[3].tokens.input, 1_111);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn codex_and_pi_are_booked_from_their_own_formats() {
    let dir = temp_dir("formats");
    let mut ledger = ledger(None, NOON_MS);
    let codex = dir.join("rollout-x.jsonl");
    let count = |at: &str, input: u64, output: u64| {
        line(
            serde_json::json!({
                "timestamp": at,
                "type": "event_msg",
                "payload": {"type": "token_count", "info": {"total_token_usage": {
                    "input_tokens": input, "output_tokens": output
                }}}
            })
            .to_string(),
        )
    };
    append(
        &codex,
        &line(
            serde_json::json!({"type": "turn_context", "payload": {"model": "gpt-6.1-sol"}})
                .to_string(),
        ),
    );
    append(&codex, &count("2026-10-23T10:00:00Z", 100, 10));
    ledger.book_session(
        "codex",
        "x",
        Some("w1"),
        std::slice::from_ref(&codex),
        NOON_MS,
    );
    // The running total goes on across bookings: only the growth counts.
    append(&codex, &count("2026-10-23T10:01:00Z", 150, 30));
    ledger.book_session("codex", "x", Some("w1"), &[codex], NOON_MS);

    let pi = dir.join("2026-10-23_p.jsonl");
    append(
        &pi,
        &line(
            serde_json::json!({
                "type": "message", "id": "e1", "timestamp": "2026-10-23T10:00:00Z",
                "message": {"role": "assistant", "model": "glm", "usage": {
                    "input": 7, "output": 3, "cacheRead": 0, "cacheWrite": 0
                }}
            })
            .to_string(),
        ),
    );
    ledger.book_session("pi", "p", Some("w1"), &[pi], NOON_MS);
    let report = ledger.report(None, &[], NOON_MS);
    let models: Vec<_> = report.spaces[0]
        .models
        .iter()
        .map(|model| {
            (
                model.agent.as_str(),
                model.model.as_str(),
                model.tokens.input,
                model.tokens.output,
            )
        })
        .collect();
    assert_eq!(
        models,
        vec![("codex", "gpt-6.1-sol", 150, 30), ("pi", "glm", 7, 3)]
    );
    // An agent herdr cannot read books nothing.
    assert_eq!(ledger.book_session("copilot", "c", None, &[], NOON_MS), 0);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn the_report_serializes_its_optional_fields_only_when_known() {
    let empty = UsageLedgerReport::default();
    assert_eq!(
        serde_json::to_value(&empty).expect("json"),
        serde_json::json!({"spaces": [], "windows": []})
    );
    // An older or minimal answer without any field reads as empty.
    let read: UsageLedgerReport = serde_json::from_value(serde_json::json!({})).expect("read");
    assert_eq!(read, empty);
    let space: UsageLedgerSpace =
        serde_json::from_value(serde_json::json!({"tokens": {"input": 3}})).expect("space");
    assert_eq!(space.workspace_id, None);
    assert_eq!(space.tokens.output, 0);
    assert!(space.days.is_empty());

    let report = ledger(None, NOON_MS).report(None, &[], NOON_MS);
    let json = serde_json::to_value(&report).expect("json");
    assert_eq!(json["coverage_start"], NOON_MS / 1_000);
    assert_eq!(json["as_of"], NOON_MS / 1_000);
    let space = serde_json::to_value(UsageLedgerSpace::default()).expect("json");
    assert!(space.get("workspace_id").is_none());
    assert!(space.get("label").is_none());
    assert!(space.get("first_at").is_none());
}
