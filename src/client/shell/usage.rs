//! Sidebar usage footer: polls `usage.read` and renders provider allowance.

use std::time::{Duration, Instant};

use crate::api::schema::{
    ProviderUsage, ProviderUsageStatus, UsageAccount, UsageDailyUse, UsageErrorKind,
    UsageFreshness, UsageReport, UsageResetCredit, UsageWindow,
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
    /// Each provider's daily use labels, read in the local zone when the
    /// report arrived; render does not read the clock's zone itself.
    pub(super) daily_use_labels: DailyUseLabelsByProvider,
}

/// Provider id and the labels of its daily use positions.
pub(super) type DailyUseLabelsByProvider = Vec<(String, DailyUseLabels)>;

/// Local-time text for each daily use position, Monday first.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct DailyUseLabels {
    /// The weekday initial of each position.
    pub(super) initials: Vec<char>,
    /// Each position's hover text: its date, its use against the daily
    /// share and when it was sampled.
    pub(super) days: Vec<String>,
    /// The pace's hover text: the use and the elapsed share it compares,
    /// and the reset.
    pub(super) pace: String,
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
        self.usage.daily_use_labels = daily_use_labels(report.as_ref(), local_offset_at);
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
    crate::usage::local_utc_offset_secs()
}

/// The local zone's offset at `at`, under that date's daylight saving rules;
/// the current offset where the platform cannot say.
fn local_offset_at(at: i64) -> i64 {
    crate::platform::local_utc_offset_at(at).unwrap_or_else(local_utc_offset_secs)
}

/// The weekday initial of each daily use position, from its day's start in
/// the local offset of that date, and the hover text of each position and
/// of the pace, with times in the local offset of each instant.
fn daily_use_labels(
    report: Option<&UsageReport>,
    local_offset_at: impl Fn(i64) -> i64,
) -> DailyUseLabelsByProvider {
    let local = |at: u64| {
        let at = i64::try_from(at).unwrap_or(i64::MAX);
        at.saturating_add(local_offset_at(at))
    };
    let clock = |at: Option<u64>| at.and_then(|at| local_hour_minute(local(at)));
    report
        .into_iter()
        .flat_map(|report| &report.providers)
        .filter_map(|provider| {
            let daily_use = provider.daily_use.as_ref()?;
            let initials = daily_use
                .day_starts
                .iter()
                .map(|&start| weekday_initial(local(start)))
                .collect();
            let at = |times: &[Option<u64>], index: usize| times.get(index).copied().flatten();
            let days = daily_use
                .days
                .iter()
                .enumerate()
                .map(|(index, used)| {
                    let date = daily_use
                        .day_starts
                        .get(index)
                        .and_then(|&start| local_date(local(start)))
                        .unwrap_or_else(|| "?".into());
                    day_tooltip(DayFacts {
                        date: &date,
                        used: *used,
                        today: daily_use.current_day.map(usize::from) == Some(index),
                        partial_from: clock(at(&daily_use.partial_from, index)),
                        first_sample: clock(at(&daily_use.first_sample_at, index)),
                        last_sample: clock(at(&daily_use.last_sample_at, index)),
                    })
                })
                .collect();
            let resets = i64::try_from(daily_use.resets_at).unwrap_or(i64::MAX);
            let reset = local_date_time(local(daily_use.resets_at))
                .map(|time| format!("{time} {}", zone_label(local_offset_at(resets))))
                .unwrap_or_else(|| "?".into());
            let pace = pace_tooltip(
                daily_use.pace_percent,
                daily_use.used_percent,
                &reset,
                &provider.label,
            );
            Some((
                provider.provider.clone(),
                DailyUseLabels {
                    initials,
                    days,
                    pace,
                },
            ))
        })
        .collect()
}

/// What a day's hover text says, in local time.
struct DayFacts<'a> {
    /// `Wed 22 Oct`.
    date: &'a str,
    used: Option<i16>,
    today: bool,
    /// The day's first sample after a day without samples, `16:40`.
    partial_from: Option<String>,
    first_sample: Option<String>,
    last_sample: Option<String>,
}

/// The weekly allowance's even share of one day, in percent.
const DAILY_SHARE_PERCENT: f64 = 100.0 / 7.0;

