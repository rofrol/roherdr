//! Sidebar usage footer: polls `usage.read` and renders provider allowance.

use std::time::{Duration, Instant};

use crate::api::schema::{ProviderUsage, ProviderUsageStatus, UsageReport, UsageWindow};

use super::render::{put_segment, put_text};
use super::*;

/// `usage.read` only returns the endpoint's cached report, so polling it is cheap.
const USAGE_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// A request older than this is treated as lost so polling cannot stall.
const USAGE_REQUEST_STALE_AFTER: Duration = Duration::from_secs(60);
/// Keep room for the agents header and a few rows before showing the footer.
const AGENTS_MIN_HEIGHT: u16 = 5;
/// Columns kept free at the right of the footer's last row for the sidebar toggle.
const SIDEBAR_TOGGLE_RESERVE: u16 = 2;

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

fn local_utc_offset_secs() -> i64 {
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
        for (column, window) in [(SHORT_WINDOW_COLUMN, short), (WEEKLY_WINDOW_COLUMN, weekly)] {
            let Some(window) = window else {
                continue;
            };
            let x = put_segment(
                buffer,
                row.x.saturating_add(column),
                row.y,
                row.right(),
                &format!("{:>3}%", window.used_percent),
                Style::default().fg(used_color(window.used_percent, palette)),
            );
            if let Some(resets_at) = window.resets_at {
                put_segment(
                    buffer,
                    x,
                    row.y,
                    row.right(),
                    &format!(" {}", compact_countdown(resets_at, now_unix)),
                    Style::default().fg(palette.overlay0),
                );
            }
        }
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

/// The 5-hour and weekly windows, falling back to the first two windows reported.
/// A provider with a weekly window but no 5-hour one leaves the short column empty.
fn footer_windows(provider: &ProviderUsage) -> (Option<&UsageWindow>, Option<&UsageWindow>) {
    let by_id = |id: &str| provider.windows.iter().find(|window| window.id == id);
    let short = by_id("five_hour").or_else(|| {
        by_id("weekly")
            .is_none()
            .then(|| provider.windows.first())
            .flatten()
    });
    let weekly = by_id("weekly").or_else(|| {
        provider
            .windows
            .iter()
            .find(|window| short.is_none_or(|short| !std::ptr::eq(*window, short)))
    });
    (short, weekly)
}

pub(super) fn provider_code(provider: &ProviderUsage) -> String {
    match provider.provider.as_str() {
        "claude" => "AN".into(),
        "codex" => "OA".into(),
        "gemini" => "GO".into(),
        "deepseek" => "DS".into(),
        "openrouter" => "OR".into(),
        _ => provider
            .label
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

pub(super) fn used_color(used_percent: u8, palette: &Palette) -> ratatui::style::Color {
    match used_percent {
        0..=49 => palette.green,
        50..=79 => palette.yellow,
        _ => palette.red,
    }
}

/// `3d`, `4h`, `25m`, or `now`.
pub(super) fn compact_countdown(resets_at: u64, now_unix: u64) -> String {
    let seconds = resets_at.saturating_sub(now_unix);
    if seconds < 60 {
        "now".into()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// `2d 4h`, `3h 20m`, `12m`, or `now`.
pub(super) fn detailed_countdown(resets_at: u64, now_unix: u64) -> String {
    let seconds = resets_at.saturating_sub(now_unix);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "now".into()
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
        assert_eq!(compact_countdown(1_030, 1_000), "now");
        assert_eq!(compact_countdown(1_000 + 25 * 60, 1_000), "25m");
        assert_eq!(compact_countdown(1_000 + 4 * 3_600, 1_000), "4h");
        assert_eq!(compact_countdown(1_000 + 3 * 86_400, 1_000), "3d");
        assert_eq!(compact_countdown(500, 1_000), "now");
        assert_eq!(
            detailed_countdown(1_000 + 2 * 86_400 + 4 * 3_600, 1_000),
            "2d 4h"
        );
        assert_eq!(
            detailed_countdown(1_000 + 3 * 3_600 + 20 * 60, 1_000),
            "3h 20m"
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
    fn balances_are_compacted_by_currency() {
        assert_eq!(compact_balance("13.41", "USD"), "$13.41");
        assert_eq!(compact_balance("7.9", "CNY"), "¥7.90");
        assert_eq!(compact_balance("250.5", "USD"), "$250");
        assert_eq!(compact_balance("abc", "credits"), "abc credits");
        assert_eq!(format_balance("13.41", "USD"), "$13.41");
    }
}
