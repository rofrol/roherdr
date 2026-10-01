//! Claude subscription limits through the Claude Code OAuth login.
//!
//! The usage endpoint is undocumented and may change; failures degrade to an
//! error status instead of breaking the footer. Herdr never refreshes the
//! token itself so it cannot race Claude Code's own credential rotation.
//!
//! Model-scoped buckets come from `limits[]`; omarchy's
//! `bin/omarchy-agent-usage-claude` reads the same endpoint and is the reference
//! for what those undocumented fields mean. When updating this parser, compare
//! with the upstream script (blob 5b3634aef1c73a2b0cbbdc43032bdbc04a8a329e,
//! 34228 bytes, branch `quattro`, moved from `basecamp/omarchy` to
//! `omacom/omarchy`) instead of guessing. Its commit `efe805387e` "Title a
//! model-scoped limit the way the flat ones title themselves" is what this
//! title format follows.

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
    /// Model-scoped windows; the fixed fields above do not cover them (a plan
    /// can limit one model, for example a separate weekly bucket for Fable).
    #[serde(default)]
    limits: Vec<LimitEntry>,
}

#[derive(Deserialize)]
struct LimitEntry {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    percent: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    scope: Option<LimitScope>,
}

#[derive(Deserialize)]
struct LimitScope {
    #[serde(default)]
    model: Option<LimitModel>,
}

#[derive(Deserialize)]
struct LimitModel {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    id: Option<String>,
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
    // Claude Code 2.1.286's /usage schemas explicitly describe utilization as
    // "Percentage of the window used, 0-100" and limits[].percent as "Share of
    // the window used, 0-100". Its usage UI divides utilization by 100 for bars.
    // Rate-limit response headers use fractions, but they are a different input:
    // Claude Code multiplies those by 100 when constructing usage-shaped data.
    // Values below one from this endpoint are therefore small percentages;
    // unrelated windows must not choose the unit.
    for (id, label, window) in [
        ("five_hour", "5h", response.five_hour.as_ref()),
        ("weekly", "week", response.seven_day.as_ref()),
        ("weekly_opus", "opus week", response.seven_day_opus.as_ref()),
        (
            "weekly_sonnet",
            "sonnet week",
            response.seven_day_sonnet.as_ref(),
        ),
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
    let mut unreadable_scoped = 0usize;
    for entry in &response.limits {
        let name = entry
            .scope
            .as_ref()
            .and_then(|scope| scope.model.as_ref())
            .and_then(|model| match model.display_name.as_deref() {
                Some(name) => Some(name),
                // An entry carrying only an id still names a window worth showing.
                None => model.id.as_deref(),
            })
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let Some(name) = name else {
            continue;
        };
        let Some(percent) = entry.percent else {
            // Report rather than guess when a scoped limit carries no number.
            unreadable_scoped += 1;
            continue;
        };
        let used_percent = super::clamp_percent(percent);
        let label = format!("{name} week");
        if usage
            .windows
            .iter()
            .any(|window| window.label.eq_ignore_ascii_case(&label))
        {
            continue;
        }
        usage.windows.push(UsageWindow {
            id: format!("scoped:{}", name.to_ascii_lowercase()),
            label,
            used_percent,
            resets_at: entry.resets_at.as_deref().and_then(parse_timestamp),
        });
    }
    if unreadable_scoped > 0 {
        usage.notes.push(format!(
            "{unreadable_scoped} model-scoped limits[] entries had no percentage"
        ));
    }
    if response.extra_usage.is_some_and(|extra| extra.is_enabled) {
        usage.notes.push("extra usage is enabled".into());
    }
    for entry in &response.limits {
        if !entry
            .severity
            .as_deref()
            .is_some_and(|severity| severity.eq_ignore_ascii_case("critical"))
        {
            continue;
        }
        let bucket = entry.kind.as_deref().unwrap_or("limit");
        let note = format!("{bucket} reported critical");
        if !usage.notes.iter().any(|existing| existing == &note) {
            usage.notes.push(note);
        }
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

    #[test]
    fn parses_scoped_model_windows() {
        let usage = parse(
            r#"{"five_hour":{"utilization":0.0},"seven_day":{"utilization":100.0},
                "limits":[
                  {"kind":"session","group":"session","percent":0,"severity":"normal"},
                  {"kind":"weekly_all","group":"weekly","percent":100,"severity":"critical"},
                  {"kind":"weekly_scoped","group":"weekly","percent":1,"severity":"normal",
                   "resets_at":"2026-10-01T23:59:59+00:00","scope":{"model":{"display_name":"Fable"}}}
                ]}"#,
        )
        .unwrap();
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
            vec![
                ("five_hour", "5h", 0),
                ("weekly", "week", 100),
                ("scoped:fable", "Fable week", 1),
            ]
        );
        assert_eq!(usage.windows[2].resets_at, Some(1_790_899_199));
        // The fixed session and weekly windows are not repeated from limits[].
        assert_eq!(usage.windows.len(), 3);
    }

