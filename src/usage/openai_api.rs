//! OpenAI API (platform, pay-as-you-go) month-to-date spend and completion
//! tokens from the Admin API. OpenAI documents no prepaid-balance endpoint, so
//! this reports spend, never credit left. The admin key reads
//! organization-wide billing: it comes only from a private file, never from
//! the environment that agent panes inherit.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageCompletionTokens, UsageSpend};
use crate::config::UsageConfig;

const COSTS_URL: &str = "https://api.openai.com/v1/organization/costs";
const COMPLETIONS_URL: &str = "https://api.openai.com/v1/organization/usage/completions";
/// Daily buckets; a month has at most 31, which is also the usage endpoint's page limit.
const BUCKETS_PER_PAGE: u32 = 31;
/// A month fits one page; more pages than this means the cursor is misbehaving.
const MAX_PAGES: usize = 4;

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Page<T> {
    #[serde(default = "Vec::new")]
    data: Vec<Bucket<T>>,
    #[serde(default)]
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Bucket<T> {
    #[serde(default = "Vec::new")]
    results: Vec<T>,
}

#[derive(Deserialize)]
struct CostResult {
    amount: Amount,
}

#[derive(Deserialize)]
struct Amount {
    value: f64,
    currency: String,
}

#[derive(Deserialize)]
struct CompletionsResult {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    input_cached_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    num_model_requests: u64,
}

pub(super) fn fetch(config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
    let key = admin_key(config)?;
    let authorization = format!("Bearer {key}");
    let since = month_start_utc(super::now_unix());
    let costs: Vec<CostResult> = fetch_all(COSTS_URL, since, &authorization)?;
    let completions: Vec<CompletionsResult> = fetch_all(COMPLETIONS_URL, since, &authorization)?;
    let mut usage = ProviderUsage::pending("openai_api", "OpenAI API");
    usage.spend = spend(&costs, since);
    usage.completion_tokens = Some(completion_tokens(&completions, since));
    usage
        .notes
        .push("organization-wide; OpenAI reports costs with a delay".into());
    Ok(usage)
}

fn admin_key(config: &UsageConfig) -> Result<String, String> {
    let path = super::expand_home(config.openai_admin_key_file.trim());
    let raw = crate::platform::read_secret_file(&path).map_err(|error| {
        format!(
            "cannot read the OpenAI admin key from {}: {error}",
            path.display()
        )
    })?;
    let key = raw.trim();
    // Headers travel to curl as lines; a key with a line break would add headers.
    if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{} must hold the OpenAI admin key on one line",
            path.display()
        ));
    }
    Ok(key.to_owned())
}

fn fetch_all<T: serde::de::DeserializeOwned>(
    base: &str,
    since: u64,
    authorization: &str,
) -> Result<Vec<T>, FetchError> {
    let mut results = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = format!("{base}?start_time={since}&bucket_width=1d&limit={BUCKETS_PER_PAGE}");
        if let Some(cursor) = &cursor {
            url.push_str("&page=");
            url.push_str(&percent_encode(cursor));
        }
        let response = super::http::get(&url, &[("Authorization", authorization)])?;
        match response.status {
            200 => {}
            401 => {
                return Err(FetchError::Failed(
                    "OpenAI rejected the admin key as invalid or revoked".into(),
                ))
            }
            403 => {
                return Err(FetchError::Failed(
                    "the OpenAI admin key needs read access to usage and costs".into(),
                ))
            }
            429 => return Err(FetchError::RateLimited),
            status => {
                return Err(FetchError::Failed(format!(
                    "OpenAI usage request failed ({status})"
                )))
            }
        }
        let page: Page<T> = serde_json::from_str(&response.body)
            .map_err(|error| format!("unexpected OpenAI usage response: {error}"))?;
        results.extend(page.data.into_iter().flat_map(|bucket| bucket.results));
        match page.next_page.filter(|_| page.has_more) {
            Some(next) if cursor.as_deref() != Some(next.as_str()) => cursor = Some(next),
            Some(_) => return Err("OpenAI usage pagination repeated a page".to_owned().into()),
            None => return Ok(results),
        }
    }
    Err("OpenAI usage pagination did not end".to_owned().into())
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// One entry per currency; a month without costs reports zero in USD so the
/// footer can tell "nothing spent" from "not read yet".
fn spend(costs: &[CostResult], since: u64) -> Vec<UsageSpend> {
    let mut totals = BTreeMap::<String, f64>::new();
    for cost in costs {
        if cost.amount.value.is_finite() {
            *totals
                .entry(cost.amount.currency.to_ascii_uppercase())
                .or_default() += cost.amount.value;
        }
    }
    if totals.is_empty() {
        totals.insert("USD".into(), 0.0);
    }
    totals
        .into_iter()
        .map(|(currency, amount)| UsageSpend {
            currency,
            amount: format!("{amount:.6}"),
            since,
        })
        .collect()
}

