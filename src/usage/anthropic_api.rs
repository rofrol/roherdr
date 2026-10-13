//! Anthropic Console credits left, from the Admin API cost report.
//! Anthropic documents no credit-balance endpoint, so herdr replays the
//! organization's billed cost day by day against the grants the Console's
//! billing page lists (configured in `[[usage.anthropic_credits]]`): a plan's
//! included credits first, as Anthropic spends them, then the others by
//! soonest expiry. The row reads `$76.92/$200 +$9.87`: the included grant
//! spent of granted, plus what the other grants have left. Every API key and
//! workspace of the organization draws from the same credits, so the report
//! is organization-wide, not herdr's own ledger. The admin key comes only
//! from a private file, never from the environment that agent panes inherit.

use serde::Deserialize;

use super::FetchError;
use crate::api::schema::{ProviderUsage, UsageAccount, UsageBalance, UsageSpend};
use crate::config::{AnthropicCreditGrant, UsageConfig};

const COST_REPORT_URL: &str = "https://api.anthropic.com/v1/organizations/cost_report";
const API_VERSION: &str = "2023-06-01";
/// Daily buckets; 31 is the endpoint's page maximum.
const BUCKETS_PER_PAGE: u32 = 31;
/// About 16 months of days; more pages than this means the cursor is
/// misbehaving or a grant that ended long ago is still configured.
const MAX_PAGES: usize = 16;

