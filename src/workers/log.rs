//! A worker's journal as its transcript: the assistant's words, its tool
//! calls and their results, the policy's, the pre-tool checks' and the
//! user's decisions, and each turn's result, as structured entries
//! ([`record_entries`]) and as the text lines both `herdr worker log` and a
//! client's worker tab show ([`entry_lines`]). Anything else (rate limits,
//! control replies, the CLI's own bookkeeping) is left out.

use serde_json::Value;

use super::one_line;
use crate::api::schema::{WorkerTranscriptEntry, WorkerTranscriptRole};

/// Tool results and inputs are cut to this many characters per line.
const DETAIL_MAX: usize = 200;

/// A structured entry's text is cut to this many characters: a tool's
/// result can be a whole file.
const ENTRY_TEXT_MAX: usize = 4000;

/// The lines one journal record shows as, none for a record the log leaves
/// out. Broken records show as they are, so nothing is silently lost.
pub(crate) fn log_lines(record: &str) -> Vec<String> {
    record_entries(record)
        .iter()
        .flat_map(entry_lines)
        .collect()
}

/// The lines an entry shows as: the one renderer of `herdr worker log` and
/// a client's worker tab.
pub(crate) fn entry_lines(entry: &WorkerTranscriptEntry) -> Vec<String> {
    match entry {
        WorkerTranscriptEntry::Message { role, text } => match role {
            WorkerTranscriptRole::User => prefixed("› you: ", text),
            WorkerTranscriptRole::Result => prefixed("  ", text),
            WorkerTranscriptRole::Assistant | WorkerTranscriptRole::Unknown => prefixed("", text),
        },
        WorkerTranscriptEntry::ToolCall { name, input } => {
            vec![format!("→ {name}: {}", one_line(input, DETAIL_MAX))]
        }
        WorkerTranscriptEntry::ToolResult { text, is_error } => vec![format!(
            "  {} {}",
            if *is_error { "✗" } else { "←" },
            one_line(text, DETAIL_MAX)
        )],
        WorkerTranscriptEntry::Status { text } => text.lines().map(str::to_owned).collect(),
        WorkerTranscriptEntry::Unknown => Vec::new(),
    }
}

fn status(text: String) -> WorkerTranscriptEntry {
    WorkerTranscriptEntry::Status { text }
}

