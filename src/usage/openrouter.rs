//! OpenRouter credits and API key spend.
//!
//! `/key` reports the key's spend and limit. `/credits` reports the account
//! balance; OpenRouter documents it as management-key only, so when it is
//! refused the key's remaining limit stands in for the balance.

use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageBalance};
use crate::config::UsageConfig;

const KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct KeyInfo {
    limit: Option<f64>,
    limit_remaining: Option<f64>,
    limit_reset: Option<String>,
    #[serde(default)]
    usage_daily: f64,
    #[serde(default)]
    usage_weekly: f64,
    #[serde(default)]
    usage_monthly: f64,
    #[serde(default)]
    is_free_tier: bool,
}

#[derive(Deserialize)]
struct Credits {
    total_credits: f64,
    total_usage: f64,
}

pub(super) fn fetch(config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
    let key = super::keys::api_key(config, &super::keys::KeyedProvider::OpenRouter)?;
    let authorization = format!("Bearer {key}");
    let headers = [("Authorization", authorization.as_str())];
    let response = super::http::get(KEY_URL, &headers)?;
    let key_info = match response.status {
        200 => parse::<KeyInfo>(&response.body)?,
        401 | 403 => return Err(FetchError::Failed("OpenRouter rejected the API key".into())),
        429 => return Err(FetchError::RateLimited),
        status => {
            return Err(FetchError::Failed(format!(
                "OpenRouter key request failed ({status})"
            )))
        }
    };
    // Documented as management-key only; the key limit is the fallback balance.
    let credits = super::http::get(CREDITS_URL, &headers)
        .ok()
        .filter(|response| response.status == 200)
        .and_then(|response| parse::<Credits>(&response.body).ok());
    Ok(usage(key_info, credits))
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str::<Envelope<T>>(body)
        .map(|envelope| envelope.data)
        .map_err(|error| format!("unexpected OpenRouter response: {error}"))
}

fn usage(key: KeyInfo, credits: Option<Credits>) -> ProviderUsage {
    let mut usage = ProviderUsage::pending("openrouter", "OpenRouter");
    if let Some(credits) = credits {
        usage
            .balances
            .push(dollars(credits.total_credits - credits.total_usage));
    } else if let Some(remaining) = key.limit_remaining {
        usage.balances.push(dollars(remaining));
        usage
            .notes
            .push("balance is this key's remaining limit".into());
    }
    if let Some(limit) = key.limit {
        let reset = key
            .limit_reset
            .map_or_else(String::new, |reset| format!(", resets {reset}"));
        usage.notes.push(format!("key limit ${limit:.2}{reset}"));
    }
    usage.notes.push(format!(
        "key spend: today ${:.2}, week ${:.2}, month ${:.2}",
        key.usage_daily, key.usage_weekly, key.usage_monthly
    ));
    if key.is_free_tier {
        usage.notes.push("free tier (no credits purchased)".into());
    }
    usage
}

fn dollars(amount: f64) -> UsageBalance {
    UsageBalance {
        currency: "USD".into(),
        total: format!("{amount:.2}"),
        granted: None,
        topped_up: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = r#"{"data":{"label":"sk-or-v1-abc...","limit":20,"limit_remaining":12.5,
        "limit_reset":"monthly","usage":7.5,"usage_daily":0.25,"usage_weekly":1.5,
        "usage_monthly":7.5,"is_free_tier":false}}"#;

    #[test]
    fn account_credits_take_precedence_over_the_key_limit() {
        let key = parse::<KeyInfo>(KEY).unwrap();
        let credits =
            parse::<Credits>(r#"{"data":{"total_credits":50,"total_usage":8.125}}"#).unwrap();
        let usage = usage(key, Some(credits));
        assert_eq!(usage.balances, vec![dollars(41.875)]);
        assert_eq!(usage.balances[0].total, "41.88");
        assert!(usage
            .notes
            .contains(&"key limit $20.00, resets monthly".into()));
    }

    #[test]
    fn key_limit_is_the_fallback_balance() {
        let usage = usage(parse::<KeyInfo>(KEY).unwrap(), None);
        assert_eq!(usage.balances[0].total, "12.50");
        assert!(usage
            .notes
            .contains(&"key spend: today $0.25, week $1.50, month $7.50".into()));
    }

    #[test]
    fn unlimited_key_without_credits_has_no_balance() {
        let key = parse::<KeyInfo>(
            r#"{"data":{"label":"","limit":null,"limit_remaining":null,"is_free_tier":true}}"#,
        )
        .unwrap();
        let usage = usage(key, None);
        assert!(usage.balances.is_empty());
        assert!(usage
            .notes
            .contains(&"free tier (no credits purchased)".into()));
    }
}
