//! Codex rate limits through `codex app-server` (`account/rateLimits/read`).
//!
//! Codex owns its ChatGPT login; herdr only speaks the app-server JSON-RPC
//! protocol over stdio and never reads Codex credentials.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::api::schema::{ProviderUsage, UsageBalance, UsageWindow};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
const RATE_LIMITS_REQUEST_ID: u64 = 2;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResult {
    rate_limits: Option<RateLimits>,
    rate_limit_reset_credits: Option<ResetCredits>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimits {
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
    credits: Option<Credits>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateWindow {
    used_percent: f64,
    window_duration_mins: Option<u64>,
    resets_at: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credits {
    #[serde(default)]
    has_credits: bool,
    #[serde(default)]
    unlimited: bool,
    balance: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetCredits {
    #[serde(default)]
    available_count: u64,
}

pub(super) fn fetch() -> Result<ProviderUsage, String> {
    let mut child = crate::noninteractive_process::command("codex")
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => "codex CLI not found on PATH".to_owned(),
            _ => format!("cannot start codex app-server: {error}"),
        })?;
    let result = request_rate_limits(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    parse_result(result?)
}

fn request_rate_limits(child: &mut Child) -> Result<serde_json::Value, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "codex app-server has no stdout".to_owned())?;
    let (lines_tx, lines_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                return;
            };
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "codex app-server has no stdin".to_owned())?;
    let requests = [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"clientInfo": {"name": "herdr", "version": env!("CARGO_PKG_VERSION")}},
        }),
        serde_json::json!({"jsonrpc": "2.0", "method": "initialized"}),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": RATE_LIMITS_REQUEST_ID,
            "method": "account/rateLimits/read",
        }),
    ];
    for request in requests {
        writeln!(stdin, "{request}")
            .map_err(|error| format!("codex app-server write failed: {error}"))?;
    }
    stdin
        .flush()
        .map_err(|error| format!("codex app-server write failed: {error}"))?;

    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let line = lines_rx
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => "codex app-server timed out".to_owned(),
                mpsc::RecvTimeoutError::Disconnected => "codex app-server exited".to_owned(),
            })?;
        if let Some(result) = rate_limits_response(&line) {
            return result;
        }
    }
}

fn rate_limits_response(line: &str) -> Option<Result<serde_json::Value, String>> {
    let message = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if message.get("id").and_then(serde_json::Value::as_u64) != Some(RATE_LIMITS_REQUEST_ID) {
        return None;
    }
    if let Some(error) = message.get("error") {
        let detail = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        return Some(Err(format!("codex rate limits unavailable: {detail}")));
    }
    Some(
        message
            .get("result")
            .cloned()
            .ok_or_else(|| "codex app-server returned no result".to_owned()),
    )
}

fn parse_result(result: serde_json::Value) -> Result<ProviderUsage, String> {
    let result: RateLimitsResult = serde_json::from_value(result)
        .map_err(|error| format!("unexpected codex rate limit response: {error}"))?;
    let limits = result
        .rate_limits
        .ok_or_else(|| "codex reported no rate limits; is it logged in with ChatGPT?".to_owned())?;
    let mut usage = ProviderUsage::pending("codex", "Codex");
    usage.plan = limits.plan_type.as_deref().map(plan_label);
    for (fallback_id, window) in [("primary", limits.primary), ("secondary", limits.secondary)] {
        let Some(window) = window else {
            continue;
        };
        let (id, label) = window_identity(window.window_duration_mins, fallback_id);
        usage.windows.push(UsageWindow {
            id,
            label,
            used_percent: super::clamp_percent(window.used_percent),
            resets_at: window.resets_at,
        });
    }
    if let Some(credits) = limits.credits {
        if credits.unlimited {
            usage.notes.push("unlimited credits".into());
        } else if credits.has_credits {
            if let Some(balance) = credits.balance {
                usage.balances.push(UsageBalance {
                    currency: "credits".into(),
                    total: balance,
                    granted: None,
                    topped_up: None,
                });
            }
        }
    }
    if let Some(resets) = result
        .rate_limit_reset_credits
        .filter(|resets| resets.available_count > 0)
    {
        let plural = if resets.available_count == 1 { "" } else { "s" };
        usage.notes.push(format!(
            "{} free limit reset{plural} available",
            resets.available_count
        ));
    }
    Ok(usage)
}

fn window_identity(minutes: Option<u64>, fallback_id: &str) -> (String, String) {
    match minutes {
        Some(300) => ("five_hour".into(), "5h".into()),
        Some(10_080) => ("weekly".into(), "week".into()),
        Some(minutes) if minutes % 1_440 == 0 => (
            format!("{}d", minutes / 1_440),
            format!("{}d", minutes / 1_440),
        ),
        Some(minutes) if minutes % 60 == 0 => {
            (format!("{}h", minutes / 60), format!("{}h", minutes / 60))
        }
        Some(minutes) => (format!("{minutes}m"), format!("{minutes}m")),
        None => (fallback_id.into(), fallback_id.into()),
    }
}

fn plan_label(plan: &str) -> String {
    match plan {
        "plus" => "Plus".into(),
        "pro" => "Pro".into(),
        "team" => "Team".into(),
        "business" => "Business".into(),
        "enterprise" => "Enterprise".into(),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_primary_and_weekly_windows() {
        let usage = parse_result(serde_json::json!({
            "rateLimits": {
                "primary": {"usedPercent": 59, "windowDurationMins": 300, "resetsAt": 1790272971},
                "secondary": {"usedPercent": 72, "windowDurationMins": 10080, "resetsAt": 1790492808},
                "credits": {"hasCredits": false, "unlimited": false, "balance": "0"},
                "planType": "plus"
            },
            "rateLimitResetCredits": {"availableCount": 3}
        }))
        .unwrap();
        assert_eq!(usage.plan.as_deref(), Some("Plus"));
        assert_eq!(
            usage
                .windows
                .iter()
                .map(|window| (
                    window.id.as_str(),
                    window.label.as_str(),
                    window.used_percent
                ))
                .collect::<Vec<_>>(),
            vec![("five_hour", "5h", 59), ("weekly", "week", 72)]
        );
        assert!(usage.balances.is_empty());
        assert_eq!(usage.notes, vec!["3 free limit resets available"]);
    }

    #[test]
    fn missing_rate_limits_is_an_error() {
        assert!(parse_result(serde_json::json!({"rateLimits": null})).is_err());
    }

    #[test]
    fn only_the_rate_limit_response_is_selected() {
        assert!(rate_limits_response(r#"{"id":1,"result":{}}"#).is_none());
        assert!(rate_limits_response(r#"{"method":"account/updated","params":{}}"#).is_none());
        assert!(rate_limits_response("not json").is_none());
        assert!(matches!(
            rate_limits_response(r#"{"id":2,"error":{"message":"not logged in"}}"#),
            Some(Err(message)) if message.contains("not logged in")
        ));
        assert!(matches!(
            rate_limits_response(r#"{"id":2,"result":{"rateLimits":null}}"#),
            Some(Ok(_))
        ));
    }

    #[test]
    fn unusual_window_lengths_get_compact_labels() {
        assert_eq!(window_identity(Some(1_440), "primary").1, "1d");
        assert_eq!(window_identity(Some(120), "primary").1, "2h");
        assert_eq!(window_identity(Some(45), "primary").1, "45m");
        assert_eq!(window_identity(None, "secondary").1, "secondary");
    }
}
