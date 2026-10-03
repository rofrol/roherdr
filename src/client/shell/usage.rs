//! Sidebar usage footer: polls `usage.read` and renders provider allowance.

use std::time::{Duration, Instant};

use crate::api::schema::{
    ProviderUsage, ProviderUsageStatus, UsageReport, UsageResetCredit, UsageWindow,
};

use super::render::{display_width, put_segment, put_text};
use super::*;

/// `usage.read` only returns the endpoint's cached report, so polling it is cheap.
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// A request older than this is treated as lost so polling cannot stall.
const USAGE_REQUEST_STALE_AFTER: Duration = Duration::from_secs(60);
/// Keep room for the agents header and a few rows before showing the footer.
const AGENTS_MIN_HEIGHT: u16 = 5;
/// Columns kept free at the right of the footer's last row for the sidebar toggle.
const SIDEBAR_TOGGLE_RESERVE: u16 = 2;
/// A reset credit expiring sooner than this is highlighted.
pub(super) const RESET_CREDIT_WARNING_SECS: u64 = 48 * 3_600;
/// The footer flags an expiring reset credit only once a window is used this much;
/// below it, redeeming the credit would recover little.
const RESET_CREDIT_WORTH_PERCENT: u8 = 50;

#[derive(Debug, Default)]
pub(super) struct ClientUsageState {
    pub(super) report: Option<UsageReport>,
    report_endpoint: Option<ClientEndpointId>,
    in_flight_since: Option<Instant>,
    next_poll: Option<Instant>,
    unsupported_boot: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct ClientUsageOverlay {
    pub(super) report: Option<UsageReport>,
    pub(super) refreshing: bool,
    /// Local UTC offset captured when the overlay opened, for reset clock times.
    pub(super) utc_offset_secs: i64,
}

impl ClientShellState {
    /// Report for the active endpoint when it has usage polling enabled.
    pub(super) fn active_usage_report(&self) -> Option<&UsageReport> {
        active_report(&self.usage, &self.active_endpoint_id)
    }

    pub(crate) fn tick_usage(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        if self
            .usage
            .in_flight_since
            .is_some_and(|since| now.duration_since(since) < USAGE_REQUEST_STALE_AFTER)
        {
            return;
        }
        let endpoint_changed =
            self.usage.report_endpoint.as_ref() != Some(&self.active_endpoint_id);
        if !endpoint_changed && self.usage.next_poll.is_some_and(|next| now < next) {
            return;
        }
        self.request_usage(now, false, outcome);
    }

    /// Queue `usage.read` without surfacing notices; background polling must stay silent.
    fn request_usage(&mut self, now: Instant, refresh: bool, outcome: &mut ClientShellInput) {
        self.usage.next_poll = Some(now + USAGE_POLL_INTERVAL);
        let endpoint_id = self.active_endpoint_id.clone();
        if !self.endpoint_is_online(&endpoint_id) {
            return;
        }
        let boot_id = self.endpoint_boot_id(&endpoint_id).map(str::to_owned);
        if boot_id.is_some() && self.usage.unsupported_boot == boot_id {
            return;
        }
        let method =
            crate::api::schema::Method::UsageRead(crate::api::schema::UsageReadParams { refresh });
        if !self.supports_endpoint_method(&method) {
            return;
        }
        if self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::UsageRead {
                endpoint_id: endpoint_id.clone(),
            },
            outcome,
        ) {
            self.usage.in_flight_since = Some(now);
        }
    }

    pub(super) fn complete_usage_read(
        &mut self,
        endpoint_id: ClientEndpointId,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        self.usage.in_flight_since = None;
        let report = match result {
            Ok(crate::api::schema::ResponseResult::UsageRead { usage }) => Some(usage),
            Ok(_) => None,
            Err(error) => {
                if error.code.as_deref() == Some("unsupported_method") {
                    self.usage.unsupported_boot =
                        self.endpoint_boot_id(&endpoint_id).map(str::to_owned);
                }
                None
            }
        };
        if let Some(ClientShellOverlay::Usage(overlay)) = self.overlay.as_mut() {
            overlay.refreshing = false;
            if endpoint_id == self.active_endpoint_id {
                overlay.report = report.clone();
            }
        }
        let changed = self.usage.report != report
            || self.usage.report_endpoint.as_ref() != Some(&endpoint_id);
        self.usage.report = report;
        self.usage.report_endpoint = Some(endpoint_id);
        (changed, Vec::new())
    }

    pub(super) fn open_usage_overlay(&mut self, outcome: &mut ClientShellInput) {
        self.overlay = Some(ClientShellOverlay::Usage(ClientUsageOverlay {
            report: self.active_usage_report().cloned(),
            refreshing: false,
            utc_offset_secs: local_utc_offset_secs(),
        }));
        self.refresh_usage(outcome);
        outcome.repaint = true;
    }

