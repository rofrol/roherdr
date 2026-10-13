//! Anthropic API credits spent since the current grant, from the Admin API
//! cost report. Anthropic documents no credit-balance endpoint, so the spend
//! is the organization's billed cost from the grant's first UTC day on, and
//! the configured grant is the limit: `$58.91/$200` for a Console balance of
//! $141.09. Every API key and workspace of the organization draws from the
//! same credits, so the report is organization-wide, not herdr's own ledger.
//! The admin key comes only from a private file, never from the environment
//! that agent panes inherit.

use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageAccount, UsageSpend};
use crate::config::UsageConfig;

const COST_REPORT_URL: &str = "https://api.anthropic.com/v1/organizations/cost_report";
const API_VERSION: &str = "2023-06-01";
/// Daily buckets; a credit cycle has at most 31, the endpoint's page maximum.
const BUCKETS_PER_PAGE: u32 = 31;
/// A cycle fits one page; more pages than this means the cursor is misbehaving.
const MAX_PAGES: usize = 8;

#[derive(Deserialize)]
struct Page {
    // Required: a reply without them must not read as a cycle with no costs.
    data: Vec<Bucket>,
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct Bucket {
    #[serde(default)]
    results: Vec<CostResult>,
}

#[derive(Deserialize)]
struct CostResult {
    /// Lowest currency units (cents) as a decimal string: `"123.45"` is $1.23.
    amount: String,
    currency: String,
}

pub(super) fn fetch(config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
    let key = admin_key(config).map_err(|problem| FetchError::Setup {
        setup: setup_steps(config, &problem),
        message: problem.message,
    })?;
    let since_day = config.anthropic_credit_since.trim();
    let since = day_start_unix(since_day).ok_or_else(|| FetchError::Setup {
        message: "usage.anthropic_credit_since is not set to the grant's first day".into(),
        setup: grant_steps(),
    })?;
    let costs = fetch_all(&format!("{since_day}T00:00:00Z"), &key)?;
    let spent = spent_nano_cents(&costs)?;
    let mut usage = ProviderUsage::pending("anthropic_api", "Anthropic · API credits");
    usage.account = Some(UsageAccount {
        source: format!(
            "file:{}",
            super::expand_home(config.anthropic_admin_key_file.trim()).display()
        ),
        ..UsageAccount::default()
    });
    let limit = u64::from(config.anthropic_credit_usd) * 100;
    usage.spend.push(UsageSpend {
        currency: "USD".into(),
        amount: dollars(spent),
        since,
        limit: Some(dollars(i128::from(limit) * NANO_PER_CENT)),
        // Spending the grant does not prove Anthropic blocks requests: the
        // organization may hold other credits. The share used still colours it.
        limit_enforcing: false,
    });
    let expires = config.anthropic_credit_expires.trim();
    usage.notes.push(if expires.is_empty() {
        format!("credit granted {since_day}")
    } else {
        format!("credit granted {since_day}, expires {expires}")
    });
    usage.notes.push(
        "organization-wide billed cost since the grant day; the Console balance is the authority"
            .into(),
    );
    Ok(usage)
}

/// Why the admin key cannot be used.
pub(super) struct KeyProblem {
    pub(super) message: String,
    /// The file exists but is shared with other users or is malformed.
    pub(super) unsafe_file: bool,
}

pub(super) fn admin_key(config: &UsageConfig) -> Result<String, KeyProblem> {
    let path = super::expand_home(config.anthropic_admin_key_file.trim());
    let raw = crate::platform::read_secret_file(&path).map_err(|error| KeyProblem {
        unsafe_file: error.kind() != std::io::ErrorKind::NotFound,
        message: format!(
            "cannot read the Anthropic admin key from {}: {error}",
            path.display()
        ),
    })?;
    let key = raw.trim();
    // Headers travel to curl as lines; a key with a line break would add headers.
    if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(KeyProblem {
            unsafe_file: true,
            message: format!(
                "{} must hold the Anthropic admin key on one line",
                path.display()
            ),
        });
    }
    Ok(key.to_owned())
}

/// Steps shown in the details when the key is missing or unusable. The path
/// is the server's, which may be another machine than the viewer's.
fn setup_steps(config: &UsageConfig, problem: &KeyProblem) -> String {
    let path = config.anthropic_admin_key_file.trim();
    let mut steps = vec![
        "Create an Admin key at platform.claude.com: Settings, Admin keys.".to_owned(),
        format!("Save it as one line in {path} on the machine running this herdr server."),
        format!("Make it private: chmod 600 {path}"),
    ];
    if problem.unsafe_file {
        steps.insert(0, problem.message.clone());
    }
    steps.push(
        "herdr only reads the cost report, but an Admin key can manage the whole organization."
            .to_owned(),
    );
    steps.join("\n")
}