#[derive(Deserialize)]
struct Page {
    // Required: a reply without them must not read as a period with no costs.
    data: Vec<Bucket>,
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct Bucket {
    /// RFC 3339 start of the UTC day.
    starting_at: String,
    #[serde(default)]
    results: Vec<CostResult>,
}

#[derive(Deserialize)]
struct CostResult {
    /// Lowest currency units (cents) as a decimal string: `"123.45"` is $1.23.
    amount: String,
    currency: String,
}

/// A configured grant during the replay.
#[derive(Debug)]
struct Grant {
    amount: i128,
    granted: String,
    expires: Option<String>,
    included: bool,
    remaining: i128,
}

impl Grant {
    fn active_on(&self, day: &str) -> bool {
        self.granted.as_str() <= day && self.expires.as_deref().is_none_or(|end| day < end)
    }
}

pub(super) fn fetch(config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
    let key = admin_key(config).map_err(|problem| FetchError::Setup {
        setup: setup_steps(config, &problem),
        message: problem.message,
    })?;
    let mut grants = grants(&config.anthropic_credits).map_err(|message| FetchError::Setup {
        message,
        setup: grant_steps(),
    })?;
    let first_day = grants
        .iter()
        .map(|grant| grant.granted.clone())
        .min()
        .ok_or_else(|| FetchError::Setup {
            message: "no [[usage.anthropic_credits]] grant is configured".into(),
            setup: grant_steps(),
        })?;
    let days = fetch_all(&format!("{first_day}T00:00:00Z"), &key)?;
    let uncovered = replay(&mut grants, &days)?;
    let today = today_utc(super::now_unix());
    let mut usage = ProviderUsage::pending("anthropic_api", "Anthropic · API credits");
    usage.account = Some(UsageAccount {
        source: format!(
            "file:{}",
            super::expand_home(config.anthropic_admin_key_file.trim()).display()
        ),
        ..UsageAccount::default()
    });
    usage.spend.extend(included_spend(&grants, &today));
    usage.balances.extend(other_balance(&grants, &today));
    for grant in &grants {
        usage.notes.push(grant_note(grant, &today));
    }
    if uncovered > 0 {
        usage.notes.push(format!(
            "US${} billed beyond the configured grants",
            cents(uncovered)
        ));
    }
    usage.notes.push(
        "replayed from the organization-wide cost report by UTC day; the Console balance is \
         the authority"
            .into(),
    );
    Ok(usage)
}

fn grants(configured: &[AnthropicCreditGrant]) -> Result<Vec<Grant>, String> {
    configured
        .iter()
        .map(|grant| {
            let granted = grant.granted.trim();
            day_start_unix(granted).ok_or_else(|| {
                format!("anthropic_credits: granted {granted:?} is not a YYYY-MM-DD day")
            })?;
            let expires = grant.expires.trim();
            let expires =
                (!expires.is_empty())
                    .then(|| {
                        day_start_unix(expires).map(|_| expires.to_owned()).ok_or_else(|| {
                        format!("anthropic_credits: expires {expires:?} is not a YYYY-MM-DD day")
                    })
                    })
                    .transpose()?;
            let amount = i128::from(grant.usd) * 100 * NANO_PER_CENT;
            Ok(Grant {
                amount,
                granted: granted.to_owned(),
                expires,
                included: grant.included,
                remaining: amount,
            })
        })
        .collect()
}

/// Draw each day's cost from the grants active that day: included credits
/// first, then the soonest to expire. Returns the cost no grant covered. A
/// negative day (a refund) undoes the latest draws first, into grants still
/// active that day, and lowers the uncovered cost before that.
fn replay(grants: &mut [Grant], days: &[(String, i128)]) -> Result<i128, String> {
    let mut uncovered = 0i128;
    // (grant, amount) per draw, newest last.
    let mut draws: Vec<(usize, i128)> = Vec::new();
    for (day, cost) in days {
        if *cost < 0 {
            let mut refund = -*cost;
            let take = refund.min(uncovered);
            uncovered -= take;
            refund -= take;
            while refund > 0 {
                let Some((index, amount)) = draws.pop() else {
                    break;
                };
                if !grants[index].active_on(day) {
                    continue;
                }
                let back = refund.min(amount);
                grants[index].remaining += back;
                refund -= back;
                if back < amount {
                    draws.push((index, amount - back));
                }
            }
            continue;
        }
        let mut order = (0..grants.len())
            .filter(|index| grants[*index].active_on(day))
            .collect::<Vec<_>>();
        order.sort_by(|a, b| {
            let (a, b) = (&grants[*a], &grants[*b]);
            b.included
                .cmp(&a.included)
                .then_with(|| match (&a.expires, &b.expires) {
                    (Some(a), Some(b)) => a.cmp(b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then_with(|| a.granted.cmp(&b.granted))
        });
        let mut left = *cost;
        for index in order {
            let take = left.min(grants[index].remaining);
            if take > 0 {
                grants[index].remaining -= take;
                draws.push((index, take));
                left -= take;
            }
        }
        uncovered = uncovered
            .checked_add(left)
            .ok_or_else(|| "Anthropic cost total overflowed".to_owned())?;
    }
    Ok(uncovered)
}

/// The newest active included grant as spent of granted.
fn included_spend(grants: &[Grant], today: &str) -> Option<UsageSpend> {
    let grant = grants
        .iter()
        .filter(|grant| grant.included && grant.active_on(today))
        .max_by(|a, b| a.granted.cmp(&b.granted))?;
    Some(UsageSpend {
        currency: "USD".into(),
        amount: dollars(grant.amount - grant.remaining),
        since: day_start_unix(&grant.granted).unwrap_or_default(),
        // Two decimals, so a whole grant reads `$200`.
        limit: Some(cents(grant.amount)),
        // Spending the grant does not prove Anthropic blocks requests: the
        // organization may hold other credits. The share used still colours it.
        limit_enforcing: false,
    })
}

/// What the active grants other than the shown included one have left.
fn other_balance(grants: &[Grant], today: &str) -> Option<UsageBalance> {
    let shown = grants
        .iter()
        .enumerate()
        .filter(|(_, grant)| grant.included && grant.active_on(today))
        .max_by(|(_, a), (_, b)| a.granted.cmp(&b.granted))
        .map(|(index, _)| index);
    let others = grants
        .iter()
        .enumerate()
        .filter(|(index, grant)| Some(*index) != shown && grant.active_on(today))
        .map(|(_, grant)| grant)
        .collect::<Vec<_>>();
    if others.is_empty() {
        return None;
    }
    Some(UsageBalance {
        currency: "USD".into(),
        total: cents(others.iter().map(|grant| grant.remaining).sum()),
        granted: Some(cents(others.iter().map(|grant| grant.amount).sum())),
        topped_up: None,
    })
}

fn grant_note(grant: &Grant, today: &str) -> String {
    let kind = if grant.included { "included" } else { "other" };
    let expires = grant
        .expires
        .as_deref()
        .map_or_else(String::new, |day| format!(", expires {day}"));
    let state = if grant.active_on(today) {
        format!("US${} left", cents(grant.remaining))
    } else {
        "not active".to_owned()
    };
    format!(
        "{kind} US${} granted {}{expires}: {state}",
        cents(grant.amount),
        grant.granted
    )
}

/// `YYYY-MM-DD` of a Unix time in UTC.
fn today_utc(now_unix: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(now_unix as i64).map_or_else(
        |_| String::new(),
        |now| {
            let date = now.date();
            format!(
                "{:04}-{:02}-{:02}",
                date.year(),
                u8::from(date.month()),
                date.day()
            )
        },
    )
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
        "At platform.claude.com create an Admin key (Settings, Admin keys) or, where that page \
         is missing, a service account with the Admin role (Settings, Service accounts) and a \
         key linked to it with the Organization scope (Settings, API keys)."
            .to_owned(),
        format!("Save it as one line in {path} on the machine running this herdr server."),
        format!("Make it private: chmod 600 {path}"),
    ];
    if problem.unsafe_file {
        steps.insert(0, problem.message.clone());
    }
    steps.push(
        "herdr only reads the cost report, but such a key can manage the whole organization."
            .to_owned(),
    );
    steps.join("\n")
}

fn grant_steps() -> String {
    [
        "Open platform.claude.com, Settings, Billing: Promotional credits and the invoice history.",
        "Add one [[usage.anthropic_credits]] table per grant still in use, with usd, granted",
        "and expires as the page shows them (YYYY-MM-DD, UTC); mark the plan's monthly",
        "API credits included = true. Add the new grant when the next one arrives.",
    ]
    .join("\n")
}

fn fetch_all(starting_at: &str, key: &str) -> Result<Vec<(String, i128)>, FetchError> {
    let mut days = Vec::new();
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
                    "the Anthropic key cannot read the cost report; it needs the Admin role and no workspace scope".into(),
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
        for bucket in page.data {
            let day = bucket
                .starting_at
                .get(..10)
                .filter(|day| day_start_unix(day).is_some())
                .ok_or_else(|| {
                    format!("unexpected Anthropic cost bucket {:?}", bucket.starting_at)
                })?;
            days.push((day.to_owned(), spent_nano_cents(&bucket.results)?));
        }
        match next_cursor(page.has_more, page.next_page, cursor.as_deref())? {
            Some(next) => cursor = Some(next),
            None => return Ok(days),
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

/// Billionths of a cent as dollars with two decimals, rounded half away from zero.
fn cents(nano_cents: i128) -> String {
    let half = if nano_cents < 0 {
        -NANO_PER_CENT / 2
    } else {
        NANO_PER_CENT / 2
    };
    let total = (nano_cents + half) / NANO_PER_CENT;
    let sign = if total < 0 { "-" } else { "" };
    let total = total.abs();
    format!("{sign}{}.{:02}", total / 100, total % 100)
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
                {"starting_at":"2026-10-13T00:00:00Z","results":[]},
                {"starting_at":"2026-10-14T00:00:00Z","results":[{"amount":"0.376543211","currency":"USD"}]}
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

    fn grant(usd: u32, granted: &str, expires: &str, included: bool) -> AnthropicCreditGrant {
        AnthropicCreditGrant {
            usd,
            granted: granted.into(),
            expires: expires.into(),
            included,
        }
    }

    fn day(day: &str, cents: i128) -> (String, i128) {
        (day.into(), cents * NANO_PER_CENT)
    }

    /// The Console of 2026-10-13: a $10 grant from Sep 27 that had paid
    /// $0.13 when the Max plan's $200 arrived on Oct 11, which then paid
    /// $76.92. The row reads `$76.92/$200 +$9.87`.
    #[test]
    fn included_credits_pay_first_and_the_others_keep_their_balance() {
        let mut grants = grants(&[
            grant(200, "2026-10-11", "2026-11-09", true),
            grant(10, "2026-09-27", "2027-09-28", false),
        ])
        .unwrap();
        let days = [
            day("2026-09-27", 3),
            day("2026-10-02", 10),
            day("2026-10-11", 2_000),
            day("2026-10-12", 4_000),
            day("2026-10-13", 1_692),
        ];
        assert_eq!(replay(&mut grants, &days), Ok(0));
        let spend = included_spend(&grants, "2026-10-13").unwrap();
        assert_eq!(
            (spend.amount.as_str(), spend.limit.as_deref()),
            ("76.920000", Some("200.00"))
        );
        assert_eq!(spend.since, 1_791_676_800);
        let other = other_balance(&grants, "2026-10-13").unwrap();
        assert_eq!(
            (other.total.as_str(), other.granted.as_deref()),
            ("9.87", Some("10.00"))
        );
    }

    #[test]
    fn spent_out_grants_pass_cost_on_and_the_rest_is_uncovered() {
        let mut grants = grants(&[
            grant(1, "2026-10-01", "2026-10-03", true),
            grant(2, "2026-10-01", "", false),
        ])
        .unwrap();
        // Day 1 empties the included dollar and takes 50 cents of the
        // other. The refunds undo the latest draws first: Oct 2 and the
        // first 30 cents of Oct 3 return the other's 50 cents; the last 20
        // reach the included grant's draw, which has expired by then, so
        // they are lost. Oct 4 spends the other's $2 and overflows by $2.
        let days = [
            day("2026-10-01", 150),
            day("2026-10-02", -20),
            day("2026-10-03", -50),
            day("2026-10-04", 400),
        ];
        assert_eq!(replay(&mut grants, &days), Ok(200 * NANO_PER_CENT));
        assert_eq!(grants[0].remaining, 0);
        assert_eq!(grants[1].remaining, 0);
        assert!(included_spend(&grants, "2026-10-04").is_none());
        assert_eq!(other_balance(&grants, "2026-10-04").unwrap().total, "0.00");
    }

    #[test]
    fn a_grant_counts_from_its_day_until_the_day_it_expires() {
        let grants = grants(&[grant(5, "2026-10-11", "2026-11-09", true)]).unwrap();
        assert!(!grants[0].active_on("2026-10-10"));
        assert!(grants[0].active_on("2026-10-11"));
        assert!(grants[0].active_on("2026-11-08"));
        assert!(!grants[0].active_on("2026-11-09"));
        assert!(super::grants(&[grant(5, "2026-10", "", false)]).is_err());
        assert!(super::grants(&[grant(5, "2026-10-11", "never", false)]).is_err());
        assert_eq!(today_utc(1_791_763_200), "2026-10-12");
        assert_eq!(cents(987 * NANO_PER_CENT + NANO_PER_CENT / 2), "9.88");
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
        assert!(steps.starts_with("At platform.claude.com create an Admin key"));
        assert!(steps.contains("chmod 600 ~/.config/herdr/anthropic-admin-key"));
        let shared = KeyProblem {
            message: "readable by other users".into(),
            unsafe_file: true,
        };
        assert!(setup_steps(&config, &shared).starts_with("readable by other users\n"));
    }
}