    /// Ask the endpoint to re-fetch providers now, then poll again shortly after.
    pub(super) fn refresh_usage(&mut self, outcome: &mut ClientShellInput) {
        let now = Instant::now();
        self.usage.in_flight_since = None;
        self.request_usage(now, true, outcome);
        if self.usage.in_flight_since.is_some() {
            if let Some(ClientShellOverlay::Usage(overlay)) = self.overlay.as_mut() {
                overlay.refreshing = true;
            }
            // Fetches finish in the background; pick up the results soon.
            self.usage.next_poll = Some(now + Duration::from_secs(5));
        }
        outcome.repaint = true;
    }
}

pub(super) fn local_utc_offset_secs() -> i64 {
    let Some(local) = crate::platform::local_datetime() else {
        return 0;
    };
    let offset = local.assume_utc().unix_timestamp() - crate::usage::now_unix() as i64;
    // Round away the seconds that elapsed between the two clock reads.
    (offset as f64 / 900.0).round() as i64 * 900
}

/// `Wed 15:29` in the captured local offset.
pub(super) fn reset_clock(resets_at: u64, utc_offset_secs: i64) -> Option<String> {
    let local = time::OffsetDateTime::from_unix_timestamp(
        i64::try_from(resets_at)
            .ok()?
            .checked_add(utc_offset_secs)?,
    )
    .ok()?;
    let weekday = local.weekday().to_string();
    Some(format!(
        "{} {:02}:{:02}",
        weekday.get(..3).unwrap_or(&weekday),
        local.hour(),
        local.minute()
    ))
}

/// [`reset_clock`] within six days, else `Oct 22 20:51`, so a weekday is never ambiguous.
pub(super) fn expiry_clock(at: u64, now_unix: u64, utc_offset_secs: i64) -> Option<String> {
    if at.saturating_sub(now_unix) < 6 * 86_400 {
        return reset_clock(at, utc_offset_secs);
    }
    let local = time::OffsetDateTime::from_unix_timestamp(
        i64::try_from(at).ok()?.checked_add(utc_offset_secs)?,
    )
    .ok()?;
    let month = local.month().to_string();
    Some(format!(
        "{} {} {:02}:{:02}",
        month.get(..3).unwrap_or(&month),
        local.day(),
        local.hour(),
        local.minute()
    ))
}

/// Reset credits that have not expired yet; a cached report can hold expired ones.
pub(super) fn live_reset_credits(
    provider: &ProviderUsage,
    now_unix: u64,
) -> impl Iterator<Item = &UsageResetCredit> {
    provider
        .reset_credits
        .iter()
        .filter(move |credit| credit.expires_at.is_none_or(|at| at > now_unix))
}

/// Expiry of a reset credit worth redeeming before it is lost: it expires
/// within [`RESET_CREDIT_WARNING_SECS`], before the weekly window resets on
/// its own, while a footer window is at least [`RESET_CREDIT_WORTH_PERCENT`] used.
fn expiring_reset_credit(provider: &ProviderUsage, now_unix: u64) -> Option<u64> {
    let expires_at = live_reset_credits(provider, now_unix)
        .filter_map(|credit| credit.expires_at)
        .min()?;
    if expires_at.saturating_sub(now_unix) > RESET_CREDIT_WARNING_SECS {
        return None;
    }
    let (short, weekly) = footer_windows(provider);
    if weekly
        .and_then(|weekly| weekly.resets_at)
        .is_some_and(|resets_at| resets_at <= expires_at)
    {
        return None;
    }
    let used = [short, weekly]
        .into_iter()
        .flatten()
        .map(|window| window.used_percent)
        .max()?;
    (used >= RESET_CREDIT_WORTH_PERCENT).then_some(expires_at)
}

/// Borrow-friendly form of [`ClientShellState::active_usage_report`] for render state.
pub(super) fn active_report<'a>(
    usage: &'a ClientUsageState,
    active_endpoint_id: &ClientEndpointId,
) -> Option<&'a UsageReport> {
    (usage.report_endpoint.as_ref() == Some(active_endpoint_id))
        .then_some(usage.report.as_ref())
        .flatten()
        .filter(|report| report.enabled)
}

/// Split the sidebar detail section into the agents panel and the usage footer.
pub(super) fn split_usage_footer(detail_area: Rect, report: Option<&UsageReport>) -> (Rect, Rect) {
    let Some(height) = report
        .map(footer_height)
        .filter(|height| *height > 1 && detail_area.height >= height + AGENTS_MIN_HEIGHT)
    else {
        return (detail_area, Rect::default());
    };
    let agents = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height - height,
    );
    let footer = Rect::new(detail_area.x, agents.bottom(), detail_area.width, height);
    (agents, footer)
}