/// `text` cut to [`ENTRY_TEXT_MAX`] characters, its lines kept.
fn capped(text: &str) -> String {
    if text.chars().count() <= ENTRY_TEXT_MAX {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(ENTRY_TEXT_MAX - 1).collect();
    cut.push('…');
    cut
}

/// What one journal record shows as, none for a record the log leaves out.
pub(crate) fn record_entries(record: &str) -> Vec<WorkerTranscriptEntry> {
    let Ok(record) = serde_json::from_str::<Value>(record) else {
        return vec![status(format!("? {}", one_line(record, DETAIL_MAX)))];
    };
    record_value_entries(&record)
}

/// [`record_entries`] of a parsed record.
pub(crate) fn record_value_entries(record: &Value) -> Vec<WorkerTranscriptEntry> {
    let dir = record["dir"].as_str().unwrap_or("");
    if let Some(raw) = record["raw"].as_str() {
        return vec![status(match dir {
            "err" => format!("stderr: {}", one_line(raw, DETAIL_MAX)),
            _ => format!("? {}", one_line(raw, DETAIL_MAX)),
        })];
    }
    let event = &record["event"];
    let kind = event["type"].as_str().unwrap_or("");
    let lines: Vec<String> = match (dir, kind) {
        ("herdr", "started") => vec![format!(
            "▶ started{} in {}",
            event["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(|name| format!(" \"{name}\""))
                .unwrap_or_default(),
            event["cwd"].as_str().unwrap_or("?")
        )],
        ("herdr", "policy") => event["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|warning| format!("! policy: {warning}"))
            .collect(),
        ("herdr", "permission") => match event["decision"].as_str() {
            Some("allow") => vec![format!(
                "  ✓ {} allowed by the policy",
                event["tool_name"].as_str().unwrap_or("?")
            )],
            Some("deny") => vec![format!(
                "  ✗ {} denied by the policy: {}",
                event["tool_name"].as_str().unwrap_or("?"),
                event["message"].as_str().unwrap_or("")
            )],
            // The question that follows says it.
            _ => Vec::new(),
        },
        // An allowed call shows as the call itself.
        ("herdr", "pre_tool_check") if event["decision"] == "deny" => vec![format!(
            "  ✗ {} denied by a pre-tool check: {}",
            event["tool_name"].as_str().unwrap_or("?"),
            one_line(event["message"].as_str().unwrap_or(""), DETAIL_MAX)
        )],
        ("herdr", "pre_tool_check_failed") => vec![format!(
            "! pre-tool check{}: {}",
            event["check"]
                .as_array()
                .map(|argv| format!(
                    " `{}`",
                    argv.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                ))
                .unwrap_or_default(),
            event["error"].as_str().unwrap_or("unknown error")
        )],
        ("herdr", "tool_output_held") => {
            let mut lines = vec![format!(
                "! a tool call's output is held open after its process {} exited",
                event["exited_pid"].as_u64().unwrap_or(0)
            )];
            if let Ok(report) = serde_json::from_value(event.clone()) {
                lines.extend(
                    super::held_output_summary(&report)
                        .lines()
                        .map(|line| format!("  {}", one_line(line, DETAIL_MAX))),
                );
            }
            lines
        }
        ("herdr", "question") => {
            let question = &event["question"];
            vec![format!(
                "? waiting for you: {}: {}",
                question["tool_name"].as_str().unwrap_or("?"),
                question["text"].as_str().unwrap_or("")
            )]
        }
        ("herdr", "answer_failed") => vec![format!(
            "! your answer was not delivered, the question waits again: {}",
            event["error"].as_str().unwrap_or("unknown error")
        )],
        ("herdr", "answer_expired") => vec![format!(
            "! your answer to {} {}",
            event["request_id"].as_str().unwrap_or("?"),
            event["how"].as_str().unwrap_or("expired")
        )],
        // `answer` is the record of journals from before the answer outbox.
        ("herdr", "answer" | "answer_intent") => {
            let answers = event["answers"]
                .as_object()
                .map(|answers| {
                    answers
                        .iter()
                        .map(|(question, answer)| {
                            format!("{question} → {}", answer.as_str().unwrap_or(""))
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|answers| !answers.is_empty());
            vec![format!(
                "  you answered {}: {}",
                event["tool_name"].as_str().unwrap_or("?"),
                answers.unwrap_or_else(|| event["decision"].as_str().unwrap_or("?").to_owned())
            )]
        }
        ("herdr", "signal") => vec![format!(
            "■ {} sent",
            event["signal"].as_str().unwrap_or("a signal")
        )],
        ("herdr", "takeover") => {
            vec!["■ taken over: ending it to resume its session in a tab".into()]
        }
        ("herdr", "takeover_tab_opened") => vec![format!(
            "■ its session resumes in tab {}",
            event["tab_id"].as_str().unwrap_or("?")
        )],
        ("herdr", "takeover_failed") => vec![format!(
            "■ takeover failed: {}",
            event["error"].as_str().unwrap_or("unknown error")
        )],
        ("herdr", "exited") => vec![match (event["code"].as_i64(), event["signal"].as_i64()) {
            (_, Some(signal)) => format!("■ exited by signal {signal}"),
            (Some(code), None) => format!("■ exited with code {code}"),
            (None, None) => "■ exited".to_owned(),
        }],
        ("herdr", "lost") => vec![format!(
            "■ lost: {}",
            event["reason"].as_str().unwrap_or("the server ended first")
        )],
        ("in", "user") => {
            return user_text(event)
                .map(|text| WorkerTranscriptEntry::Message {
                    role: WorkerTranscriptRole::User,
                    text: capped(&text),
                })
                .into_iter()
                .collect();
        }
        ("in", "control_request") if event["request"]["subtype"] == "interrupt" => {
            vec!["■ interrupt sent".into()]
        }
        ("out", "system") if event["subtype"] == "init" => vec![format!(
            "  session {}",
            event["session_id"].as_str().unwrap_or("?")
        )],
        ("out", "assistant") => {
            return event["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| match block["type"].as_str() {
                    Some("text") => {
                        block["text"]
                            .as_str()
                            .map(|text| WorkerTranscriptEntry::Message {
                                role: WorkerTranscriptRole::Assistant,
                                text: capped(text),
                            })
                    }
                    Some("tool_use") => Some(WorkerTranscriptEntry::ToolCall {
                        name: block["name"].as_str().unwrap_or("tool").to_owned(),
                        input: capped(&tool_input_text(&block["input"])),
                    }),
                    _ => None,
                })
                .collect();
        }
        // The CLI echoes the prompt (`--replay-user-messages`), already shown
        // from the `in` side; tool results come back as user messages too.
        ("out", "user") => {
            return event["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"] == "tool_result")
                .map(|block| WorkerTranscriptEntry::ToolResult {
                    text: capped(&tool_result_text(&block["content"])),
                    is_error: block["is_error"].as_bool().unwrap_or(false),
                })
                .collect();
        }
        ("out", "result") => {
            let text = event["result"].as_str().unwrap_or("");
            let mut entries = vec![status(format!(
                "■ turn {}{}",
                event["subtype"].as_str().unwrap_or("ended"),
                event["terminal_reason"]
                    .as_str()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            ))];
            if !text.is_empty() {
                entries.push(WorkerTranscriptEntry::Message {
                    role: WorkerTranscriptRole::Result,
                    text: capped(text),
                });
            }
            return entries;
        }
        _ => Vec::new(),
    };
    lines.into_iter().map(status).collect()
}

/// `text` split into lines, the first one after `prefix`.
fn prefixed(prefix: &str, text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{}{line}", " ".repeat(prefix.chars().count()))
            }
        })
        .collect()
}