fn grant_steps() -> String {
    [
        "Open platform.claude.com, Settings, Billing, Promotional credits.",
        "Set usage.anthropic_credit_since to the day the credit arrived (YYYY-MM-DD),",
        "usage.anthropic_credit_usd to its amount and usage.anthropic_credit_expires to its expiry.",
    ]
    .join("\n")
}

fn fetch_all(starting_at: &str, key: &str) -> Result<Vec<CostResult>, FetchError> {
    let mut results = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = format!(
            "{COST_REPORT_URL}?starting_at={}&bucket_width=1d&limit={BUCKETS_PER_PAGE}",
            percent_encode(starting_at)
        );
        if let Some(cursor) = &cursor {
            url.push_str("&page=");
            url.push_str(&percent_encode(cursor));
        }
        let response = super::http::get(
            &url,
            &[("x-api-key", key), ("anthropic-version", API_VERSION)],
        )?;
        match response.status {
            200 => {}
            401 => {
                return Err(FetchError::Auth(
                    "Anthropic rejected the admin key as invalid or revoked".into(),
                ))
            }
            403 => {
                return Err(FetchError::Auth(
                    "the Anthropic key cannot read the cost report; it needs an Admin key".into(),
                ))
            }
            429 => return Err(FetchError::RateLimited),
            status => {
                return Err(FetchError::Failed(format!(
                    "Anthropic cost report request failed ({status})"
                )))
            }
        }
        let page: Page = serde_json::from_str(&response.body)
            .map_err(|error| format!("unexpected Anthropic cost report: {error}"))?;
        results.extend(page.data.into_iter().flat_map(|bucket| bucket.results));
        match next_cursor(page.has_more, page.next_page, cursor.as_deref())? {
            Some(next) => cursor = Some(next),
            None => return Ok(results),
        }
    }
    Err("Anthropic cost report pagination did not end"
        .to_owned()
        .into())
}

/// The cursor of the next page, `None` after the last one. More pages
/// without a usable new cursor would leave the total short, so they fail.
fn next_cursor(
    has_more: bool,
    next_page: Option<String>,
    current: Option<&str>,
) -> Result<Option<String>, String> {
    if !has_more {
        return Ok(None);
    }
    match next_page.filter(|next| !next.is_empty()) {
        Some(next) if current != Some(next.as_str()) => Ok(Some(next)),
        Some(_) => Err("Anthropic cost report repeated a page".to_owned()),
        None => Err("Anthropic cost report has more pages but no cursor".to_owned()),
    }
}

/// Billionths of a cent, so summing many decimal strings loses nothing.
const NANO_PER_CENT: i128 = 1_000_000_000;

/// The total in billionths of a cent. A non-USD or malformed amount fails the
/// read rather than showing a wrong total.
fn spent_nano_cents(costs: &[CostResult]) -> Result<i128, String> {
    costs.iter().try_fold(0i128, |total, cost| {
        if !cost.currency.eq_ignore_ascii_case("USD") {
            return Err(format!(
                "Anthropic reported a cost in {}, not USD",
                cost.currency
            ));
        }
        let amount = parse_nano_cents(&cost.amount)
            .ok_or_else(|| format!("unexpected Anthropic cost amount {:?}", cost.amount))?;
        total
            .checked_add(amount)
            .ok_or_else(|| "Anthropic cost total overflowed".to_owned())
    })
}

/// `"123.45"` cents as billionths of a cent; digits past the ninth decimal
/// are dropped.
fn parse_nano_cents(text: &str) -> Option<i128> {
    let (negative, digits) = match text.trim().strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.trim()),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || whole.len() > 18
    {
        return None;
    }
    let whole: i128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut nanos = String::from(&fraction[..fraction.len().min(9)]);
    while nanos.len() < 9 {
        nanos.push('0');
    }
    let value = whole * NANO_PER_CENT + nanos.parse::<i128>().ok()?;
    Some(if negative { -value } else { value })
}