/// Without the agents panel: the spaces over the whole height, less the usage
/// footer at the bottom (empty when there is no report or no room).
pub(super) fn split_spaces_and_footer(
    sections: Rect,
    report: Option<&UsageReport>,
) -> (Rect, Rect) {
    let content = Rect::new(
        sections.x,
        sections.y,
        sections.width.saturating_sub(1),
        sections.height,
    );
    let height = report
        .map(footer_height)
        .filter(|height| *height > 1 && content.height >= height + AGENTS_MIN_HEIGHT)
        .unwrap_or(0);
    let spaces = Rect::new(content.x, content.y, content.width, content.height - height);
    let footer = Rect::new(content.x, spaces.bottom(), content.width, height);
    (spaces, footer)
}

/// Separator row plus one row per provider.
fn footer_height(report: &UsageReport) -> u16 {
    u16::try_from(report.providers.len())
        .unwrap_or(u16::MAX)
        .saturating_add(1)
}

/// Column of the two-letter provider code, and of the short and weekly window cells.
const CODE_COLUMN: u16 = 1;
const SHORT_WINDOW_COLUMN: u16 = 4;
const WEEKLY_WINDOW_COLUMN: u16 = 14;

pub(super) fn render_usage_footer(
    buffer: &mut Buffer,
    area: Rect,
    report: &UsageReport,
    now_unix: u64,
    palette: &Palette,
    hits: &mut ShellHitMap,
) {
    if area.height < 2 || area.width == 0 {
        return;
    }
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(palette.surface_dim),
    );
    let content_width = area.width.saturating_sub(SIDEBAR_TOGGLE_RESERVE);
    hits.usage_footer = Rect::new(area.x, area.y, content_width, area.height);

    for (row, provider) in (area.y + 1..area.bottom()).zip(&report.providers) {
        render_provider_row(
            buffer,
            Rect::new(area.x, row, content_width, 1),
            provider,
            now_unix,
            palette,
        );
        hits.tooltips
            .push(provider_code_tooltip(area.x, row, provider));
    }
}

/// Hovering a row's code (and its `!` mark) names the vendor: `AN Anthropic`.
/// The details modal shows the same names, so hover is never the only way.
fn provider_code_tooltip(
    x: u16,
    row: u16,
    provider: &ProviderUsage,
) -> super::tooltip::TooltipTarget {
    let code = provider_code(provider);
    let mut text = format!("{code} {}", provider_name(provider));
    if matches!(
        provider.status,
        ProviderUsageStatus::Error | ProviderUsageStatus::Unknown
    ) {
        text.push_str(" · refresh failed");
    }
    super::tooltip::TooltipTarget {
        // The code and the `!` mark after it.
        rect: Rect::new(x.saturating_add(CODE_COLUMN), row, 3, 1),
        // Per provider, not per code: two rows share `OA`.
        id: format!("usage:{}", provider.provider),
        text,
        bg: None,
    }
}

fn render_provider_row(
    buffer: &mut Buffer,
    row: Rect,
    provider: &ProviderUsage,
    now_unix: u64,
    palette: &Palette,
) {
    let failed = matches!(
        provider.status,
        ProviderUsageStatus::Error | ProviderUsageStatus::Unknown
    );
    let x = put_segment(
        buffer,
        row.x.saturating_add(CODE_COLUMN),
        row.y,
        row.right(),
        &provider_code(provider),
        Style::default()
            .fg(palette.overlay1)
            .add_modifier(Modifier::BOLD),
    );
    if failed {
        put_segment(
            buffer,
            x,
            row.y,
            row.right(),
            "!",
            Style::default().fg(palette.red),
        );
    }
    let short_x = row.x.saturating_add(SHORT_WINDOW_COLUMN);
    let (short, weekly) = footer_windows(provider);
    if short.is_some() || weekly.is_some() {
        let mut end = short_x;
        for (column, window) in [(SHORT_WINDOW_COLUMN, short), (WEEKLY_WINDOW_COLUMN, weekly)] {
            let Some(window) = window else {
                continue;
            };
            end = put_segment(
                buffer,
                row.x.saturating_add(column),
                row.y,
                row.right(),
                &format!("{:>3}%", window.used_percent),
                Style::default().fg(used_color(window.used_percent, palette)),
            );
            if let Some(resets_at) = window.resets_at {
                end = put_segment(
                    buffer,
                    end,
                    row.y,
                    row.right(),
                    &format!(" {}", compact_countdown(resets_at, now_unix)),
                    Style::default().fg(palette.overlay0),
                );
            }
        }
        render_reset_credit_expiry(buffer, row, end, provider, now_unix, palette);
    } else if let Some(balance) = provider.balances.first() {
        // Prepaid balances have no reset window; label them so they do not read as a 5h cell.
        let x = put_segment(
            buffer,
            short_x,
            row.y,
            row.right(),
            &compact_balance(&balance.total, &balance.currency),
            Style::default().fg(palette.text),
        );
        put_segment(
            buffer,
            x,
            row.y,
            row.right(),
            " balance",
            Style::default().fg(palette.overlay0),
        );
    } else if let Some(spend) = provider.spend.first() {
        // Pay-as-you-go spend is money already used, not credit left. With a
        // limit it reads `$4.20/$20`: money first, never a bare percentage
        // that would look like a subscription window.
        let (text, color) = match spend_limit_percent(spend) {
            Some(percent) => (
                format!(
                    "{}/{}",
                    compact_spend(&spend.amount, &spend.currency),
                    compact_limit(spend.limit.as_deref().unwrap_or_default(), &spend.currency)
                ),
                if spend.limit_enforcing {
                    palette.red
                } else {
                    used_color(percent, palette)
                },
            ),
            None => (compact_spend(&spend.amount, &spend.currency), palette.text),
        };
        let x = put_segment(
            buffer,
            short_x,
            row.y,
            row.right(),
            &text,
            Style::default().fg(color),
        );
        put_segment(
            buffer,
            x,
            row.y,
            row.right(),
            " spend",
            Style::default().fg(palette.overlay0),
        );
    } else if !failed {
        let placeholder = if provider.status == ProviderUsageStatus::Pending {
            "…"
        } else {
            "?"
        };
        put_segment(
            buffer,
            short_x,
            row.y,
            row.right(),
            placeholder,
            Style::default().fg(palette.overlay0),
        );
    }
}

