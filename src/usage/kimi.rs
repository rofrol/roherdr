//! Kimi (Moonshot) prepaid API balance. The international platform
//! (`api.moonshot.ai`) bills in USD, the Chinese one (`api.moonshot.cn`) in
//! CNY; they are separate accounts, so the host is configurable and the
//! currency follows it.

use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageBalance};
use crate::config::UsageConfig;

#[derive(Deserialize)]
struct BalanceResponse {
    data: BalanceData,
}

#[derive(Deserialize)]
struct BalanceData {
    available_balance: f64,
    voucher_balance: Option<f64>,
    /// Can go negative when spend ran past the cash balance.
    cash_balance: Option<f64>,
}

pub(super) fn fetch(config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
    let key = super::keys::api_key(config, &super::keys::KeyedProvider::Kimi)
        .map_err(FetchError::Auth)?;
    let authorization = format!("Bearer {}", key.secret);
    let host = config.kimi_host.trim();
    let url = format!("https://{host}/v1/users/me/balance");
    let response = super::http::get(&url, &[("Authorization", &authorization)])?;
    match response.status {
        200 => {
            let mut usage = parse(&response.body, currency(host))?;
            usage.account = Some(key.account());
            Ok(usage)
        }
        401 | 403 => Err(FetchError::Auth("Kimi rejected the API key".into())),
        429 => Err(FetchError::RateLimited),
        status => Err(FetchError::Failed(format!(
            "Kimi balance request failed ({status})"
        ))),
    }
}

fn currency(host: &str) -> &'static str {
    if host.ends_with(".cn") {
        "CNY"
    } else {
        "USD"
    }
}

fn parse(body: &str, currency: &str) -> Result<ProviderUsage, String> {
    let response: BalanceResponse = serde_json::from_str(body)
        .map_err(|error| format!("unexpected Kimi balance response: {error}"))?;
    let amount = |value: f64| format!("{value:.2}");
    let mut usage = ProviderUsage::pending("kimi", "Kimi");
    usage.balances.push(UsageBalance {
        currency: currency.to_owned(),
        total: amount(response.data.available_balance),
        granted: response.data.voucher_balance.map(amount),
        topped_up: response.data.cash_balance.map(amount),
    });
    if response.data.available_balance <= 0.0 {
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
    fn parses_available_voucher_and_cash_balance() {
        let usage = parse(
            r#"{"code":0,"data":{"available_balance":49.58894,"voucher_balance":46.58893,"cash_balance":3.00001},"scode":"0x0","status":true}"#,
            "USD",
        )
        .unwrap();
        assert_eq!(
            usage.balances,
            vec![UsageBalance {
                currency: "USD".into(),
                total: "49.59".into(),
                granted: Some("46.59".into()),
                topped_up: Some("3.00".into()),
            }]
        );
        assert!(usage.notes.is_empty());
    }

    #[test]
    fn an_empty_balance_adds_a_note_and_negative_cash_is_kept() {
        let usage = parse(
            r#"{"data":{"available_balance":0,"voucher_balance":0,"cash_balance":-1.5}}"#,
            "CNY",
        )
        .unwrap();
        assert_eq!(usage.balances[0].topped_up.as_deref(), Some("-1.50"));
        assert_eq!(usage.notes.len(), 1);
    }

    #[test]
    fn the_chinese_platform_bills_in_yuan() {
        assert_eq!(currency("api.moonshot.cn"), "CNY");
        assert_eq!(currency("api.moonshot.ai"), "USD");
    }
}