pub(super) fn user_text(event: &Value) -> Option<String> {
    let content = &event["message"]["content"];
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let text = content
        .as_array()?
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// A tool call's input as its most telling field: the command, the path or
/// the URL, else the input itself; on one line, cut.
pub(super) fn tool_input(input: &Value) -> String {
    one_line(&tool_input_text(input), DETAIL_MAX)
}

/// [`tool_input`] as written, its lines kept.
fn tool_input_text(input: &Value) -> String {
    [
        "command",
        "file_path",
        "notebook_path",
        "path",
        "url",
        "pattern",
    ]
    .iter()
    .find_map(|key| input[*key].as_str())
    .map(str::to_owned)
    .unwrap_or_else(|| input.to_string())
}

pub(super) fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(dir: &str, event: Value) -> Vec<String> {
        log_lines(&json!({"ts_ms": 1, "dir": dir, "event": event}).to_string())
    }

    #[test]
    fn shows_the_conversation_tools_decisions_and_result() {
        assert_eq!(
            lines(
                "in",
                json!({"type": "user", "message": {"role": "user", "content": "fix it\nplease"}})
            ),
            vec!["› you: fix it", "       please"]
        );
        assert_eq!(
            lines(
                "out",
                json!({"type": "assistant", "message": {"content": [
                    {"type": "text", "text": "Looking."},
                    {"type": "tool_use", "name": "Bash", "input": {"command": "git status"}}
                ]}})
            ),
            vec!["Looking.", "→ Bash: git status"]
        );
        assert_eq!(
            lines(
                "out",
                json!({"type": "user", "message": {"content": [
                    {"type": "tool_result", "content": "clean\ntree", "is_error": false}
                ]}})
            ),
            vec!["  ← clean tree"]
        );
        assert_eq!(
            lines(
                "herdr",
                json!({"type": "question", "question": {"tool_name": "Bash", "text": "git push"}})
            ),
            vec!["? waiting for you: Bash: git push"]
        );
        assert_eq!(
            lines(
                "herdr",
                json!({"type": "answer", "tool_name": "Bash", "decision": "deny", "answers": null})
            ),
            vec!["  you answered Bash: deny"]
        );
        assert_eq!(
            lines(
                "out",
                json!({"type": "result", "subtype": "success", "terminal_reason": "completed",
                       "result": "Done."})
            ),
            vec!["■ turn success (completed)", "  Done."]
        );
    }

    #[test]
    fn leaves_out_echoes_and_bookkeeping() {
        assert!(lines(
            "out",
            json!({"type": "user", "message": {"content": "fix it"}})
        )
        .is_empty());
        assert!(lines("out", json!({"type": "rate_limit_event"})).is_empty());
        assert!(lines("herdr", json!({"type": "permission", "decision": "ask"})).is_empty());
    }

    #[test]
    fn broken_records_show_as_they_are() {
        assert_eq!(log_lines("not json"), vec!["? not json"]);
        assert_eq!(
            log_lines(&json!({"dir": "err", "raw": "boom"}).to_string()),
            vec!["stderr: boom"]
        );
    }
}
