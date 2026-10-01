//! Gemini (Google AI subscription) limits through the Antigravity CLI.
//!
//! `agy -p /quota --output-format json` prints the weekly model-group quotas
//! without starting a model turn. Antigravity owns its Google login; herdr only
//! reads the command's JSON output and never touches its credentials.

use std::io::Read;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;

use crate::api::schema::{ProviderUsage, UsageWindow};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct PrintResult {
    status: Option<String>,
    command: Option<Command>,
}

#[derive(Deserialize)]
struct Command {
    data: Option<QuotaData>,
}

#[derive(Deserialize)]
struct QuotaData {
    #[serde(default)]
    groups: Vec<QuotaGroup>,
}

#[derive(Deserialize)]
struct QuotaGroup {
    #[serde(default)]
    buckets: Vec<QuotaBucket>,
}

#[derive(Deserialize)]
struct QuotaBucket {
    id: String,
    window: Option<String>,
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

pub(super) fn fetch() -> Result<ProviderUsage, String> {
    let mut child = crate::noninteractive_process::command("agy")
        .args(["-p", "/quota", "--output-format", "json"])
        // Keep agy from picking up a project or AGENTS.md from herdr's cwd.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => "agy (Antigravity CLI) not found on PATH".to_owned(),
            _ => format!("cannot start agy: {error}"),
        })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "agy has no stdout".to_owned())?;
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let result = stdout.read_to_string(&mut output);
        let _ = output_tx.send(result.map(|_| output));
    });
    let output = output_rx.recv_timeout(RESPONSE_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(format!("agy output unreadable: {error}")),
        Err(mpsc::RecvTimeoutError::Timeout) => return Err("agy /quota timed out".into()),
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err("agy exited".into()),
    };
    parse(&output)
}

fn parse(output: &str) -> Result<ProviderUsage, String> {
    let result: PrintResult = serde_json::from_str(output.trim())
        .map_err(|_| "unexpected agy /quota output; is agy logged in?".to_owned())?;
    if result
        .status
        .as_deref()
        .is_some_and(|status| status != "SUCCESS")
    {
        return Err("agy /quota failed; is agy logged in?".into());
    }
    let groups = result
        .command
        .and_then(|command| command.data)
        .map(|data| data.groups)
        .ok_or_else(|| "agy reported no quota; is it logged in?".to_owned())?;
    let mut usage = ProviderUsage::pending("gemini", "Gemini");
    for bucket in groups.into_iter().flat_map(|group| group.buckets) {
        let Some(remaining) = bucket.remaining_fraction else {
            continue;
        };
        let (id, label) = window_identity(&bucket.id, bucket.window.as_deref());
        usage.windows.push(UsageWindow {
            id,
            label,
            used_percent: super::clamp_percent((1.0 - remaining) * 100.0),
            resets_at: bucket.reset_time.as_deref().and_then(parse_timestamp),
        });
    }
    if usage.windows.is_empty() {
        return Err("agy reported no quota buckets".into());
    }
    Ok(usage)
}

/// `gemini-weekly` keeps the canonical window id; other model groups such as
/// `3p-weekly` get their bucket prefix so they stay distinct.
fn window_identity(bucket_id: &str, window: Option<&str>) -> (String, String) {
    let (base_id, base_label) = match window {
        Some("weekly") => ("weekly", "week"),
        Some("daily") => ("daily", "day"),
        Some("five_hour") => ("five_hour", "5h"),
        Some(other) => (other, other),
        None => (bucket_id, bucket_id),
    };
    match bucket_id.split_once('-') {
        Some(("gemini", _)) => (base_id.into(), base_label.into()),
        Some((group, _)) => (
            format!("{base_id}_{group}"),
            format!("{group} {base_label}"),
        ),
        None => (bucket_id.into(), base_label.into()),
    }
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    u64::try_from(parsed.unix_timestamp()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUOTA: &str = r#"{"conversation_id":"","status":"SUCCESS","response":"...","command":{"name":"usage","data":{"groups":[
        {"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.467468798160553,"reset_time":"2026-10-01T00:06:12Z"}]},
        {"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-10-02T09:31:04Z"}]}
    ]}}}"#;

    #[test]
    fn parses_model_group_buckets_as_windows() {
        let usage = parse(QUOTA).unwrap();
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
            vec![("weekly", "week", 53), ("weekly_3p", "3p week", 0)]
        );
        assert_eq!(usage.windows[0].resets_at, Some(1_790_813_172));
    }

    #[test]
    fn logged_out_or_unexpected_output_is_an_error() {
        assert!(parse("You are not logged into Antigravity.").is_err());
        assert!(parse(r#"{"status":"ERROR"}"#).is_err());
        assert!(parse(r#"{"status":"SUCCESS","command":{"data":{"groups":[]}}}"#).is_err());
    }
}