/// One to two lines: the day's use in percent of the weekly allowance and
/// the multiple of the daily share, which the bar caps, and its samples.
fn day_tooltip(day: DayFacts<'_>) -> String {
    let date = if day.today {
        format!("Today, {}", day.date)
    } else {
        day.date.to_owned()
    };
    let Some(used) = day.used else {
        return format!("{date}: no samples\nuse unknown, not 0%");
    };
    let used_text = percent_text(f64::from(used));
    if let Some(from) = day.partial_from {
        return format!(
            "{date}: at least {used_text} observed\nsamples from {from}; earlier use unknown"
        );
    }
    let multiple = f64::from(used) / DAILY_SHARE_PERCENT;
    let share = format!("{multiple:.1}x the daily share");
    if day.today {
        let last = day
            .last_sample
            .map(|last| format!("; last sample {last}"))
            .unwrap_or_default();
        return format!("{date}: {used_text} so far\n{share}{last}");
    }
    let samples = match (day.first_sample, day.last_sample) {
        (Some(first), Some(last)) if first == last => format!("; sample {first}"),
        (Some(first), Some(last)) => format!("; samples {first}-{last}"),
        _ => String::new(),
    };
    format!(
        "{date}: {used_text} of the weekly allowance\n{share} ({}){samples}",
        percent_text(DAILY_SHARE_PERCENT)
    )
}

/// `-8 percentage points: 17% used vs 25% elapsed`, then the reset and
/// where the numbers come from.
fn pace_tooltip(pace: i32, used: Option<u8>, reset: &str, source: &str) -> String {
    let compared = match used {
        Some(used) => format!(
            "{used}% used vs {}% elapsed",
            i32::from(used).saturating_sub(pace)
        ),
        None => "against an even pace".into(),
    };
    format!(
        "{pace:+} percentage points: {compared}\nresets {reset}; source: {source} usage endpoint"
    )
}

/// `34%`, `14.3%`: one decimal unless whole.
fn percent_text(percent: f64) -> String {
    let tenths = (percent * 10.0).round();
    if tenths % 10.0 == 0.0 {
        format!("{}%", tenths / 10.0)
    } else {
        format!("{:.1}%", tenths / 10.0)
    }
}

/// `UTC+1`, `UTC-3:30`, `UTC` for an offset in seconds.
fn zone_label(offset_secs: i64) -> String {
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let minutes = offset_secs.unsigned_abs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "UTC".into(),
        (hours, 0) => format!("UTC{sign}{hours}"),
        (hours, minutes) => format!("UTC{sign}{hours}:{minutes:02}"),
    }
}

/// `09:15` for local seconds since the epoch.
fn local_hour_minute(local: i64) -> Option<String> {
    let local = time::OffsetDateTime::from_unix_timestamp(local).ok()?;
    Some(format!("{:02}:{:02}", local.hour(), local.minute()))
}

/// `Wed 22 Oct` for local seconds since the epoch.
fn local_date(local: i64) -> Option<String> {
    let local = time::OffsetDateTime::from_unix_timestamp(local).ok()?;
    let weekday = local.weekday().to_string();
    let month = local.month().to_string();
    Some(format!(
        "{} {} {}",
        weekday.get(..3).unwrap_or(&weekday),
        local.day(),
        month.get(..3).unwrap_or(&month)
    ))
}

/// `Fri 30 Oct 21:00` for local seconds since the epoch.
fn local_date_time(local: i64) -> Option<String> {
    Some(format!(
        "{} {}",
        local_date(local)?,
        local_hour_minute(local)?
    ))
}

