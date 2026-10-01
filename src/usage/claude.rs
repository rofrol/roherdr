//! Claude subscription limits through the Claude Code OAuth login.
//!
//! The usage endpoint is undocumented and may change; failures degrade to an
//! error status instead of breaking the footer. Herdr never refreshes the
//! token itself so it cannot race Claude Code's own credential rotation.

use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageWindow};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const EXPIRED_LOGIN: &str = "Claude login expired; run Claude Code to renew it";

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OauthCredentials>,
}

#[derive(Deserialize)]
struct OauthCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Milliseconds since the Unix epoch.
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<LimitWindow>,
    seven_day: Option<LimitWindow>,
    seven_day_opus: Option<LimitWindow>,
    seven_day_sonnet: Option<LimitWindow>,
    extra_usage: Option<ExtraUsage>,
}

#[derive(Deserialize)]
struct LimitWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ExtraUsage {
    #[serde(default)]
    is_enabled: bool,
}

pub(super) fn fetch() -> Result<ProviderUsage, FetchError> {
    let credentials = read_credentials()?;
    if credentials
        .expires_at
        .is_some_and(|expires_at| expires_at / 1000 <= super::now_unix())
    {
        return Err(FetchError::Failed(EXPIRED_LOGIN.into()));
    }
    let authorization = format!("Bearer {}", credentials.access_token);
    let response = super::http::get(
        USAGE_URL,
        &[
            ("Authorization", &authorization),
            ("anthropic-beta", OAUTH_BETA),
        ],
    )?;
    match response.status {
        200 => {
            let mut usage = parse(&response.body)?;
            usage.plan = credentials.subscription_type.map(|plan| capitalize(&plan));
            Ok(usage)
        }
        401 | 403 => Err(FetchError::Failed(EXPIRED_LOGIN.into())),
        429 => Err(FetchError::RateLimited),
        status => Err(FetchError::Failed(format!(
            "Claude usage request failed ({status})"
        ))),
    }
}

fn read_credentials() -> Result<OauthCredentials, String> {
    let raw = credentials_file_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .or_else(|| crate::platform::read_keychain_generic_password(KEYCHAIN_SERVICE))
        .ok_or_else(|| "Claude Code login not found".to_owned())?;
    serde_json::from_str::<CredentialsFile>(&raw)
        .ok()
        .and_then(|file| file.oauth)
        .ok_or_else(|| "Claude Code login has no OAuth token".to_owned())
}

fn credentials_file_path() -> Option<std::path::PathBuf> {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".claude"))
        })?;
    Some(config_dir.join(".credentials.json"))
}

fn parse(body: &str) -> Result<ProviderUsage, String> {
    let response: UsageResponse = serde_json::from_str(body)
        .map_err(|error| format!("unexpected Claude usage response: {error}"))?;
    let mut usage = ProviderUsage::pending("claude", "Claude");
    for (id, label, window) in [
        ("five_hour", "5h", response.five_hour),
        ("weekly", "week", response.seven_day),
        ("weekly_opus", "opus week", response.seven_day_opus),
        ("weekly_sonnet", "sonnet week", response.seven_day_sonnet),
    ] {
        let Some(window) = window else {
            continue;
        };
        let Some(utilization) = window.utilization else {
            continue;
        };
        usage.windows.push(UsageWindow {
            id: id.into(),
            label: label.into(),
            used_percent: super::clamp_percent(utilization),
            resets_at: window.resets_at.as_deref().and_then(parse_timestamp),
        });
    }
    if response.extra_usage.is_some_and(|extra| extra.is_enabled) {
        usage.notes.push("extra usage is enabled".into());
    }
    Ok(usage)
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    u64::try_from(parsed.unix_timestamp()).ok()
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_and_weekly_windows() {
        let usage = parse(
            r#"{"five_hour":{"utilization":32.0,"resets_at":"2026-09-24T15:29:59.536013+00:00"},
                "seven_day":{"utilization":11.4,"resets_at":"2026-09-30T23:59:59+00:00"},
                "seven_day_opus":null,"seven_day_sonnet":null,
                "extra_usage":{"is_enabled":false}}"#,
        )
        .unwrap();
        assert_eq!(
            usage
                .windows
                .iter()
                .map(|window| (window.id.as_str(), window.used_percent))
                .collect::<Vec<_>>(),
            vec![("five_hour", 32), ("weekly", 11)]
        );
        assert_eq!(usage.windows[0].resets_at, Some(1_790_263_799));
        assert!(usage.notes.is_empty());
    }

    #[test]
    fn skips_windows_without_utilization() {
        let usage = parse(r#"{"five_hour":{"utilization":null,"resets_at":null}}"#).unwrap();
        assert!(usage.windows.is_empty());
    }

    #[test]
    fn capitalizes_plan_names() {
        assert_eq!(capitalize("max"), "Max");
        assert_eq!(capitalize(""), "");
    }
}