fn completion_tokens(results: &[CompletionsResult], since: u64) -> UsageCompletionTokens {
    results.iter().fold(
        UsageCompletionTokens {
            input: 0,
            cached_input: 0,
            output: 0,
            requests: 0,
            since,
        },
        |mut total, result| {
            total.input = total.input.saturating_add(result.input_tokens);
            total.cached_input = total
                .cached_input
                .saturating_add(result.input_cached_tokens);
            total.output = total.output.saturating_add(result.output_tokens);
            total.requests = total.requests.saturating_add(result.num_model_requests);
            total
        },
    )
}

/// OpenAI's daily buckets are UTC days, so the month starts at 00:00 UTC.
fn month_start_utc(now_unix: u64) -> u64 {
    let Ok(now) = time::OffsetDateTime::from_unix_timestamp(now_unix as i64) else {
        return now_unix;
    };
    now.date().replace_day(1).map_or(now_unix, |first| {
        first.midnight().assume_utc().unix_timestamp() as u64
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_sums_every_result_of_every_bucket_per_currency() {
        let page: Page<CostResult> = serde_json::from_str(
            r#"{"data":[
                {"results":[{"amount":{"currency":"usd","value":0.080173}},{"amount":{"currency":"usd","value":1.5}}]},
                {"results":[]},
                {"results":[{"amount":{"currency":"usd","value":0.02}}]}
            ],"has_more":false,"next_page":null}"#,
        )
        .unwrap();
        let costs = page
            .data
            .into_iter()
            .flat_map(|bucket| bucket.results)
            .collect::<Vec<_>>();
        assert_eq!(
            spend(&costs, 7),
            vec![UsageSpend {
                currency: "USD".into(),
                amount: "1.600173".into(),
                since: 7,
            }]
        );
    }

    #[test]
    fn a_month_without_costs_reports_zero() {
        assert_eq!(spend(&[], 7)[0].amount, "0.000000");
    }

    #[test]
    fn completion_tokens_keep_cached_input_inside_input() {
        let results = vec![
            CompletionsResult {
                input_tokens: 1_000,
                input_cached_tokens: 400,
                output_tokens: 50,
                num_model_requests: 2,
            },
            CompletionsResult {
                input_tokens: 10,
                input_cached_tokens: 0,
                output_tokens: 5,
                num_model_requests: 1,
            },
        ];
        let tokens = completion_tokens(&results, 3);
        assert_eq!(
            (
                tokens.input,
                tokens.cached_input,
                tokens.output,
                tokens.requests
            ),
            (1_010, 400, 55, 3)
        );
    }

    #[test]
    fn month_starts_at_midnight_utc_on_the_first() {
        // 2026-10-03T05:40:00Z
        assert_eq!(month_start_utc(1_791_006_000), 1_790_812_800);
    }

    #[test]
    fn cursor_is_percent_encoded() {
        assert_eq!(percent_encode("page_a+b/c="), "page_a%2Bb%2Fc%3D");
    }
}