/// `M`, `T`, `W`, … for local seconds since the epoch.
fn weekday_initial(local: i64) -> char {
    time::OffsetDateTime::from_unix_timestamp(local)
        .ok()
        .and_then(|local| local.weekday().to_string().chars().next())
        .unwrap_or('?')
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

/// Separator row plus one row per provider, and two more under a provider
/// with daily use.
fn footer_height(report: &UsageReport) -> u16 {
    report
        .providers
        .iter()
        .map(|provider| 1 + DAILY_USE_ROWS * u16::from(provider.daily_use.is_some()))
        .fold(1, u16::saturating_add)
}

/// The bars and the weekday initials under a provider's row.
const DAILY_USE_ROWS: u16 = 2;
/// Bar heights from the lowest to a full bar, twice the daily share.
const DAILY_USE_BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Column of the two-letter provider code, and of the short and weekly window cells.
const CODE_COLUMN: u16 = 1;
const SHORT_WINDOW_COLUMN: u16 = 4;
const WEEKLY_WINDOW_COLUMN: u16 = 14;

pub(super) fn render_usage_footer(
    buffer: &mut Buffer,
    area: Rect,
    report: &UsageReport,
    now_unix: u64,
    daily_use_labels: &[(String, DailyUseLabels)],
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

    let mut row = area.y + 1;
    for provider in &report.providers {
        if row >= area.bottom() {
            break;
        }
        render_provider_row(
            buffer,
            Rect::new(area.x, row, content_width, 1),
            provider,
            now_unix,
            palette,
        );
        hits.tooltips
            .push(provider_code_tooltip(area.x, row, provider));
        row += 1;
        if let Some(daily_use) = &provider.daily_use {
            if row.saturating_add(DAILY_USE_ROWS) <= area.bottom() {
                let labels = daily_use_labels
                    .iter()
                    .find(|(id, _)| *id == provider.provider)
                    .map(|(_, labels)| labels);
                render_daily_use(
                    buffer,
                    Rect::new(area.x, row, content_width, DAILY_USE_ROWS),
                    &provider.provider,
                    daily_use,
                    labels,
                    palette,
                    hits,
                );
            }
            row = row.saturating_add(DAILY_USE_ROWS);
        }
    }
}

/// One bar per weekday under the provider's windows, Monday first, on a
/// fixed scale where a full bar is twice the daily share: within the share
/// in the normal colour, over it yellow, over twice it a full bar in bold
/// red. Today is highlighted, an unknown day is `·`, a sampled day without
/// use a space and a partial day dim; then the use against an even pace.
/// Under them each day's weekday initial. Every day and the pace have a
/// tooltip with the numbers.
#[allow(clippy::too_many_arguments)] // The footer's render inputs, passed through.
fn render_daily_use(
    buffer: &mut Buffer,
    area: Rect,
    provider_id: &str,
    daily_use: &UsageDailyUse,
    labels: Option<&DailyUseLabels>,
    palette: &Palette,
    hits: &mut ShellHitMap,
) {
    let today_style = Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let mut x = area.x.saturating_add(SHORT_WINDOW_COLUMN);
    for (index, day) in daily_use.days.iter().enumerate() {
        let current = daily_use.current_day.map(usize::from) == Some(index);
        let partial = day.is_some()
            && daily_use
                .partial_from
                .get(index)
                .is_some_and(Option::is_some);
        let (glyph, mut style) = match day {
            Some(used) => {
                let used = f64::from(*used);
                let style = match DailyUseLevel::of(used) {
                    DailyUseLevel::Within if current => today_style,
                    DailyUseLevel::Within if partial => Style::default().fg(palette.overlay0),
                    DailyUseLevel::Within => Style::default().fg(palette.overlay1),
                    DailyUseLevel::Over => Style::default().fg(palette.yellow),
                    DailyUseLevel::FarOver => Style::default()
                        .fg(palette.red)
                        .add_modifier(Modifier::BOLD),
                };
                (daily_use_bar(used), style)
            }
            None if current => ('·', today_style),
            None => ('·', Style::default().fg(palette.overlay0)),
        };
        if current {
            style = style.add_modifier(Modifier::BOLD);
        }
        if partial {
            style = style.add_modifier(Modifier::DIM);
        }
        if let Some(text) = labels.and_then(|labels| labels.days.get(index)) {
            hits.tooltips.push(super::tooltip::TooltipTarget {
                rect: Rect::new(x, area.y, 1, 1),
                id: format!("usage:{provider_id}:day:{index}"),
                text: text.clone(),
                bg: None,
                starts_at_target: false,
                multiline: true,
            });
        }
        let mut glyph_buffer = [0; 4];
        put_segment(
            buffer,
            x,
            area.y,
            area.right(),
            glyph.encode_utf8(&mut glyph_buffer),
            style,
        );
        let initial = labels
            .and_then(|labels| labels.initials.get(index))
            .copied()
            .unwrap_or('?');
        put_segment(
            buffer,
            x,
            area.y.saturating_add(1),
            area.right(),
            initial.encode_utf8(&mut glyph_buffer),
            if current {
                today_style
            } else {
                Style::default().fg(palette.overlay0)
            },
        );
        x = x.saturating_add(1);
    }
    let pace = pace_label(daily_use.pace_percent);
    let pace_x = x.saturating_add(1);
    put_segment(
        buffer,
        pace_x,
        area.y,
        area.right(),
        &pace,
        Style::default().fg(if daily_use.pace_percent > 0 {
            palette.yellow
        } else {
            palette.overlay0
        }),
    );
    if let Some(labels) = labels.filter(|_| pace_x < area.right()) {
        let width = display_width(&pace).min(area.right() - pace_x);
        hits.tooltips.push(super::tooltip::TooltipTarget {
            rect: Rect::new(pace_x, area.y, width, 1),
            id: format!("usage:{provider_id}:pace"),
            text: labels.pace.clone(),
            bg: None,
            starts_at_target: false,
            multiline: true,
        });
    }
}

/// `-8pp vs pace`: the latest use less an even pace, in percentage points.
fn pace_label(pace_percent: i32) -> String {
    format!("{pace_percent:+}pp vs pace")
}

/// A day's use against the weekly allowance's even daily share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DailyUseLevel {
    /// Up to the share.
    Within,
    /// Over the share, up to twice it.
    Over,
    /// Over twice the share.
    FarOver,
}