/// Countdown to the soonest reset credit's expiry, right-aligned so it lines up
/// across rows: `exp1d20h`, else `e1d20h`, else `e1d`, else nothing. `exp` keeps it from
/// reading as a third reset countdown. Yellow when the credit is worth redeeming.
fn render_reset_credit_expiry(
    buffer: &mut Buffer,
    row: Rect,
    end: u16,
    provider: &ProviderUsage,
    now_unix: u64,
    palette: &Palette,
) {
    let Some(expires_at) = live_reset_credits(provider, now_unix)
        .filter_map(|credit| credit.expires_at)
        .min()
    else {
        return;
    };
    let countdown = compact_countdown(expires_at, now_unix);
    // Keep a space after the windows; a clipped countdown would misreport the expiry.
    let room = row.right().saturating_sub(end.saturating_add(1));
    // The first unit alone is floored, so it never shows more time than is left.
    let first_unit = countdown_parts(expires_at, now_unix).0;
    let Some(text) = [
        format!("exp{countdown}"),
        format!("e{countdown}"),
        format!("e{first_unit}"),
    ]
    .into_iter()
    .find(|text| display_width(text) <= room) else {
        return;
    };
    let color = if expiring_reset_credit(provider, now_unix).is_some() {
        palette.yellow
    } else {
        palette.overlay0
    };
    put_segment(
        buffer,
        row.right().saturating_sub(display_width(&text)),
        row.y,
        row.right(),
        &text,
        Style::default().fg(color),
    );
}

/// The 5-hour and weekly windows, falling back to the first two windows reported.
/// A provider with a weekly window but no 5-hour one leaves the short column empty.
/// Model-specific windows never take a cell: showing one here would silently
/// change what the cell means. Filter lazily to avoid allocations during render.
fn footer_windows(provider: &ProviderUsage) -> (Option<&UsageWindow>, Option<&UsageWindow>) {
    let shared = || {
        provider.windows.iter().filter(|window| {
            !window.id.starts_with("scoped:")
                && !matches!(window.id.as_str(), "weekly_opus" | "weekly_sonnet")
        })
    };
    let by_id = |id: &str| shared().find(|window| window.id == id);
    let short =
        by_id("five_hour").or_else(|| by_id("weekly").is_none().then(|| shared().next()).flatten());
    let weekly = by_id("weekly").or_else(|| {
        shared().find(|window| short.is_none_or(|short| !std::ptr::eq(*window, short)))
    });
    (short, weekly)
}