    #[test]
    fn percentage_units_do_not_depend_on_other_windows() {
        // The public representation rounds to whole percentages. A low value
        // must never be multiplied by 100, regardless of the other windows.
        for (session, weekly, scoped, expected) in [
            (0.32, 0.114, 0.5, vec![0, 0, 1]),
            (1.0, 100.0, 1.0, vec![1, 100, 1]),
            (0.5, 100.0, 0.5, vec![1, 100, 1]),
            (0.5, 0.25, 100.0, vec![1, 0, 100]),
        ] {
            let body = format!(
                r#"{{"five_hour":{{"utilization":{session}}},
                    "seven_day":{{"utilization":{weekly}}},
                    "limits":[{{"kind":"weekly_scoped","percent":{scoped},
                      "scope":{{"model":{{"display_name":"Fable"}}}}}}]}}"#
            );
            let usage = parse(&body).unwrap();
            assert_eq!(
                usage
                    .windows
                    .iter()
                    .map(|window| window.used_percent)
                    .collect::<Vec<_>>(),
                expected,
                "{body}"
            );
        }
    }

    #[test]
    fn critical_severity_becomes_a_note() {
        let usage = parse(
            r#"{"limits":[{"kind":"weekly_all","percent":100,"severity":"critical"},
                 {"kind":"session","percent":0,"severity":"normal"}]}"#,
        )
        .unwrap();
        assert_eq!(usage.notes, vec!["weekly_all reported critical"]);
    }

    #[test]
    fn skips_scoped_entries_without_a_model_or_a_known_bucket() {
        let usage = parse(
            r#"{"seven_day_sonnet":{"utilization":40.0},
                "limits":[
                  {"kind":"weekly_scoped","percent":40,"scope":{"model":{"display_name":"Sonnet"}}},
                  {"kind":"weekly_scoped","percent":7,"scope":{"model":{"display_name":"  "}}},
                  {"kind":"weekly_scoped","scope":null}
                ]}"#,
        )
        .unwrap();
        assert_eq!(
            usage
                .windows
                .iter()
                .map(|window| (window.label.as_str(), window.used_percent))
                .collect::<Vec<_>>(),
            vec![("sonnet week", 40)]
        );
    }

    #[test]
    fn notes_scoped_entries_without_a_percentage() {
        let usage = parse(
            r#"{"limits":[{"kind":"weekly_scoped","scope":{"model":{"display_name":"Fable"}}},
                 {"kind":"weekly_scoped","percent":40,"scope":{"model":{"display_name":"Sonnet"}}}]}"#,
        )
        .unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(
            usage.notes,
            vec!["1 model-scoped limits[] entries had no percentage"]
        );
    }

    #[test]
    fn falls_back_to_the_model_id_when_there_is_no_display_name() {
        let usage = parse(
            r#"{"limits":[{"kind":"weekly_scoped","percent":12,
                 "scope":{"model":{"id":"haiku"}}}]}"#,
        )
        .unwrap();
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
            vec![("scoped:haiku", "haiku week", 12)]
        );
    }
}