impl DailyUseLevel {
    fn of(used: f64) -> Self {
        let multiple = used / DAILY_SHARE_PERCENT;
        if multiple > 2.0 + SCALE_EPSILON {
            Self::FarOver
        } else if multiple > 1.0 + SCALE_EPSILON {
            Self::Over
        } else {
            Self::Within
        }
    }
}

/// Absorbs float rounding at the scale's exact steps.
const SCALE_EPSILON: f64 = 1e-9;

/// The bar for `used` percent of the weekly allowance on a fixed scale:
/// 0 to twice the daily share over the eight heights, so the share itself
/// is a half bar. Any use shows at least the lowest bar; no use, or a drop,
/// is a space; above twice the share the bar stays full.
fn daily_use_bar(used: f64) -> char {
    if used <= 0.0 {
        return ' ';
    }
    let steps = DAILY_USE_BARS.len() as f64;
    let level = (used / (2.0 * DAILY_SHARE_PERCENT) * steps - SCALE_EPSILON).ceil();
    DAILY_USE_BARS[(level.clamp(1.0, steps) as usize) - 1]
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
    if provider.setup.is_some() {
        text.push_str(" · setup needed");
    } else if matches!(
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
        starts_at_target: false,
        multiline: false,
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
    if failed && provider.setup.is_none() {
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
    if provider.setup.is_some() {
        // The provider never ran; the details say what to set up.
        put_segment(
            buffer,
            short_x,
            row.y,
            row.right(),
            "setup needed",
            Style::default().fg(palette.yellow),
        );
    } else if short.is_some() || weekly.is_some() {
        let mut end = short_x;
        for (column, window) in [(SHORT_WINDOW_COLUMN, short), (WEEKLY_WINDOW_COLUMN, weekly)] {
            let Some(window) = window else {
                continue;
            };
            if window_reset_passed(window, now_unix) {
                // The value is from before the reset; the current one is unknown.
                end = put_segment(
                    buffer,
                    row.x.saturating_add(column),
                    row.y,
                    row.right(),
                    "  ?%",
                    Style::default().fg(palette.overlay0),
                );
                continue;
            }
            let color = if window_is_current(window, now_unix) {
                used_color(window.used_percent, palette)
            } else {
                palette.overlay0
            };
            end = put_segment(
                buffer,
                row.x.saturating_add(column),
                row.y,
                row.right(),
                &format!("{:>3}%", window.used_percent),
                Style::default().fg(color),
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
    } else if let Some(balance) = provider
        .balances
        .first()
        .filter(|_| provider.spend.is_empty())
    {
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
        // Credits a provider reports beside its spend (Anthropic's grants
        // other than the plan's) are money left, so they read `+$9.87`.
        match provider.balances.first() {
            Some(balance) => put_segment(
                buffer,
                x,
                row.y,
                row.right(),
                &format!(" +{}", compact_balance(&balance.total, &balance.currency)),
                Style::default().fg(palette.text),
            ),
            None => put_segment(
                buffer,
                x,
                row.y,
                row.right(),
                " spend",
                Style::default().fg(palette.overlay0),
            ),
        };
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

/// Whether the window has reset since it was observed, so its percentage
/// describes the previous window.
pub(super) fn window_reset_passed(window: &UsageWindow, now_unix: u64) -> bool {
    window
        .resets_at
        .is_some_and(|resets_at| resets_at <= now_unix)
}

/// Whether a window's percentage is current: the server calls it fresh (an
/// older server says nothing either way) and it has not reset since.
/// A stale or failed window shows its last value dimmed, never as current.
pub(super) fn window_is_current(window: &UsageWindow, now_unix: u64) -> bool {
    matches!(window.freshness, None | Some(UsageFreshness::Fresh))
        && !window_reset_passed(window, now_unix)
}

/// The window's freshness for the details, when it is not current.
pub(super) fn window_staleness(window: &UsageWindow, now_unix: u64) -> Option<String> {
    if window_reset_passed(window, now_unix) {
        return Some("reset since, now unknown".into());
    }
    match window.freshness? {
        UsageFreshness::Fresh => None,
        UsageFreshness::Stale => Some("stale".into()),
        UsageFreshness::Failed => Some(match window.error_kind {
            Some(kind) => format!("refresh failed: {}", error_kind_text(kind)),
            None => "refresh failed".into(),
        }),
        UsageFreshness::Unknown => Some("freshness unknown".into()),
    }
}

fn error_kind_text(kind: UsageErrorKind) -> &'static str {
    match kind {
        UsageErrorKind::RateLimited => "rate limited",
        UsageErrorKind::Auth => "login",
        UsageErrorKind::Setup => "setup needed",
        UsageErrorKind::Network => "network",
        UsageErrorKind::Failed | UsageErrorKind::Unknown => "error",
    }
}

/// `you@example.com · Org · keychain:Claude Code-credentials`, for the details.
pub(super) fn account_text(account: &UsageAccount) -> String {
    let mut parts = Vec::new();
    parts.extend(account.email.as_deref());
    parts.extend(account.organization.as_deref());
    parts.push(account.source.as_str());
    let mut text = parts.join(" · ");
    if let Some(dir) = account.config_dir.as_deref() {
        text.push_str(&format!(" ({dir})"));
    }
    text
}

pub(super) fn provider_code(provider: &ProviderUsage) -> String {
    match provider.provider.as_str() {
        "claude" | "anthropic_api" => "AN".into(),
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
        "anthropic_api" => "Anthropic · API credits".into(),
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

/// `text` broken at spaces into lines of at most `width` columns; a word
/// longer than the width gets a line of its own.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let needed = unicode_width::UnicodeWidthStr::width(line.as_str())
                + usize::from(!line.is_empty())
                + unicode_width::UnicodeWidthStr::width(word);
            if !line.is_empty() && needed > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
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
    use crate::api::schema::{UsageBalance, UsageSpend, UsageWindow};

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
            ..Default::default()
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
            &[],
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
                ..Default::default()
            },
            UsageWindow {
                id: "weekly".into(),
                label: "week".into(),
                used_percent: weekly,
                resets_at: Some(1_000 + 6 * 86_400),
                ..Default::default()
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

    #[test]
    fn footer_shows_credits_spent_of_a_grant_plus_other_credits_left() {
        let mut credits = provider("anthropic_api", "Anthropic · API credits");
        credits.spend.push(UsageSpend {
            currency: "USD".into(),
            amount: "76.920000".into(),
            since: 0,
            limit: Some("200.00".into()),
            limit_enforcing: false,
        });
        let report = UsageReport {
            enabled: true,
            providers: vec![credits.clone()],
        };
        assert_eq!(footer_text(&report, 26)[1], " AN $76.92/$200 spend");
        credits.balances.push(UsageBalance {
            currency: "USD".into(),
            total: "9.87".into(),
            granted: Some("10.00".into()),
            topped_up: None,
        });
        let report = UsageReport {
            enabled: true,
            providers: vec![credits],
        };
        assert_eq!(footer_text(&report, 26)[1], " AN $76.92/$200 +$9.87");
    }

    /// 2026-10-19 00:00 UTC, a Monday.
    const MONDAY: u64 = 1_792_368_000;

    fn daily_use(days: Vec<Option<i16>>, current_day: Option<u8>, pace: i32) -> UsageDailyUse {
        UsageDailyUse {
            window: "weekly".into(),
            starts_at: MONDAY,
            resets_at: MONDAY + 7 * 86_400,
            days,
            day_starts: (0..7).map(|day| MONDAY + day * 86_400).collect(),
            current_day,
            pace_percent: pace,
            partial_from: Vec::new(),
            used_percent: None,
            first_sample_at: Vec::new(),
            last_sample_at: Vec::new(),
        }
    }

    fn render_with_offset(
        report: &UsageReport,
        width: u16,
        local_offset_at: impl Fn(i64) -> i64,
    ) -> Buffer {
        let area = Rect::new(0, 0, width, footer_height(report));
        let mut buffer = Buffer::empty(area);
        render_usage_footer(
            &mut buffer,
            area,
            report,
            1_000,
            &daily_use_labels(Some(report), local_offset_at),
            &Palette::catppuccin(),
            &mut ShellHitMap::default(),
        );
        buffer
    }

    fn buffer_rows(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
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

    #[test]
    fn footer_shows_daily_use_bars_and_weekdays_under_the_an_row() {
        let mut claude = windowed("claude", "Claude", 2, 12);
        claude.daily_use = Some(daily_use(
            vec![None, Some(8), Some(2), Some(0), None, Some(4), Some(1)],
            Some(6),
            4,
        ));
        let report = UsageReport {
            enabled: true,
            providers: vec![claude, windowed("codex", "Codex", 87, 100)],
        };
        assert_eq!(footer_height(&report), 5);

        let buffer = render_with_offset(&report, 26, |_| 0);
        assert_eq!(
            buffer_rows(&buffer),
            vec![
                "─".repeat(26),
                " AN   2% 47m   12% 6d".to_owned(),
                "    ·▃▁ ·▂▁ +4pp vs pace".to_owned(),
                "    MTWTFSS".to_owned(),
                " OA  87% 47m  100% 6d".to_owned(),
            ]
        );
        let palette = Palette::catppuccin();
        // Today, Sunday, is highlighted in both rows.
        assert_eq!(buffer[(10, 2)].fg, palette.accent);
        assert_eq!(buffer[(10, 3)].fg, palette.accent);
        assert_eq!(buffer[(9, 2)].fg, palette.overlay1);
        // An unknown day is dim, never a bar.
        assert_eq!(buffer[(4, 2)].fg, palette.overlay0);
    }

    #[test]
    fn daily_use_weekdays_follow_the_local_offset_and_pace_can_be_behind() {
        let mut claude = windowed("claude", "Claude", 2, 12);
        claude.daily_use = Some(daily_use(vec![Some(3); 7], None, -3));
        let report = UsageReport {
            enabled: true,
            providers: vec![claude],
        };
        // An hour west of UTC, the Monday 00:00 UTC day starts on Sunday evening.
        let rows = buffer_rows(&render_with_offset(&report, 26, |_| -3_600));
        assert_eq!(rows[2], "    ▁▁▁▁▁▁▁ -3pp vs pace");
        assert_eq!(rows[3], "    SMTWTFS");
    }

    #[test]
    fn daily_use_weekdays_read_each_dates_own_offset_across_a_dst_change() {
        // Central Europe: CEST (+2) until 2026-10-25 01:00 UTC, then CET (+1);
        // the days start at that week's local midnights.
        let cet = |at: i64| if at < 1_792_890_000 { 7_200 } else { 3_600 };
        let mut week = daily_use(vec![Some(1); 7], Some(6), 0);
        week.day_starts = (0..7).map(|day| MONDAY - 7_200 + day * 86_400).collect();
        let mut claude = windowed("claude", "Claude", 2, 12);
        claude.daily_use = Some(week);
        let report = UsageReport {
            enabled: true,
            providers: vec![claude],
        };
        let labels = daily_use_labels(Some(&report), cet);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].0, "claude");
        assert_eq!(labels[0].1.initials, "MTWTFSS".chars().collect::<Vec<_>>());
        // Sunday 25 Oct, the day the clocks go back, is still that Sunday.
        assert!(labels[0].1.days[6].starts_with("Today, Sun 25 Oct: "));
        // Sunday evening's offset alone would put every midnight on the day before.
        assert_eq!(
            daily_use_labels(Some(&report), |_| 3_600)[0].1.initials,
            "SMTWTFS".chars().collect::<Vec<_>>()
        );
        assert!(daily_use_labels(None, cet).is_empty());
    }

    #[test]
    fn a_partial_day_is_dim_and_its_tooltip_names_its_first_sample() {
        let mut week = daily_use(
            vec![None, None, Some(3), Some(5), None, None, None],
            Some(3),
            0,
        );
        // Wednesday 09:15 UTC, after a sleep from the reset.
        let wednesday_first = MONDAY + 2 * 86_400 + 9 * 3_600 + 15 * 60;
        week.partial_from = vec![None, None, Some(wednesday_first), None, None, None, None];
        let mut claude = windowed("claude", "Claude", 2, 12);
        claude.daily_use = Some(week);
        let report = UsageReport {
            enabled: true,
            providers: vec![claude],
        };
        // In local time, an hour east of UTC.
        let labels = daily_use_labels(Some(&report), |_| 3_600);
        assert_eq!(
            labels[0].1.days[2],
            "Wed 21 Oct: at least 3% observed\nsamples from 10:15; earlier use unknown"
        );
        let area = Rect::new(0, 0, 26, footer_height(&report));
        let mut buffer = Buffer::empty(area);
        let mut hits = ShellHitMap::default();
        let palette = Palette::catppuccin();
        render_usage_footer(
            &mut buffer,
            area,
            &report,
            1_000,
            &labels,
            &palette,
            &mut hits,
        );
        assert_eq!(buffer_rows(&buffer)[2], "    ··▁▂··· +0pp vs pace");
        // Wednesday is dim; Thursday, today, counted from Wednesday, is not.
        assert_eq!(buffer[(6, 2)].fg, palette.overlay0);
        assert!(buffer[(6, 2)].modifier.contains(Modifier::DIM));
        assert!(!buffer[(7, 2)].modifier.contains(Modifier::DIM));
        let days = hits
            .tooltips
            .iter()
            .filter(|tooltip| tooltip.id.contains(":day:"))
            .collect::<Vec<_>>();
        // Every day has its numbers on hover, one cell each.
        assert_eq!(days.len(), 7);
        assert_eq!(days[2].rect, Rect::new(6, 2, 1, 1));
        assert_eq!(days[2].text, labels[0].1.days[2]);
        assert!(days.iter().all(|day| day.multiline));
        let pace = hits
            .tooltips
            .iter()
            .find(|tooltip| tooltip.id == "usage:claude:pace")
            .unwrap();
        assert_eq!(pace.rect, Rect::new(12, 2, 12, 1));
        assert_eq!(pace.text, labels[0].1.pace);
    }

    #[test]
    fn daily_use_bars_use_a_fixed_scale_where_a_full_bar_is_twice_the_daily_share() {
        let share = 100.0 / 7.0;
        assert_eq!(daily_use_bar(0.0), ' ');
        assert_eq!(daily_use_bar(-2.0), ' ');
        // Any use shows.
        assert_eq!(daily_use_bar(0.1), '▁');
        assert_eq!(daily_use_bar(1.0), '▁');
        assert_eq!(daily_use_bar(0.5 * share), '▂');
        // The share is a half bar.
        assert_eq!(daily_use_bar(share), '▄');
        assert_eq!(daily_use_bar(14.0), '▄');
        assert_eq!(daily_use_bar(15.0), '▅');
        assert_eq!(daily_use_bar(1.5 * share), '▆');
        assert_eq!(daily_use_bar(2.0 * share), '█');
        assert_eq!(daily_use_bar(5.0 * share), '█');
    }

    #[test]
    fn daily_use_colours_follow_the_daily_share() {
        let share = 100.0 / 7.0;
        assert_eq!(DailyUseLevel::of(0.0), DailyUseLevel::Within);
        assert_eq!(DailyUseLevel::of(0.5 * share), DailyUseLevel::Within);
        assert_eq!(DailyUseLevel::of(share), DailyUseLevel::Within);
        assert_eq!(DailyUseLevel::of(1.5 * share), DailyUseLevel::Over);
        assert_eq!(DailyUseLevel::of(2.0 * share), DailyUseLevel::Over);
        assert_eq!(DailyUseLevel::of(5.0 * share), DailyUseLevel::FarOver);

        // 10% within, 20% over, 40% far over, and today 20% over.
        let mut claude = windowed("claude", "Claude", 2, 12);
        claude.daily_use = Some(daily_use(
            vec![Some(10), Some(20), Some(40), Some(20), None, None, None],
            Some(3),
            -5,
        ));
        let report = UsageReport {
            enabled: true,
            providers: vec![claude],
        };
        let buffer = render_with_offset(&report, 26, |_| 0);
        let palette = Palette::catppuccin();
        assert_eq!(buffer_rows(&buffer)[2], "    ▃▆█▆··· -5pp vs pace");
        assert_eq!(buffer[(4, 2)].fg, palette.overlay1);
        assert_eq!(buffer[(5, 2)].fg, palette.yellow);
        assert!(!buffer[(5, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(6, 2)].fg, palette.red);
        assert!(buffer[(6, 2)].modifier.contains(Modifier::BOLD));
        // Today keeps the warning colour, in bold, and its initial the accent.
        assert_eq!(buffer[(7, 2)].fg, palette.yellow);
        assert!(buffer[(7, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(7, 3)].fg, palette.accent);
        assert!(buffer[(7, 3)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(5, 3)].fg, palette.overlay0);
    }

    #[test]
    fn daily_use_tooltips_give_the_numbers() {
        let mut week = daily_use(
            vec![Some(9), None, Some(34), Some(9), Some(14), None, None],
            Some(3),
            -8,
        );
        let hour = 3_600;
        let day = 86_400;
        week.partial_from = vec![
            Some(MONDAY + 16 * hour + 40 * 60),
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        week.first_sample_at = vec![
            Some(MONDAY + 16 * hour + 40 * 60),
            None,
            Some(MONDAY + 2 * day + 8 * hour + 12 * 60),
            Some(MONDAY + 3 * day + 9 * hour),
            Some(MONDAY + 4 * day + 9 * hour),
            None,
            None,
        ];
        week.last_sample_at = vec![
            Some(MONDAY + 20 * hour),
            None,
            Some(MONDAY + 2 * day + 23 * hour + 40 * 60),
            Some(MONDAY + 3 * day + 14 * hour + 32 * 60),
            Some(MONDAY + 4 * day + 9 * hour),
            None,
            None,
        ];
        week.used_percent = Some(17);
        let mut claude = windowed("claude", "Claude", 2, 12);
        claude.daily_use = Some(week);
        let report = UsageReport {
            enabled: true,
            providers: vec![claude],
        };
        let labels = &daily_use_labels(Some(&report), |_| 0)[0].1;
        assert_eq!(
            labels.days,
            vec![
                "Mon 19 Oct: at least 9% observed\nsamples from 16:40; earlier use unknown",
                "Tue 20 Oct: no samples\nuse unknown, not 0%",
                "Wed 21 Oct: 34% of the weekly allowance\n2.4x the daily share (14.3%); samples 08:12-23:40",
                "Today, Thu 22 Oct: 9% so far\n0.6x the daily share; last sample 14:32",
                "Fri 23 Oct: 14% of the weekly allowance\n1.0x the daily share (14.3%); sample 09:00",
                "Sat 24 Oct: no samples\nuse unknown, not 0%",
                "Sun 25 Oct: no samples\nuse unknown, not 0%",
            ]
        );
        assert_eq!(
            labels.pace,
            "-8 percentage points: 17% used vs 25% elapsed\nresets Mon 26 Oct 00:00 UTC; source: Claude usage endpoint"
        );
        // The reset in the zone of that instant.
        assert!(daily_use_labels(Some(&report), |_| 3_600)[0]
            .1
            .pace
            .ends_with("resets Mon 26 Oct 01:00 UTC+1; source: Claude usage endpoint"));
        assert!(daily_use_labels(Some(&report), |_| -(3 * 3_600 + 1_800))[0]
            .1
            .pace
            .contains("UTC-3:30"));
    }

    #[test]
    fn the_pace_is_labelled_in_percentage_points() {
        assert_eq!(pace_label(-8), "-8pp vs pace");
        assert_eq!(pace_label(4), "+4pp vs pace");
        assert_eq!(pace_label(0), "+0pp vs pace");
        // An older server sends no utilization.
        assert_eq!(
            pace_tooltip(3, None, "Mon 26 Oct 00:00 UTC", "Claude"),
            "+3 percentage points: against an even pace\nresets Mon 26 Oct 00:00 UTC; source: Claude usage endpoint"
        );
    }

    #[test]
    fn footer_dims_stale_windows_and_hides_values_from_before_a_reset() {
        let mut claude = windowed("claude", "Claude", 30, 100);
        claude.windows[0].freshness = Some(UsageFreshness::Fresh);
        claude.windows[1].freshness = Some(UsageFreshness::Failed);
        let mut codex = windowed("codex", "Codex", 100, 40);
        // Observed before its reset at 999, so the 100% describes the last window.
        codex.windows[0].resets_at = Some(999);
        let report = UsageReport {
            enabled: true,
            providers: vec![claude, codex],
        };

        let rows = footer_text(&report, 26);
        assert_eq!(rows[1], " AN  30% 47m  100% 6d");
        assert_eq!(rows[2], " OA   ?%       40% 6d");

        let area = Rect::new(0, 0, 26, footer_height(&report));
        let mut buffer = Buffer::empty(area);
        render_usage_footer(
            &mut buffer,
            area,
            &report,
            1_000,
            &[],
            &Palette::catppuccin(),
            &mut ShellHitMap::default(),
        );
        let palette = Palette::catppuccin();
        assert_eq!(buffer[(5, 1)].fg, palette.green);
        assert_eq!(buffer[(15, 1)].fg, palette.overlay0);
        assert_eq!(buffer[(6, 2)].fg, palette.overlay0);
    }

    #[test]
    fn window_staleness_names_the_reason() {
        let mut window = window(50, 2_000);
        assert_eq!(window_staleness(&window, 1_000), None);
        window.freshness = Some(UsageFreshness::Stale);
        assert_eq!(window_staleness(&window, 1_000).as_deref(), Some("stale"));
        window.freshness = Some(UsageFreshness::Failed);
        window.error_kind = Some(UsageErrorKind::RateLimited);
        assert_eq!(
            window_staleness(&window, 1_000).as_deref(),
            Some("refresh failed: rate limited")
        );
        assert_eq!(
            window_staleness(&window, 2_000).as_deref(),
            Some("reset since, now unknown")
        );
    }

    #[test]
    fn account_text_names_whom_and_where() {
        let account = UsageAccount {
            source: "keychain:Claude Code-credentials".into(),
            config_dir: Some("/home/me/.claude".into()),
            email: Some("me@example.com".into()),
            organization: None,
        };
        assert_eq!(
            account_text(&account),
            "me@example.com · keychain:Claude Code-credentials (/home/me/.claude)"
        );
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
                &[],
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
    fn words_wrap_at_spaces_and_keep_paragraphs() {
        assert_eq!(
            wrap_words("create an admin key\nsave it", 10),
            vec!["create an", "admin key", "save it"]
        );
        assert_eq!(
            wrap_words("averyveryverylongword x", 5),
            vec!["averyveryverylongword", "x"]
        );
    }

    #[test]
    fn a_provider_that_needs_setup_says_so_instead_of_failing() {
        let mut api = provider("openai_api", "OpenAI API");
        api.status = ProviderUsageStatus::Error;
        api.setup = Some("Create an Admin key".into());
        let tip = provider_code_tooltip(0, 0, &api);
        assert_eq!(tip.text, "OA OpenAI · API spend · setup needed");
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 1));
        render_provider_row(
            &mut buffer,
            Rect::new(0, 0, 30, 1),
            &api,
            0,
            &Palette::catppuccin(),
        );
        let row = (0..30).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert_eq!(row.trim_end(), " OA setup needed");
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
                ..Default::default()
            },
            UsageWindow {
                id: "weekly".into(),
                label: "week".into(),
                used_percent: 100,
                resets_at: None,
                ..Default::default()
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
