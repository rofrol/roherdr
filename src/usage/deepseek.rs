use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageBalance};
use crate::config::UsageConfig;

const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

#[derive(Deserialize)]
struct BalanceResponse {
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<BalanceInfo>,
}

#[derive(Deserialize)]
struct BalanceInfo {
    currency: String,
    total_balance: String,
    granted_balance: Option<String>,
    topped_up_balance: Option<String>,
}

pub(super) fn fetch(config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
    let key = super::keys::api_key(config, &super::keys::KeyedProvider::DeepSeek)?;
    let authorization = format!("Bearer {key}");
    let response = super::http::get(BALANCE_URL, &[("Authorization", &authorization)])?;
    match response.status {
        200 => Ok(parse(&response.body)?),
        401 | 403 => Err(FetchError::Failed("DeepSeek rejected the API key".into())),
        429 => Err(FetchError::RateLimited),
        status => Err(FetchError::Failed(format!(
            "DeepSeek balance request failed ({status})"
        ))),
    }
}

fn parse(body: &str) -> Result<ProviderUsage, String> {
    let response: BalanceResponse = serde_json::from_str(body)
        .map_err(|error| format!("unexpected DeepSeek balance response: {error}"))?;
    let mut usage = ProviderUsage::pending("deepseek", "DeepSeek");
    usage.balances = response
        .balance_infos
        .into_iter()
        .map(|info| UsageBalance {
            currency: info.currency,
            total: info.total_balance,
            granted: info.granted_balance,
            topped_up: info.topped_up_balance,
        })
        .collect();
    if !response.is_available {
        usage
            .notes
            .push("balance is insufficient for API calls".into());
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_balance_per_currency() {
        let usage = parse(
            r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"13.41","granted_balance":"0.00","topped_up_balance":"13.41"}]}"#,
        )
        .unwrap();
        assert_eq!(
            usage.balances,
            vec![UsageBalance {
                currency: "USD".into(),
                total: "13.41".into(),
                granted: Some("0.00".into()),
                topped_up: Some("13.41".into()),
            }]
        );
        assert!(usage.notes.is_empty());
    }

    #[test]
    fn unavailable_balance_adds_a_note() {
        let usage = parse(r#"{"is_available":false,"balance_infos":[]}"#).unwrap();
        assert_eq!(usage.notes.len(), 1);
    }
}