pub(super) fn provider_code(provider: &ProviderUsage) -> String {
    match provider.provider.as_str() {
        "claude" => "AN".into(),
        // Codes name vendors: Codex limits and API spend are both OpenAI.
        "codex" | "openai_api" => "OA".into(),
        "gemini" => "GO".into(),
        "deepseek" => "DS".into(),
        "openrouter" => "OR".into(),
        "kimi" => "KM".into(),
        _ => provider
            .label
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

/// Vendor behind a row's code, with the product where one vendor has two rows.
pub(super) fn provider_name(provider: &ProviderUsage) -> String {
    match provider.provider.as_str() {
        "claude" => "Anthropic".into(),
        "codex" => "OpenAI · Codex".into(),
        "openai_api" => "OpenAI · API spend".into(),
        "gemini" => "Google".into(),
        "deepseek" => "DeepSeek".into(),
        "openrouter" => "OpenRouter".into(),
        "kimi" => "Moonshot · Kimi".into(),
        _ => provider.label.clone(),
    }
}

pub(super) fn used_color(used_percent: u8, palette: &Palette) -> ratatui::style::Color {
    match used_percent {
        0..=49 => palette.green,
        50..=79 => palette.yellow,
        _ => palette.red,
    }
}

/// The two most significant units of a countdown: days and hours from a day
/// up, hours and minutes from an hour up, else minutes. Units are floored and
/// a zero second unit is left out (`24h 30m` is `1d`). A positive time under
/// a minute is `<1m`; a reset that has passed is `now`.
fn countdown_parts(resets_at: u64, now_unix: u64) -> (String, Option<String>) {
    let seconds = resets_at.saturating_sub(now_unix);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
    );
    let second = |value: u64, unit: &str| (value > 0).then(|| format!("{value}{unit}"));
    if seconds == 0 {
        ("now".into(), None)
    } else if days > 0 {
        (format!("{days}d"), second(hours, "h"))
    } else if hours > 0 {
        (format!("{hours}h"), second(minutes, "m"))
    } else if minutes > 0 {
        (format!("{minutes}m"), None)
    } else {
        ("<1m".into(), None)
    }
}

/// `3d`, `1d10h`, `2h15m`, `45m`, `<1m` or `now`: without a space, and with
/// the second unit dropped once the first has two digits (`23h59m` is `23h`,
/// `12d3h` is `12d`), so a cell in the footer fits five columns.
pub(super) fn compact_countdown(resets_at: u64, now_unix: u64) -> String {
    match countdown_parts(resets_at, now_unix) {
        (first, Some(second)) if first.chars().count() <= 2 => format!("{first}{second}"),
        (first, _) => first,
    }
}

/// `2d 4h`, `3h 20m`, `12m`, `<1m` or `now`, for the usage modal.
pub(super) fn detailed_countdown(resets_at: u64, now_unix: u64) -> String {
    match countdown_parts(resets_at, now_unix) {
        (first, Some(second)) => format!("{first} {second}"),
        (first, None) => first,
    }
}

pub(super) fn compact_balance(total: &str, currency: &str) -> String {
    let amount = total
        .parse::<f64>()
        .ok()
        .filter(|amount| amount.is_finite())
        .map_or_else(
            || total.to_owned(),
            |amount| {
                if amount >= 100.0 {
                    format!("{amount:.0}")
                } else {
                    format!("{amount:.2}")
                }
            },
        );
    match currency {
        "USD" => format!("${amount}"),
        "CNY" => format!("¥{amount}"),
        "EUR" => format!("€{amount}"),
        _ => format!("{amount} {currency}"),
    }
}

/// Spend rounded to cents; a sub-cent spend reads `<$0.01`, not `$0.00`.
pub(super) fn compact_spend(amount: &str, currency: &str) -> String {
    match amount.parse::<f64>() {
        Ok(value) if value > 0.0 && value < 0.005 => {
            format!("<{}", compact_balance("0.01", currency))
        }
        _ => compact_balance(amount, currency),
    }
}

/// Share of the spend limit used, when the provider reports a positive limit.
pub(super) fn spend_limit_percent(spend: &crate::api::schema::UsageSpend) -> Option<u8> {
    let limit = spend.limit.as_deref()?.parse::<f64>().ok()?;
    let amount = spend.amount.parse::<f64>().ok()?;
    let percent = amount / limit * 100.0;
    (limit.is_finite() && limit > 0.0 && percent.is_finite())
        .then(|| percent.round().clamp(0.0, 100.0) as u8)
}

/// A limit without cents when it is whole: `$20`, else `$20.50`.
pub(super) fn compact_limit(limit: &str, currency: &str) -> String {
    let whole = limit.strip_suffix(".00").unwrap_or(limit);
    format_balance(whole, currency)
}

/// Token count such as `950`, `12.3k` or `4.1M`.
pub(super) fn compact_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1_000..=999_999 => format!("{:.1}k", count as f64 / 1_000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// `Oct 1 UTC` for a period start in Unix seconds.
pub(super) fn utc_day(unix: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix as i64).map_or_else(
        |_| "?".into(),
        |at| {
            let month = at.month().to_string();
            format!("{} {} UTC", &month[..3.min(month.len())], at.day())
        },
    )
}

pub(super) fn format_balance(total: &str, currency: &str) -> String {
    match currency {
        "USD" => format!("${total}"),
        "CNY" => format!("¥{total}"),
        "EUR" => format!("€{total}"),
        _ => format!("{total} {currency}"),
    }
}

/// Age of an observation such as `just now` or `4m ago`.
pub(super) fn observed_age(observed_at: u64, now_unix: u64) -> String {
    let seconds = now_unix.saturating_sub(observed_at);
    if seconds < 60 {
        "just now".into()
    } else {
        format!("{} ago", compact_countdown(now_unix, observed_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{UsageBalance, UsageWindow};

    fn provider(id: &str, label: &str) -> ProviderUsage {
        let mut usage = ProviderUsage::pending(id, label);
        usage.status = ProviderUsageStatus::Ok;
        usage
    }

    fn window(used_percent: u8, resets_at: u64) -> UsageWindow {
        UsageWindow {
            id: "w".into(),
            label: "w".into(),
            used_percent,
            resets_at: Some(resets_at),
        }
    }

    fn footer_text(report: &UsageReport, width: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, footer_height(report));
        let mut buffer = Buffer::empty(area);
        let mut hits = ShellHitMap::default();
        render_usage_footer(
            &mut buffer,
            area,
            report,
            1_000,
            &Palette::catppuccin(),
            &mut hits,
        );
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn windowed(id: &str, label: &str, short: u8, weekly: u8) -> ProviderUsage {
        let mut usage = provider(id, label);
        usage.windows = vec![
            UsageWindow {
                id: "five_hour".into(),
                label: "5h".into(),
                used_percent: short,
                resets_at: Some(1_000 + 47 * 60),
            },
            UsageWindow {
                id: "weekly".into(),
                label: "week".into(),
                used_percent: weekly,
                resets_at: Some(1_000 + 6 * 86_400),
            },
        ];
        usage
    }

    #[test]
    fn footer_shows_short_and_weekly_usage_per_provider() {
        let mut deepseek = provider("deepseek", "DeepSeek");
        deepseek.balances.push(UsageBalance {
            currency: "USD".into(),
            total: "13.41".into(),
            granted: None,
            topped_up: None,
        });
        let report = UsageReport {
            enabled: true,
            providers: vec![
                windowed("claude", "Claude", 2, 12),
                windowed("codex", "Codex", 87, 100),
                deepseek,
            ],
        };

        let rows = footer_text(&report, 26);

        assert_eq!(rows[0], "─".repeat(26));
        assert_eq!(rows[1], " AN   2% 47m   12% 6d");
        assert_eq!(rows[2], " OA  87% 47m  100% 6d");
        assert_eq!(rows[3], " DS $13.41 balance");
    }

    fn with_credit(mut usage: ProviderUsage, expires_at: u64) -> ProviderUsage {
        usage.reset_credits.push(UsageResetCredit {
            id: "credit".into(),
            kind: "codexRateLimits".into(),
            title: "Full reset".into(),
            expires_at: Some(expires_at),
        });
        usage
    }

    #[test]
    fn footer_shows_when_the_first_reset_credit_expires() {
        let expires_at = 1_000 + 30 * 3_600;
        let row = |usage: ProviderUsage, width: u16| {
            let report = UsageReport {
                enabled: true,
                providers: vec![usage],
            };
            footer_text(&report, width).remove(1)
        };
        let style_at = |usage: ProviderUsage, width: u16, column: u16| {
            let report = UsageReport {
                enabled: true,
                providers: vec![usage],
            };
            let area = Rect::new(0, 0, width, footer_height(&report));
            let mut buffer = Buffer::empty(area);
            render_usage_footer(
                &mut buffer,
                area,
                &report,
                1_000,
                &Palette::catppuccin(),
                &mut ShellHitMap::default(),
            );
            buffer[(column, 1)].fg
        };
        let palette = Palette::catppuccin();

        // Right-aligned to the 30 usable columns, before the sidebar toggle.
        let busy = || with_credit(windowed("codex", "Codex", 87, 60), expires_at);
        assert_eq!(row(busy(), 32), " OA  87% 47m   60% 6d  exp1d6h");
        assert_eq!(style_at(busy(), 32, 29), palette.yellow);
        assert_eq!(row(busy(), 30), " OA  87% 47m   60% 6d  e1d6h");
        assert_eq!(row(busy(), 28), " OA  87% 47m   60% 6d  e1d");
        assert_eq!(row(busy(), 26), " OA  87% 47m   60% 6d");

        // Little is used, so it is shown but not highlighted.
        let idle = || with_credit(windowed("codex", "Codex", 20, 40), expires_at);
        assert_eq!(row(idle(), 32), " OA  20% 47m   40% 6d  exp1d6h");
        assert_eq!(style_at(idle(), 32, 29), palette.overlay0);
        // The weekly window resets on its own before the credit expires.
        let mut early_weekly = windowed("codex", "Codex", 87, 60);
        early_weekly.windows[1].resets_at = Some(1_000 + 3_600);
        assert_eq!(
            style_at(with_credit(early_weekly, expires_at), 32, 29),
            palette.overlay0
        );
        // Further away than the warning period.
        let later = with_credit(windowed("codex", "Codex", 87, 60), 1_000 + 3 * 86_400);
        assert_eq!(row(later.clone(), 32), " OA  87% 47m   60% 6d    exp3d");
        assert_eq!(style_at(later, 32, 29), palette.overlay0);
        // Already expired; a cached report can still carry it.
        assert_eq!(
            row(with_credit(windowed("codex", "Codex", 87, 60), 999), 32),
            " OA  87% 47m   60% 6d"
        );
    }

    #[test]
    fn footer_keeps_the_sidebar_toggle_column_free() {
        let report = UsageReport {
            enabled: true,
            providers: vec![windowed("claude", "Claude", 100, 100)],
        };

        let rows = footer_text(&report, 20);

        assert!(rows[1].chars().count() <= 18, "{:?}", rows[1]);
    }

    #[test]
    fn failed_provider_is_marked() {
        let mut claude = provider("claude", "Claude");
        claude.status = ProviderUsageStatus::Error;
        let report = UsageReport {
            enabled: true,
            providers: vec![claude],
        };
        assert_eq!(footer_text(&report, 24)[1], " AN!");
    }

    #[test]
    fn spend_against_a_limit_is_a_clamped_percent_and_a_whole_limit_drops_cents() {
        let mut spend = crate::api::schema::UsageSpend {
            currency: "USD".into(),
            amount: "4.20".into(),
            since: 0,
            limit: Some("20.00".into()),
            limit_enforcing: false,
        };
        assert_eq!(spend_limit_percent(&spend), Some(21));
        assert_eq!(compact_limit("20.00", "USD"), "$20");
        assert_eq!(compact_limit("20.50", "USD"), "$20.50");
        spend.amount = "25".into();
        assert_eq!(spend_limit_percent(&spend), Some(100));
        spend.limit = Some("0.00".into());
        assert_eq!(spend_limit_percent(&spend), None);
        spend.limit = None;
        assert_eq!(spend_limit_percent(&spend), None);
    }

    #[test]
    fn hovering_a_code_names_its_vendor_per_row() {
        let tip = provider_code_tooltip(10, 5, &provider("claude", "Claude"));
        assert_eq!(tip.text, "AN Anthropic");
        assert_eq!(tip.rect, Rect::new(11, 5, 3, 1));
        let codex = provider_code_tooltip(10, 6, &provider("codex", "Codex"));
        let api = provider_code_tooltip(10, 7, &provider("openai_api", "OpenAI API"));
        assert_eq!(codex.text, "OA OpenAI · Codex");
        assert_eq!(api.text, "OA OpenAI · API spend");
        assert_ne!(codex.id, api.id);
        let mut failed = provider("mistral", "Mistral");
        failed.status = ProviderUsageStatus::Error;
        assert_eq!(
            provider_code_tooltip(0, 0, &failed).text,
            "MI Mistral · refresh failed"
        );
    }

    #[test]
    fn openai_api_spend_shares_the_openai_vendor_code() {
        assert_eq!(provider_code(&provider("codex", "Codex")), "OA");
        assert_eq!(provider_code(&provider("openai_api", "OpenAI API")), "OA");
    }

    #[test]
    fn unknown_provider_code_uses_its_label() {
        assert_eq!(provider_code(&provider("mistral", "Mistral")), "MI");
        assert_eq!(provider_code(&provider("x", "")), "");
    }

    #[test]
    fn single_window_fills_the_short_column() {
        let mut other = provider("other", "Other");
        other.windows.push(window(40, 1_000 + 3_600));
        let (short, weekly) = footer_windows(&other);
        assert!(short.is_some());
        assert!(weekly.is_none());
    }

    #[test]
    fn model_scoped_windows_never_take_a_footer_cell() {
        let mut scoped = provider("claude", "Claude");
        let mut only_scoped = window(90, 1_000 + 3_600);
        only_scoped.id = "scoped:fable".into();
        only_scoped.label = "Fable week".into();
        for id in ["scoped:fable", "weekly_opus", "weekly_sonnet"] {
            let mut model_window = only_scoped.clone();
            model_window.id = id.into();
            scoped.windows = vec![model_window.clone()];
            let (short, weekly) = footer_windows(&scoped);
            assert!(short.is_none() && weekly.is_none(), "{id}");

            let mut session = window(4, 1_000 + 3_600);
            session.id = "five_hour".into();
            scoped.windows = vec![session, model_window];
            let (short, weekly) = footer_windows(&scoped);
            assert_eq!(short.map(|window| window.used_percent), Some(4));
            assert!(weekly.is_none(), "{id}");
        }

        scoped.windows = vec![
            UsageWindow {
                id: "five_hour".into(),
                label: "5h".into(),
                used_percent: 4,
                resets_at: None,
            },
            UsageWindow {
                id: "weekly".into(),
                label: "week".into(),
                used_percent: 100,
                resets_at: None,
            },
            only_scoped,
        ];
        let (short, weekly) = footer_windows(&scoped);
        assert_eq!(short.map(|window| window.used_percent), Some(4));
        assert_eq!(weekly.map(|window| window.used_percent), Some(100));
    }

    #[test]
    fn weekly_only_windows_leave_the_short_column_empty() {
        let mut gemini = provider("gemini", "Gemini");
        for (id, used) in [("weekly", 53), ("weekly_3p", 0)] {
            let mut window = window(used, 1_000 + 86_400);
            window.id = id.into();
            gemini.windows.push(window);
        }
        let (short, weekly) = footer_windows(&gemini);
        assert!(short.is_none());
        assert_eq!(weekly.map(|window| window.used_percent), Some(53));
    }

    #[test]
    fn footer_is_hidden_without_providers_or_room() {
        let empty = UsageReport {
            enabled: true,
            providers: Vec::new(),
        };
        let area = Rect::new(0, 0, 20, 30);
        assert_eq!(split_usage_footer(area, Some(&empty)).1, Rect::default());
        assert_eq!(split_usage_footer(area, None).1, Rect::default());

        let report = UsageReport {
            enabled: true,
            providers: vec![provider("claude", "Claude"), provider("codex", "Codex")],
        };
        let (agents, footer) = split_usage_footer(area, Some(&report));
        assert_eq!(footer, Rect::new(0, 27, 20, 3));
        assert_eq!(agents.height, 27);
        let short = Rect::new(0, 0, 20, AGENTS_MIN_HEIGHT + 2);
        assert_eq!(split_usage_footer(short, Some(&report)).1, Rect::default());
    }

    #[test]
    fn countdowns_pick_a_readable_unit() {
        let (minute, hour, day) = (60, 3_600, 86_400);
        let compact = |seconds: u64| compact_countdown(1_000 + seconds, 1_000);
        let detailed = |seconds: u64| detailed_countdown(1_000 + seconds, 1_000);
        // A reset that has passed, and one under a minute away.
        assert_eq!(compact_countdown(500, 1_000), "now");
        assert_eq!(compact(0), "now");
        assert_eq!(compact(30), "<1m");
        assert_eq!(detailed(59), "<1m");
        assert_eq!(compact(25 * minute), "25m");
        assert_eq!(compact(45 * minute + 59), "45m");
        // From an hour: hours and minutes, a zero minute left out.
        assert_eq!(compact(hour), "1h");
        assert_eq!(compact(2 * hour + 15 * minute), "2h15m");
        assert_eq!(detailed(2 * hour + 15 * minute), "2h 15m");
        assert_eq!(compact(23 * hour + 59 * minute), "23h");
        assert_eq!(compact(9 * hour + 59 * minute), "9h59m");
        assert_eq!(compact(12 * day + 3 * hour), "12d");
        // From a day: days and hours; minutes are dropped, and `24h 30m` is a day.
        assert_eq!(compact(24 * hour), "1d");
        assert_eq!(compact(24 * hour + 30 * minute), "1d");
        assert_eq!(compact(34 * hour), "1d10h");
        assert_eq!(detailed(34 * hour), "1d 10h");
        assert_eq!(compact(48 * hour), "2d");
        assert_eq!(compact(3 * day), "3d");
        assert_eq!(detailed(2 * day + 4 * hour), "2d 4h");
        assert_eq!(detailed(3 * hour + 20 * minute), "3h 20m");
        // An observation's age keeps its wording.
        assert_eq!(observed_age(1_000, 1_030), "just now");
        assert_eq!(observed_age(1_000, 1_000 + 4 * minute), "4m ago");
    }

    #[test]
    fn expiry_clock_names_the_date_beyond_six_days() {
        // 2026-09-24T15:29:59Z is a Thursday.
        let at = 1_790_263_799;
        assert_eq!(
            expiry_clock(at, at - 5 * 86_400, 0).as_deref(),
            Some("Thu 15:29")
        );
        assert_eq!(
            expiry_clock(at, at - 6 * 86_400, 7_200).as_deref(),
            Some("Sep 24 17:29")
        );
    }

    #[test]
    fn reset_clock_uses_the_captured_offset() {
        // 2026-09-24T15:29:59Z is a Thursday.
        assert_eq!(reset_clock(1_790_263_799, 0).as_deref(), Some("Thu 15:29"));
        assert_eq!(
            reset_clock(1_790_263_799, 7_200).as_deref(),
            Some("Thu 17:29")
        );
    }

    #[test]
    fn spend_rounds_to_cents_but_keeps_sub_cent_spend_visible() {
        assert_eq!(compact_spend("1.600173", "USD"), "$1.60");
        assert_eq!(compact_spend("0.000000", "USD"), "$0.00");
        assert_eq!(compact_spend("0.0012", "USD"), "<$0.01");
    }

    #[test]
    fn token_counts_and_period_start_are_compact() {
        assert_eq!(compact_tokens(950), "950");
        assert_eq!(compact_tokens(12_345), "12.3k");
        assert_eq!(compact_tokens(4_100_000), "4.1M");
        // 2026-10-01T00:00:00Z
        assert_eq!(utc_day(1_790_812_800), "Oct 1 UTC");
    }

    #[test]
    fn balances_are_compacted_by_currency() {
        assert_eq!(compact_balance("13.41", "USD"), "$13.41");
        assert_eq!(compact_balance("7.9", "CNY"), "¥7.90");
        assert_eq!(compact_balance("250.5", "USD"), "$250");
        assert_eq!(compact_balance("abc", "credits"), "abc credits");
        assert_eq!(format_balance("13.41", "USD"), "$13.41");
    }
}