/// Billionths of a cent as dollars with six decimals, rounded half away from zero.
fn dollars(nano_cents: i128) -> String {
    // A micro-dollar is a ten-thousandth of a cent: 100_000 billionths.
    let half = if nano_cents < 0 { -50_000 } else { 50_000 };
    let micros = (nano_cents + half) / 100_000;
    let sign = if micros < 0 { "-" } else { "" };
    let micros = micros.abs();
    format!("{sign}{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// Midnight UTC of a `YYYY-MM-DD` day as a Unix time.
fn day_start_unix(day: &str) -> Option<u64> {
    let mut parts = day.split('-');
    let (year, month, date) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || date.len() != 2 {
        return None;
    }
    let month = time::Month::try_from(month.parse::<u8>().ok()?).ok()?;
    let date =
        time::Date::from_calendar_date(year.parse().ok()?, month, date.parse().ok()?).ok()?;
    u64::try_from(date.midnight().assume_utc().unix_timestamp()).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spent_sums_cent_strings_of_every_bucket_exactly() {
        let page: Page = serde_json::from_str(
            r#"{"data":[
                {"starting_at":"2026-10-12T00:00:00Z","ending_at":"2026-10-13T00:00:00Z",
                 "results":[{"amount":"5000.5","currency":"USD","description":null},
                            {"amount":"890.123456789","currency":"USD"}]},
                {"results":[]},
                {"results":[{"amount":"0.376543211","currency":"USD"}]}
            ],"has_more":false,"next_page":null}"#,
        )
        .unwrap();
        let costs = page
            .data
            .into_iter()
            .flat_map(|bucket| bucket.results)
            .collect::<Vec<_>>();
        let spent = spent_nano_cents(&costs).unwrap();
        assert_eq!(spent, 5891 * NANO_PER_CENT);
        assert_eq!(dollars(spent), "58.910000");
    }

    #[test]
    fn amounts_are_cents_and_malformed_or_foreign_ones_fail() {
        assert_eq!(
            parse_nano_cents("123.45"),
            Some(12_345 * NANO_PER_CENT / 100)
        );
        assert_eq!(parse_nano_cents("7"), Some(7 * NANO_PER_CENT));
        assert_eq!(parse_nano_cents(".5"), Some(NANO_PER_CENT / 2));
        assert_eq!(parse_nano_cents("-2.5"), Some(-5 * NANO_PER_CENT / 2));
        for bad in ["", ".", "1e3", "1.2.3", "abc", "1,5"] {
            assert_eq!(parse_nano_cents(bad), None, "{bad}");
        }
        let euros = [CostResult {
            amount: "1".into(),
            currency: "EUR".into(),
        }];
        assert!(spent_nano_cents(&euros).is_err());
        assert_eq!(dollars(0), "0.000000");
        assert_eq!(dollars(-NANO_PER_CENT), "-0.010000");
    }

    #[test]
    fn a_reply_without_data_or_has_more_is_not_a_cycle_without_costs() {
        assert!(serde_json::from_str::<Page>("{}").is_err());
        assert!(serde_json::from_str::<Page>(r#"{"data":[]}"#).is_err());
        assert!(
            serde_json::from_str::<Page>(r#"{"data":[],"has_more":false,"next_page":null}"#)
                .is_ok()
        );
    }

    #[test]
    fn pagination_ends_only_on_the_last_page_and_never_short() {
        assert_eq!(next_cursor(false, Some("p".into()), None), Ok(None));
        assert_eq!(
            next_cursor(true, Some("p2".into()), Some("p1")),
            Ok(Some("p2".into()))
        );
        assert!(next_cursor(true, None, None).is_err());
        assert!(next_cursor(true, Some(String::new()), None).is_err());
        assert!(next_cursor(true, Some("p1".into()), Some("p1")).is_err());
    }

    #[test]
    fn grant_day_is_midnight_utc_and_must_be_a_real_date() {
        // 2026-10-12T00:00:00Z
        assert_eq!(day_start_unix("2026-10-12"), Some(1_791_763_200));
        for bad in [
            "",
            "2026-10",
            "2026-13-01",
            "2026-02-30",
            "26-10-12",
            "2026-10-12T00",
        ] {
            assert_eq!(day_start_unix(bad), None, "{bad}");
        }
    }

    #[test]
    fn setup_steps_name_the_server_path_and_the_problem_only_when_the_file_is_bad() {
        let config = UsageConfig::default();
        let missing = KeyProblem {
            message: "cannot read".into(),
            unsafe_file: false,
        };
        let steps = setup_steps(&config, &missing);
        assert!(steps.starts_with("Create an Admin key"));
        assert!(steps.contains("chmod 600 ~/.config/herdr/anthropic-admin-key"));
        let shared = KeyProblem {
            message: "readable by other users".into(),
            unsafe_file: true,
        };
        assert!(setup_steps(&config, &shared).starts_with("readable by other users\n"));
    }
}
