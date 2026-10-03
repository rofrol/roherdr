use super::*;

mod settings_overlay;
mod worktree_overlays;

#[derive(Default)]
pub(crate) struct OverlayRender {
    pub(crate) area: Rect,
    pub(crate) menu_rows: Vec<(Rect, usize)>,
    pub(crate) primary: Rect,
    pub(crate) clear: Rect,
    pub(crate) cancel: Rect,
    pub(crate) navigator_popup: Rect,
    pub(crate) navigator_search: Rect,
    pub(crate) navigator_rows: Vec<(Rect, ClientNavigatorTarget)>,
    pub(crate) navigator_scrollbar: Rect,
    pub(crate) navigator_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) worktree_search: Rect,
    pub(crate) worktree_rows: Vec<(Rect, usize)>,
    pub(crate) help_popup: Rect,
    pub(crate) help_scrollbar: Rect,
    pub(crate) help_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) help_max_scroll: usize,
    pub(crate) settings_popup: Rect,
    pub(crate) settings_tabs: Vec<(Rect, ClientSettingsSection)>,
    pub(crate) settings_choices: Vec<(Rect, usize)>,
    pub(crate) product_announcement_scrollbar: Rect,
    pub(crate) product_announcement_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) product_announcement_max_scroll: usize,
    pub(crate) release_notes_scrollbar: Rect,
    pub(crate) release_notes_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) release_notes_max_scroll: usize,
    pub(crate) cursor: Option<crate::protocol::CursorState>,
}

pub(crate) fn render_client_overlay(
    b: &mut Buffer,
    o: &ClientShellOverlay,
    s: &ClientShellSnapshot,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    if !matches!(
        o,
        ClientShellOverlay::Navigator(_)
            | ClientShellOverlay::ContextMenu(_)
            | ClientShellOverlay::GlobalMenu(_)
    ) {
        for y in b.area.y..b.area.bottom() {
            for x in b.area.x..b.area.right() {
                let c = &mut b[(x, y)];
                c.set_style(c.style().add_modifier(Modifier::DIM));
            }
        }
    }
    match o {
        ClientShellOverlay::Onboarding => render_onboarding_overlay(b, k, p),
        ClientShellOverlay::ProductAnnouncement(v) => render_product_announcement_overlay(b, v, p),
        ClientShellOverlay::ReleaseNotes(v) => {
            render_release_notes_overlay(b, v, &s.update_install_command, p)
        }
        ClientShellOverlay::Rename(v) => render_rename_overlay(b, v, p),
        ClientShellOverlay::ConfirmClose(v) => render_confirm_close_overlay(b, v, p),
        ClientShellOverlay::Help(v) => render_help_overlay(b, v, k, p),
        ClientShellOverlay::Navigator(v) => {
            render_navigator_overlay(b, v, endpoints, active_endpoint_id, p)
        }
        ClientShellOverlay::Settings(v) => {
            settings_overlay::render_settings_overlay(b, v, s.integration_updates_available, p)
        }
        ClientShellOverlay::WorktreeCreate(v) => {
            worktree_overlays::render_worktree_create_overlay(b, v, p)
        }
        ClientShellOverlay::WorktreeOpen(v) => {
            worktree_overlays::render_worktree_open_overlay(b, v, p)
        }
        ClientShellOverlay::WorktreeRemove(v) => {
            worktree_overlays::render_worktree_remove_overlay(b, v, p)
        }
        ClientShellOverlay::Usage(v) => render_usage_overlay(b, v, p),
        ClientShellOverlay::ContextMenu(_)
        | ClientShellOverlay::GlobalMenu(_)
        | ClientShellOverlay::NotificationLog(_) => None,
    }
}

const USAGE_MODAL_WIDTH: u16 = 76;
const USAGE_BAR_WIDTH: usize = 12;

fn usage_overlay_lines(
    overlay: &super::usage::ClientUsageOverlay,
    now_unix: u64,
    p: &Palette,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::{Line, Span};
    // Paragraph patches cell styles, so clear modifiers left by the sidebar underneath.
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::BOLD | Modifier::DIM);
    let dim = base.fg(p.overlay0);
    let Some(report) = overlay.report.as_ref() else {
        return vec![Line::from(Span::styled(" waiting for the server…", dim))];
    };
    if !report.enabled {
        return vec![Line::from(Span::styled(
            " usage polling is disabled; set [usage] enabled = true",
            dim,
        ))];
    }
    let mut lines = Vec::new();
    for provider in &report.providers {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let mut heading = vec![Span::styled(
            format!(" {}", provider.label),
            base.fg(p.text).add_modifier(Modifier::BOLD),
        )];
        if let Some(plan) = provider.plan.as_deref() {
            heading.push(Span::styled(format!(" · {plan}"), base.fg(p.overlay1)));
        }
        if let Some(observed_at) = provider.observed_at {
            heading.push(Span::styled(
                format!("  {}", super::usage::observed_age(observed_at, now_unix)),
                dim,
            ));
        }
        lines.push(Line::from(heading));
        for window in &provider.windows {
            let used = window.used_percent.min(100);
            let filled = (usize::from(used) * USAGE_BAR_WIDTH).div_ceil(100);
            let color = super::usage::used_color(used, p);
            let mut spans = vec![
                Span::styled(format!("   {:<11}", window.label), base.fg(p.overlay1)),
                Span::styled("█".repeat(filled), base.fg(color)),
                Span::styled("░".repeat(USAGE_BAR_WIDTH - filled), base.fg(p.surface1)),
                Span::styled(format!(" {used:>3}% used"), base.fg(color)),
            ];
            if let Some(resets_at) = window.resets_at {
                let mut reset = if resets_at < now_unix.saturating_add(60) {
                    "  resetting now".to_owned()
                } else {
                    format!(
                        "  resets in {}",
                        super::usage::detailed_countdown(resets_at, now_unix)
                    )
                };
                if let Some(clock) = super::usage::reset_clock(resets_at, overlay.utc_offset_secs) {
                    reset.push_str(&format!(" ({clock})"));
                }
                spans.push(Span::styled(reset, dim));
            }
            lines.push(Line::from(spans));
        }
        for balance in &provider.balances {
            let mut text = format!(
                "   balance    {}",
                super::usage::format_balance(&balance.total, &balance.currency)
            );
            let parts = [
                ("topped up", balance.topped_up.as_deref()),
                ("granted", balance.granted.as_deref()),
            ]
            .into_iter()
            .filter_map(|(label, amount)| {
                amount.map(|amount| {
                    format!(
                        "{label} {}",
                        super::usage::format_balance(amount, &balance.currency)
                    )
                })
            })
            .collect::<Vec<_>>();
            if !parts.is_empty() {
                text.push_str(&format!(" ({})", parts.join(", ")));
            }
            lines.push(Line::from(Span::styled(text, base.fg(p.text))));
        }
        for spend in &provider.spend {
            let mut text = format!(
                "   spend      {}",
                super::usage::compact_spend(&spend.amount, &spend.currency)
            );
            if let (Some(limit), Some(percent)) = (
                spend.limit.as_deref(),
                super::usage::spend_limit_percent(spend),
            ) {
                text.push_str(&format!(
                    " of {} limit ({percent}%)",
                    super::usage::format_balance(limit, &spend.currency)
                ));
            }
            lines.push(Line::from(vec![
                Span::styled(text, base.fg(p.text)),
                Span::styled(
                    format!("  since {}", super::usage::utc_day(spend.since)),
                    dim,
                ),
            ]));
            if spend.limit_enforcing {
                lines.push(Line::from(Span::styled(
                    "   limit reached: the provider rejects API requests",
                    base.fg(p.red),
                )));
            }
        }
        if let Some(tokens) = &provider.completion_tokens {
            let plural = if tokens.requests == 1 { "" } else { "s" };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "   tokens     in {} (cached {}), out {}",
                        super::usage::compact_tokens(tokens.input),
                        super::usage::compact_tokens(tokens.cached_input),
                        super::usage::compact_tokens(tokens.output),
                    ),
                    base.fg(p.text),
                ),
                Span::styled(
                    format!("  {} request{plural}, completions only", tokens.requests),
                    dim,
                ),
            ]));
        }
        lines.extend(reset_credit_lines(
            provider,
            now_unix,
            overlay.utc_offset_secs,
            base,
            p,
        ));
        for note in &provider.notes {
            lines.push(Line::from(Span::styled(format!("   {note}"), dim)));
        }
        match provider.status {
            crate::api::schema::ProviderUsageStatus::Pending => {
                lines.push(Line::from(Span::styled("   loading…", dim)));
            }
            crate::api::schema::ProviderUsageStatus::Error
            | crate::api::schema::ProviderUsageStatus::Unknown => {
                let message = provider.message.as_deref().unwrap_or("refresh failed");
                lines.push(Line::from(Span::styled(
                    format!("   ! {message}"),
                    base.fg(p.red),
                )));
            }
            crate::api::schema::ProviderUsageStatus::Ok => {}
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no providers enabled in [usage]",
            dim,
        )));
    }
    lines
}

/// `3 reset credits · Full reset (Weekly + 5 hr) · redeem in Codex`, then one
/// expiry per line, soonest first. A shared title is shown once in the heading.
fn reset_credit_lines(
    provider: &crate::api::schema::ProviderUsage,
    now_unix: u64,
    utc_offset_secs: i64,
    base: Style,
    p: &Palette,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::{Line, Span};
    let credits = super::usage::live_reset_credits(provider, now_unix).collect::<Vec<_>>();
    let Some(first) = credits.first() else {
        return Vec::new();
    };
    let shared_title = credits
        .iter()
        .all(|credit| credit.title == first.title)
        .then_some(first.title.as_str())
        .filter(|title| !title.is_empty());
    let plural = if credits.len() == 1 { "" } else { "s" };
    let mut heading = format!("   {} reset credit{plural}", credits.len());
    if let Some(title) = shared_title {
        heading.push_str(&format!(" · {title}"));
    }
    heading.push_str(&format!(" · redeem in {}", provider.label));
    let mut lines = vec![Line::from(Span::styled(heading, base.fg(p.overlay1)))];
    for credit in credits {
        let mut text = "     ".to_owned();
        if shared_title.is_none() && !credit.title.is_empty() {
            text.push_str(&format!("{}: ", credit.title));
        }
        let color = match credit.expires_at {
            Some(expires_at) => {
                text.push_str(&format!(
                    "expires in {}",
                    super::usage::detailed_countdown(expires_at, now_unix)
                ));
                if let Some(clock) =
                    super::usage::expiry_clock(expires_at, now_unix, utc_offset_secs)
                {
                    text.push_str(&format!(" ({clock})"));
                }
                if expires_at.saturating_sub(now_unix) <= super::usage::RESET_CREDIT_WARNING_SECS {
                    p.yellow
                } else {
                    p.overlay0
                }
            }
            None => {
                text.push_str("no expiry reported");
                p.overlay0
            }
        };
        lines.push(Line::from(Span::styled(text, base.fg(color))));
    }
    lines
}

fn render_usage_overlay(
    b: &mut Buffer,
    overlay: &super::usage::ClientUsageOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let lines = usage_overlay_lines(overlay, crate::usage::now_unix(), p);
    let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    // Border, header, gap, content, gap, footer, border.
    let outer = popup(b.area, USAGE_MODAL_WIDTH, content_height.saturating_add(6))?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    put_text(
        b,
        inner.x.saturating_add(1),
        inner.y,
        inner.width.saturating_sub(2),
        "usage",
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    let label = if overlay.refreshing {
        " refreshing… "
    } else {
        " r refresh "
    };
    let refresh_width = display_width(label).min(inner.width);
    let refresh = Rect::new(
        inner.right().saturating_sub(refresh_width),
        inner.y,
        refresh_width,
        1,
    );
    button(
        b,
        refresh,
        label,
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        inner.height.saturating_sub(4),
    );
    ratatui::widgets::Widget::render(ratatui::widgets::Paragraph::new(lines), body, b);
    put_text(
        b,
        inner.x.saturating_add(1),
        inner.bottom().saturating_sub(1),
        inner.width.saturating_sub(2),
        "esc close · r refresh",
        base.fg(p.overlay0),
    );
    Some(OverlayRender {
        area: outer,
        primary: refresh,
        ..OverlayRender::default()
    })
}

pub(crate) fn render_global_menu(
    buffer: &mut Buffer,
    launcher: Rect,
    menu: &ClientGlobalMenuOverlay,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
) -> Option<OverlayRender> {
    let items = super::super::global_menu::global_menu_items(snapshot);
    let screen = buffer.area;
    let width = items
        .iter()
        .map(|(label, action)| {
            display_width(label)
                + u16::from(super::super::global_menu::global_menu_item_has_badge(
                    snapshot, *action,
                )) * 2
        })
        .max()
        .unwrap_or(8)
        .saturating_add(4)
        .min(screen.width.max(1));
    let height = (items.len() as u16)
        .saturating_add(2)
        .min(screen.height.max(1));
    let x = launcher
        .right()
        .saturating_sub(width)
        .min(screen.right().saturating_sub(width));
    let y = launcher.y.saturating_sub(height).max(screen.y);
    let rect = Rect::new(x, y, width, height);
    let inner = panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let mut rows = Vec::new();
    for (index, (label, action)) in items.iter().enumerate() {
        let row_y = inner.y.saturating_add(index as u16);
        if row_y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, row_y, inner.width, 1);
        let highlighted = index == menu.highlighted;
        let style = if highlighted {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.panel_bg)
        };
        buffer.set_style(row, style);
        let has_badge = super::super::global_menu::global_menu_item_has_badge(snapshot, *action);
        if has_badge {
            let badge_style = if highlighted {
                style
            } else {
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD)
            };
            put_text(buffer, row.x, row.y, row.width.min(2), " ●", badge_style);
            put_text(
                buffer,
                row.x.saturating_add(2),
                row.y,
                row.width.saturating_sub(2),
                &format!(" {label}"),
                style,
            );
        } else {
            put_text(buffer, row.x, row.y, row.width, &format!(" {label}"), style);
        }
        rows.push((row, index));
    }
    Some(OverlayRender {
        area: rect,
        menu_rows: rows,
        ..OverlayRender::default()
    })
}

/// One row of the dropdown: its day group (`Today`, `Oct 2`), the time, the
/// text, whether it is unread, and the tab's state icon with its colour.
pub(crate) type LogRow = (
    Option<String>,
    String,
    String,
    bool,
    Option<(&'static str, ratatui::style::Color)>,
);

/// The notification history dropdown under its button, over the panes:
/// `HH:MM` and the text per row, newest first, unread ones marked `•` in
/// the column after the selection bar. A muted separator line names the day
/// above the first row of each day, so the times need no date of their own.
pub(crate) fn render_notification_log(
    buffer: &mut Buffer,
    button: Rect,
    highlighted: Option<usize>,
    rows: &[LogRow],
    palette: &Palette,
) -> Option<OverlayRender> {
    let screen = buffer.area;
    // As wide as the longest row, like a tab label's tooltip: at least 56
    // columns, at most the screen less a margin (and 140). The left edge stays
    // under the button and the box moves left when the right edge is short.
    let max_width = screen
        .width
        .saturating_sub(2)
        .min(140)
        .max(20.min(screen.width));
    let min_width = 56.min(max_width);
    // Each row whose day differs from the row above starts a group with a
    // separator line; separators are not rows, so they take no hit target.
    let separators = rows
        .iter()
        .enumerate()
        .map(|(index, (day, ..))| {
            day.as_ref()
                .filter(|day| index == 0 || rows[index - 1].0.as_ref() != Some(*day))
        })
        .collect::<Vec<_>>();
    // One column for the times and one for the state icons, the same on every
    // row.
    let time_width = rows
        .iter()
        .map(|(_, time, ..)| display_width(time))
        .max()
        .unwrap_or(0);
    let has_icons = rows.iter().any(|(.., icon)| icon.is_some());
    let text_offset =
        2 + if time_width > 0 { time_width + 1 } else { 0 } + if has_icons { 2 } else { 1 };
    let widest = rows
        .iter()
        .map(|(_, _, text, ..)| {
            text_offset + display_width(&text.replace(|c: char| c.is_control(), " ")) + 1
        })
        .max()
        .unwrap_or(0);
    let width = widest.saturating_add(2).clamp(min_width, max_width);
    let lines = rows.len() + separators.iter().flatten().count();
    let height = (lines.max(1) as u16)
        .saturating_add(2)
        .min(screen.height.saturating_sub(button.bottom()).max(3));
    let x = button.x.min(screen.right().saturating_sub(width));
    let rect = Rect::new(x, button.bottom(), width, height).intersection(screen);
    let inner = panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let mut hits = Vec::new();
    if rows.is_empty() {
        put_text(
            buffer,
            inner.x.saturating_add(1),
            inner.y,
            inner.width.saturating_sub(1),
            "no notifications yet",
            Style::default().fg(palette.overlay1).bg(palette.panel_bg),
        );
    }
    let mut row_y = inner.y;
    for (index, (_, time, text, unread, icon)) in rows.iter().enumerate() {
        if let Some(day) = separators[index] {
            if row_y >= inner.bottom() {
                break;
            }
            put_text(
                buffer,
                inner.x.saturating_add(1),
                row_y,
                inner.width.saturating_sub(1),
                day,
                Style::default().fg(palette.overlay1).bg(palette.panel_bg),
            );
            row_y = row_y.saturating_add(1);
        }
        if row_y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, row_y, inner.width, 1);
        let selected = Some(index) == highlighted;
        // The highlighted row is a light accent tint with a bar in the first
        // column, so the state icons keep their own colours (a solid accent
        // fill turned them white); without an RGB palette, the solid fill.
        let tint = super::render::tabs::blend(
            palette.accent,
            palette.panel_bg,
            1,
            if matches!(palette.panel_bg, ratatui::style::Color::Rgb(r, g, b)
                if 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) < 128_000)
            {
                4
            } else {
                6
            },
        );
        let solid = selected && tint.is_none();
        let base = match (selected, tint) {
            (true, Some(tint)) => Style::default().fg(palette.text).bg(tint),
            (true, None) => Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent),
            _ => Style::default().fg(palette.text).bg(palette.panel_bg),
        };
        buffer.set_style(row, base);
        // The selection bar and the unread mark have columns of their own, so
        // the highlighted row still shows whether it is unread.
        if selected && !solid {
            put_text(buffer, row.x, row.y, 1, "▌", base.fg(palette.accent));
        }
        if *unread {
            let mark_style = if solid {
                base
            } else {
                base.fg(palette.accent).add_modifier(Modifier::BOLD)
            };
            put_text(buffer, row.x.saturating_add(1), row.y, 1, "•", mark_style);
        }
        let time_style = if solid {
            base
        } else {
            base.fg(palette.overlay1)
        };
        // The time (right-aligned in its column), then the tab's state icon as
        // its sidebar line draws it, then the text, each with a space between.
        let mut column = row.x.saturating_add(2);
        if time_width > 0 {
            let pad = time_width.saturating_sub(display_width(time));
            put_text(
                buffer,
                column.saturating_add(pad),
                row.y,
                time_width,
                time,
                time_style,
            );
            column = column.saturating_add(time_width + 1);
        }
        // A row with no live tab starts its text (with its own mark) in the
        // icon column, so the words line up with the rows that have an icon.
        let mut text_x = column;
        if has_icons {
            if let Some((glyph, color)) = icon {
                let style = if solid { base } else { base.fg(*color) };
                put_text(buffer, column, row.y, 1, glyph, style);
                text_x = column.saturating_add(2);
            }
        }
        let text = crate::ui::truncate_end(
            &text.replace(|character: char| character.is_control(), " "),
            usize::from(row.right().saturating_sub(text_x)),
        );
        put_text(
            buffer,
            text_x,
            row.y,
            row.right().saturating_sub(text_x),
            &text,
            base,
        );
        hits.push((row, index));
        row_y = row_y.saturating_add(1);
    }
    Some(OverlayRender {
        area: rect,
        menu_rows: hits,
        ..OverlayRender::default()
    })
}

/// The job status a context menu item closes, when it is one of the
/// `Close jobs:` row's chips.
fn job_chip_status(action: ClientContextMenuAction) -> Option<crate::api::schema::TabStatus> {
    use crate::api::schema::TabStatus;
    match action {
        ClientContextMenuAction::StopRunningJobs => Some(TabStatus::Running),
        ClientContextMenuAction::CloseFailedJobs => Some(TabStatus::Failed),
        ClientContextMenuAction::CloseSucceededJobs => Some(TabStatus::Succeeded),
        _ => None,
    }
}

const JOB_CHIPS_LABEL: &str = "Close jobs:";

pub(crate) fn render_context_menu(
    buffer: &mut Buffer,
    menu: &ClientContextMenuOverlay,
    palette: &Palette,
) -> Option<OverlayRender> {
    let items = menu.items();
    let screen = buffer.area;
    let chips = items
        .iter()
        .enumerate()
        .filter(|(_, item)| job_chip_status(item.action).is_some())
        .collect::<Vec<_>>();
    // Each chip is its label with a column of padding each side, a column apart.
    let chips_width = chips
        .iter()
        .map(|(_, item)| display_width(&item.label) + 3)
        .sum::<u16>();
    let chips_row_width = if chips.is_empty() {
        0
    } else {
        display_width(JOB_CHIPS_LABEL) + 1 + chips_width
    };
    let max_item_width = items
        .iter()
        .filter(|item| job_chip_status(item.action).is_none())
        .map(|item| display_width(&item.label))
        .max()
        .unwrap_or(0)
        .max(chips_row_width);
    let width = max_item_width
        .saturating_add(4)
        .max(14)
        .min(screen.width.max(1));
    let row_count = items.len() - chips.len() + usize::from(!chips.is_empty());
    let height = (row_count as u16)
        .saturating_add(2)
        .min(screen.height.max(1));
    let x = menu
        .x
        .min(screen.x.saturating_add(screen.width.saturating_sub(width)));
    let y = menu.y.min(
        screen
            .y
            .saturating_add(screen.height.saturating_sub(height)),
    );
    let rect = Rect::new(x, y, width, height);
    let inner = panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let highlight = Style::default()
        .fg(panel_contrast_fg(palette))
        .bg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let plain = Style::default().fg(palette.text).bg(palette.panel_bg);
    let mut rows = Vec::new();
    let mut row_y = inner.y;
    let mut chips_drawn = false;
    for (index, item) in items.iter().enumerate() {
        if row_y >= inner.bottom() {
            break;
        }
        if let Some(status) = job_chip_status(item.action) {
            if chips_drawn {
                continue;
            }
            chips_drawn = true;
            let row = Rect::new(inner.x, row_y, inner.width, 1);
            buffer.set_style(row, plain);
            put_text(buffer, row.x, row.y, row.width, JOB_CHIPS_LABEL, plain);
            let mut chip_x = row.x.saturating_add(display_width(JOB_CHIPS_LABEL) + 1);
            for (chip_index, chip) in &chips {
                let chip_status = job_chip_status(chip.action).unwrap_or(status);
                let width = display_width(&chip.label) + 2;
                let chip_rect = Rect::new(chip_x, row.y, width, 1).intersection(row);
                let style = if *chip_index == menu.highlighted {
                    highlight
                } else {
                    Style::default()
                        .fg(super::tabs::tab_status_color(Some(chip_status), palette)
                            .unwrap_or(palette.text))
                        .bg(palette.surface0)
                        .add_modifier(Modifier::BOLD)
                };
                buffer.set_style(chip_rect, style);
                put_text(
                    buffer,
                    chip_rect.x,
                    row.y,
                    chip_rect.width,
                    &format!(" {} ", chip.label),
                    style,
                );
                rows.push((chip_rect, *chip_index));
                chip_x = chip_x.saturating_add(width + 1);
            }
            row_y = row_y.saturating_add(1);
            continue;
        }
        let row = Rect::new(inner.x, row_y, inner.width, 1);
        let style = if index == menu.highlighted {
            highlight
        } else {
            plain
        };
        buffer.set_style(row, style);
        // One column of padding each side: the text does not touch the border
        // (the width reserves two spare columns for it).
        put_text(
            buffer,
            row.x.saturating_add(1),
            row.y,
            row.width.saturating_sub(1),
            &item.label,
            style,
        );
        rows.push((row, index));
        row_y = row_y.saturating_add(1);
    }
    Some(OverlayRender {
        area: rect,
        menu_rows: rows,
        ..OverlayRender::default()
    })
}

fn panel(
    b: &mut Buffer,
    a: Rect,
    c: ratatui::style::Color,
    bg: ratatui::style::Color,
) -> Option<Rect> {
    if a.width < 2 || a.height < 2 {
        return None;
    }
    let background = Style::default().bg(bg).remove_modifier(Modifier::DIM);
    let border = Style::default().fg(c).bg(bg).remove_modifier(Modifier::DIM);
    for y in a.y..a.bottom() {
        for x in a.x..a.right() {
            b[(x, y)].set_symbol(" ").set_style(background);
        }
    }
    for x in a.x..a.right() {
        b[(x, a.y)]
            .set_symbol(if x == a.x {
                "┌"
            } else if x + 1 == a.right() {
                "┐"
            } else {
                "─"
            })
            .set_style(border);
        let y = a.bottom() - 1;
        b[(x, y)]
            .set_symbol(if x == a.x {
                "└"
            } else if x + 1 == a.right() {
                "┘"
            } else {
                "─"
            })
            .set_style(border);
    }
    for y in a.y + 1..a.bottom() - 1 {
        b[(a.x, y)].set_symbol("│").set_style(border);
        b[(a.right() - 1, y)].set_symbol("│").set_style(border);
    }
    Some(Rect::new(a.x + 1, a.y + 1, a.width - 2, a.height - 2))
}
fn popup(a: Rect, w: u16, h: u16) -> Option<Rect> {
    popup_with_width_cap(a, w, h, 4)
}

fn popup_with_width_cap(a: Rect, w: u16, h: u16, width_cap: u16) -> Option<Rect> {
    let w = w.min(a.width.saturating_sub(width_cap));
    let h = h.min(a.height.saturating_sub(2));
    if w < 4 || h < 4 {
        return None;
    }
    Some(Rect::new(
        a.x + (a.width - w) / 2,
        a.y + (a.height - h) / 2,
        w,
        h,
    ))
}
fn button(b: &mut Buffer, r: Rect, t: &str, s: Style) {
    b.set_style(r, s);
    let w = display_width(t).min(r.width);
    put_text(b, r.x + (r.width - w) / 2, r.y, w, t, s)
}
fn row(i: Rect, ws: &[u16], gap: u16, off: u16) -> Vec<Rect> {
    let total = ws.iter().sum::<u16>() + gap * (ws.len().saturating_sub(1) as u16);
    let mut x = i.x + i.width.saturating_sub(total) / 2;
    ws.iter()
        .map(|w| {
            let r = Rect::new(
                x,
                i.y + off.min(i.height.saturating_sub(1)),
                (*w).min(i.width.saturating_sub(x - i.x)),
                1,
            );
            x += *w + gap;
            r
        })
        .collect()
}
fn contrast(p: &Palette) -> ratatui::style::Color {
    match p.panel_bg {
        ratatui::style::Color::Reset => p.surface_dim,
        c => c,
    }
}
fn render_release_notes_overlay(
    b: &mut Buffer,
    notes: &crate::app::state::ReleaseNotesState,
    install_command: &str,
    p: &Palette,
) -> Option<OverlayRender> {
    let outer = popup(
        b.area,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
    )?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    if inner.height < 8 || inner.width < 20 {
        return Some(OverlayRender {
            area: outer,
            ..OverlayRender::default()
        });
    }

    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    let title_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );
    let subtitle_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y.saturating_add(1),
        stack.header.width.saturating_sub(2),
        1,
    );
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    put_text(
        b,
        title_area.x,
        title_area.y,
        title_area.width,
        &format!("v{}", notes.version),
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        subtitle_area.x,
        subtitle_area.y,
        subtitle_area.width,
        if notes.preview {
            "update ready"
        } else {
            "what's new in this release"
        },
        base.fg(p.overlay1),
    );
    let close = crate::ui::release_notes_close_button_rect(Rect::new(
        stack.header.x,
        stack.header.y,
        stack.header.width,
        1,
    ));
    button(
        b,
        close,
        " esc close ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );

    let body = stack.content;
    let lines = crate::ui::release_notes_display_lines(notes, install_command, p);
    let metrics = crate::ui::release_notes_scroll_metrics(notes, install_command, body, p);
    let max_scroll = metrics.max_offset_from_bottom;
    let scroll = usize::from(notes.scroll).min(max_scroll);
    let track = crate::ui::release_notes_scrollbar_rect(body, metrics);
    let text_area = track
        .map(|_| Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height))
        .unwrap_or(body);
    let paragraph = ratatui::widgets::Paragraph::new(
        lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>(),
    )
    .wrap(ratatui::widgets::Wrap { trim: false })
    .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
    ratatui::widgets::Widget::render(paragraph, text_area, b);
    if let Some(track) = track {
        crate::ui::render_scrollbar_buffer(b, metrics, track, p.overlay0, p.overlay1, "▐");
    }

    if let Some(footer_area) = stack.footer {
        let footer_line = ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(" scroll ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("wheel ↑↓", base.fg(p.text)),
            ratatui::text::Span::styled("  ·  ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("close", base.fg(p.overlay0)),
            ratatui::text::Span::styled(" esc / enter ", base.fg(p.text)),
        ]);
        ratatui::widgets::Widget::render(
            ratatui::widgets::Paragraph::new(footer_line),
            footer_area,
            b,
        );
    }

    Some(OverlayRender {
        area: outer,
        primary: close,
        release_notes_scrollbar: track.unwrap_or_default(),
        release_notes_scroll_metrics: Some(metrics),
        release_notes_max_scroll: max_scroll,
        ..OverlayRender::default()
    })
}

fn render_product_announcement_overlay(
    b: &mut Buffer,
    announcement: &crate::app::state::ProductAnnouncementState,
    p: &Palette,
) -> Option<OverlayRender> {
    let outer = popup(
        b.area,
        crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
        crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
    )?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    if inner.height < 8 || inner.width < 20 {
        return Some(OverlayRender {
            area: outer,
            ..OverlayRender::default()
        });
    }

    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    let title_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );
    let subtitle_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y.saturating_add(1),
        stack.header.width.saturating_sub(2),
        1,
    );
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    put_text(
        b,
        title_area.x,
        title_area.y,
        title_area.width,
        &announcement.title,
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    let subtitle = if announcement.preview {
        "product announcement preview"
    } else {
        "product announcement"
    };
    put_text(
        b,
        subtitle_area.x,
        subtitle_area.y,
        subtitle_area.width,
        &format!("{subtitle} · v{}", announcement.version),
        base.fg(p.overlay1),
    );
    let close = crate::ui::release_notes_close_button_rect(Rect::new(
        stack.header.x,
        stack.header.y,
        stack.header.width,
        1,
    ));
    button(
        b,
        close,
        " esc close ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );

    let body = stack.content;
    let lines = crate::ui::product_announcement_display_lines(announcement, p);
    let metrics = crate::ui::product_announcement_scroll_metrics(announcement, body, p);
    let max_scroll = metrics.max_offset_from_bottom;
    let scroll = usize::from(announcement.scroll).min(max_scroll);
    let track = crate::ui::release_notes_scrollbar_rect(body, metrics);
    let text_area = track
        .map(|_| Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height))
        .unwrap_or(body);
    let paragraph = ratatui::widgets::Paragraph::new(
        lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>(),
    )
    .wrap(ratatui::widgets::Wrap { trim: false })
    .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
    ratatui::widgets::Widget::render(paragraph, text_area, b);
    if let Some(track) = track {
        crate::ui::render_scrollbar_buffer(b, metrics, track, p.overlay0, p.overlay1, "▐");
    }

    if let Some(footer_area) = stack.footer {
        let footer_line = ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(" scroll ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("wheel ↑↓", base.fg(p.text)),
            ratatui::text::Span::styled("  ·  ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("close", base.fg(p.overlay0)),
            ratatui::text::Span::styled(" esc / enter ", base.fg(p.text)),
        ]);
        ratatui::widgets::Widget::render(
            ratatui::widgets::Paragraph::new(footer_line),
            footer_area,
            b,
        );
    }

    Some(OverlayRender {
        area: outer,
        primary: close,
        product_announcement_scrollbar: track.unwrap_or_default(),
        product_announcement_scroll_metrics: Some(metrics),
        product_announcement_max_scroll: max_scroll,
        ..OverlayRender::default()
    })
}

fn render_onboarding_overlay(
    b: &mut Buffer,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    let prefix = k.primary_prefix_label();
    let hint_width = display_width("  ")
        + display_width(&prefix)
        + display_width(crate::ui::ONBOARDING_PREFIX_SUFFIX)
        + display_width(crate::ui::ONBOARDING_HELP_LABEL)
        + display_width(crate::ui::ONBOARDING_HELP_SUFFIX);
    let outer = popup_with_width_cap(b.area, (hint_width + 5).max(64), 16, 3)?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    if inner.height < 11 {
        return Some(OverlayRender {
            area: outer,
            ..OverlayRender::default()
        });
    }
    let stack = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1);
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    let title = base.fg(p.text).add_modifier(Modifier::BOLD);
    let muted = base.fg(p.overlay0);
    let text = base.fg(p.overlay1);
    let accent = base.fg(p.accent).add_modifier(Modifier::BOLD);

    put_text(
        b,
        stack.header.x,
        stack.header.y,
        stack.header.width,
        crate::ui::ONBOARDING_TITLE,
        title,
    );
    put_text(
        b,
        stack.header.x,
        stack.header.y.saturating_add(1),
        stack.header.width,
        crate::ui::ONBOARDING_SUBTITLE,
        muted,
    );

    let content = stack.content;
    for (offset, line) in crate::ui::ONBOARDING_DESCRIPTION.iter().enumerate() {
        put_text(
            b,
            content.x,
            content.y.saturating_add(offset as u16),
            content.width,
            line,
            text,
        );
    }

    let key_y = content.y.saturating_add(4);
    let mut key_x = content.x;
    for (value, style) in [
        ("  ", base),
        (prefix.as_str(), accent),
        (crate::ui::ONBOARDING_PREFIX_SUFFIX, text),
        (crate::ui::ONBOARDING_HELP_LABEL, accent),
        (crate::ui::ONBOARDING_HELP_SUFFIX, text),
    ] {
        let width = display_width(value);
        put_text(
            b,
            key_x,
            key_y,
            content.right().saturating_sub(key_x),
            value,
            style,
        );
        key_x = key_x.saturating_add(width);
    }
    put_text(
        b,
        content.x,
        content.y.saturating_add(5),
        content.width,
        crate::ui::ONBOARDING_NEXT,
        text,
    );

    let primary = crate::ui::onboarding_welcome_continue_rect(stack.actions.unwrap_or_default());
    button(
        b,
        primary,
        " ↵ continue ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );
    Some(OverlayRender {
        area: outer,
        primary,
        ..OverlayRender::default()
    })
}

fn render_rename_overlay(
    b: &mut Buffer,
    v: &ClientRenameOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 56, 7)?;
    let i = panel(b, q, p.accent, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        v.title,
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let input = Rect::new(i.x, i.y + 2, i.width, 1);
    b.set_style(input, Style::default().fg(p.text).bg(p.surface0));
    let cursor = text_editor::render(
        b,
        Rect::new(input.x + 1, input.y, input.width.saturating_sub(1), 1),
        &v.input,
        Style::default().fg(p.text).bg(p.surface0),
    );
    let rs = row(i, &[8, 10, 12], 2, 3);
    let [save, clear, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *save,
        " ↵ save ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let n = Style::default()
        .fg(p.text)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);
    button(b, *clear, " ^c clear ", n);
    button(b, *cancel, " esc cancel ", n);
    Some(OverlayRender {
        area: q,
        primary: *save,
        clear: *clear,
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor,
        ..OverlayRender::default()
    })
}

fn render_navigator_overlay(
    b: &mut Buffer,
    n: &ClientNavigatorOverlay,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    p: &Palette,
) -> Option<OverlayRender> {
    let a = b.area;
    let width = a.width.saturating_sub(4).min(116);
    let height = a.height.saturating_sub(2).min(42);
    if width < 4 || height < 9 {
        return None;
    }
    let q = Rect::new(
        a.x + (a.width - width) / 2,
        a.y + (a.height - height) / 2,
        width,
        height,
    )
    .intersection(a);
    let i = panel(b, q, p.accent, p.panel_bg)?;
    put_text(
        b,
        q.x + 2,
        q.y,
        q.width.saturating_sub(4),
        " Go to ",
        Style::default().fg(p.accent).bg(p.panel_bg),
    );
    let rows = super::aggregate_navigation::navigator_rows(endpoints, active_endpoint_id, n);
    let search = if n.search_focused {
        " / ".to_owned()
    } else if let Some(f) = n.filter {
        format!(
            " / {}",
            match f {
                ClientNavigatorFilter::Blocked => "blocked",
                ClientNavigatorFilter::Working => "working",
                ClientNavigatorFilter::Idle => "idle",
                ClientNavigatorFilter::Done => "done",
            }
        )
    } else if n.query.is_empty() {
        " / search agents and terminals".to_owned()
    } else {
        format!(" / {}", n.query)
    };
    let terminal_count = rows
        .iter()
        .filter(|row| matches!(row.target, ClientNavigatorTarget::Pane { .. }))
        .count();
    let count = format!(
        "{terminal_count} {}",
        if terminal_count == 1 {
            "terminal"
        } else {
            "terminals"
        }
    );
    put_text(
        b,
        i.x,
        i.y,
        i.width.saturating_sub(display_width(&count) + 1),
        &search,
        Style::default()
            .fg(if n.search_focused { p.text } else { p.overlay0 })
            .bg(p.panel_bg),
    );
    let cursor = if n.search_focused {
        text_editor::render(
            b,
            Rect::new(
                i.x + 3,
                i.y,
                i.width.saturating_sub(4 + display_width(&count)),
                1,
            ),
            &n.query,
            Style::default().fg(p.text).bg(p.panel_bg),
        )
    } else {
        None
    };
    put_right_text(
        b,
        i,
        i.y,
        &count,
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &"─".repeat(i.width as usize),
        Style::default().fg(p.surface1).bg(p.panel_bg),
    );
    let body = Rect::new(i.x, i.y + 2, i.width, i.height.saturating_sub(5));
    let selected = super::aggregate_navigation::navigator_selected_index(&rows, n).unwrap_or(0);
    let max = rows.len().saturating_sub(body.height as usize);
    let scroll = n
        .scroll
        .max(selected.saturating_sub(body.height.saturating_sub(1) as usize))
        .min(selected)
        .min(max);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max.saturating_sub(scroll),
        max_offset_from_bottom: max,
        viewport_rows: usize::from(body.height),
    };
    let scrollbar =
        (max > 0 && body.width > 1).then_some(Rect::new(body.right() - 1, body.y, 1, body.height));
    let row_width = body.width.saturating_sub(u16::from(scrollbar.is_some()));
    let mut row_hits = Vec::new();
    if rows.is_empty() {
        put_text(
            b,
            body.x,
            body.y,
            body.width,
            " No matching agents or terminals",
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        );
    }
    for (ix, r) in rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(body.height as usize)
    {
        let rect = Rect::new(body.x, body.y + (ix - scroll) as u16, row_width, 1);
        row_hits.push((rect, r.target.clone()));
        let st = if r.stale {
            Style::default()
                .fg(p.overlay0)
                .bg(if ix == selected {
                    p.surface0
                } else {
                    p.panel_bg
                })
                .add_modifier(Modifier::DIM)
        } else if ix == selected {
            Style::default()
                .fg(contrast(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(
                    if matches!(r.target, ClientNavigatorTarget::Machine { .. }) {
                        p.subtext0
                    } else {
                        p.text
                    },
                )
                .bg(p.panel_bg)
        };
        let is_pane = matches!(r.target, ClientNavigatorTarget::Pane { .. });
        let connector = if !is_pane {
            ""
        } else if rows
            .get(ix + 1)
            .is_some_and(|next| matches!(next.target, ClientNavigatorTarget::Pane { .. }))
        {
            "├─ "
        } else {
            "└─ "
        };
        let padding = u16::from(r.depth.saturating_sub(u8::from(is_pane))) * 2 + 1;
        let connector_x = rect.x + padding;
        let indent = format!("{:width$}{connector}", "", width = usize::from(padding));
        let current = if r.current { "◆ " } else { "" };
        let status = r.status.map(status_dot).unwrap_or_default();
        let status_separator = if status.is_empty() { "" } else { " " };
        let label = format!("{indent}{current}{status}{status_separator}{}", r.label);
        let st = if r.status.is_none() {
            st.add_modifier(Modifier::BOLD)
        } else {
            st
        };
        b.set_style(rect, st);
        let columns = if r.status.is_some() {
            if rect.width >= 64 {
                24
            } else if rect.width >= 36 {
                12
            } else {
                0
            }
        } else {
            0
        };
        put_text(
            b,
            rect.x,
            rect.y,
            rect.width.saturating_sub(columns),
            &label,
            st,
        );
        if is_pane {
            put_text(
                b,
                connector_x,
                rect.y,
                rect.right().saturating_sub(connector_x).min(2),
                connector,
                if r.stale || ix == selected {
                    st
                } else {
                    st.fg(p.overlay0)
                },
            );
        }
        if let Some(status) = r.status {
            let prefix = format!("{indent}{current}");
            let status_style = if r.stale || ix == selected {
                st
            } else {
                Style::default().fg(status_color(status, p)).bg(p.panel_bg)
            };
            put_text(
                b,
                rect.x.saturating_add(display_width(&prefix)),
                rect.y,
                display_width(status_dot(status)),
                status_dot(status),
                status_style,
            );
            let meta_style = if r.stale || ix == selected {
                st
            } else {
                st.fg(p.overlay0)
            };
            if columns > 0 {
                put_text(
                    b,
                    rect.right() - columns + 1,
                    rect.y,
                    11,
                    r.agent.as_deref().unwrap_or("terminal"),
                    meta_style,
                );
            }
            if columns == 24 {
                put_text(
                    b,
                    rect.right() - 11,
                    rect.y,
                    11,
                    if r.agent.is_some() {
                        status_text(status)
                    } else {
                        "shell"
                    },
                    meta_style,
                );
            }
        }
        let machine_status = match &r.target {
            ClientNavigatorTarget::Machine { endpoint_id } if !endpoint_id.is_local() => endpoints
                .iter()
                .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
                .map(|endpoint| endpoint.status),
            _ => None,
        };
        if let Some(status) = machine_status {
            let (glyph, state, color) = endpoint_status_presentation(status, p);
            let signal = if status == ClientEndpointStatus::Online {
                glyph.to_owned()
            } else {
                format!("{glyph} {state}")
            };
            let signal_style = if ix == selected {
                st
            } else {
                Style::default()
                    .fg(color)
                    .bg(p.panel_bg)
                    .add_modifier(if r.stale {
                        Modifier::DIM
                    } else {
                        Modifier::empty()
                    })
            };
            put_right_text(b, rect, rect.y, &signal, signal_style);
        } else if r.status.is_none() && !r.meta.is_empty() {
            let label_width = display_width(&label).min(rect.width);
            let meta = Rect::new(
                rect.x.saturating_add(label_width).saturating_add(1),
                rect.y,
                rect.width.saturating_sub(label_width.saturating_add(1)),
                1,
            );
            put_right_text(b, meta, rect.y, &r.meta, st)
        }
    }
    if let Some(track) = scrollbar {
        crate::ui::render_scrollbar_buffer(b, metrics, track, p.overlay0, p.overlay1, "▐");
    }
    if let Some(r) = rows.get(selected) {
        put_text(
            b,
            i.x,
            i.bottom() - 3,
            i.width,
            &format!(" {}", r.detail),
            Style::default().fg(p.subtext0).bg(p.panel_bg),
        );
        put_text(
            b,
            i.x,
            i.bottom() - 2,
            i.width,
            &format!(" {}", r.meta),
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        );
    }
    put_text(
        b,
        i.x,
        i.bottom() - 1,
        i.width,
        if n.search_focused {
            " search type · move ↑↓/ctrl+n/p · open enter · back esc"
        } else {
            " ↑↓/j/k rows · ←→ workspace · / search · a/b/w/i/d filter · enter open · esc close"
        },
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    Some(OverlayRender {
        area: q,
        primary: Rect::default(),
        clear: Rect::default(),
        cancel: Rect::default(),
        navigator_popup: q,
        navigator_search: Rect::new(i.x, i.y, i.width, 1),
        navigator_rows: row_hits,
        navigator_scrollbar: scrollbar.unwrap_or_default(),
        navigator_scroll_metrics: Some(metrics),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor,
        ..OverlayRender::default()
    })
}

fn help_lines(
    keybinds: &LiveKeybindConfig,
    query: &str,
    palette: &Palette,
) -> Vec<(usize, ratatui::text::Line<'static>)> {
    use ratatui::text::{Line, Span};

    let groups = crate::input::filter_keybind_help_groups(
        crate::input::keybind_help_groups(&keybinds.keybinds, &keybinds.prefix),
        query,
    );
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);
    if groups.is_empty() {
        let message = " no matching keybinds";
        return vec![(
            message.chars().count(),
            Line::from(Span::styled(
                message,
                Style::default().fg(palette.overlay1).bg(palette.panel_bg),
            )),
        )];
    }

    let mut lines = Vec::new();
    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(Span::styled(
                format!(" {group}"),
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD),
            )),
        ));
        for (key, label) in entries {
            let padded_key = format!(" {key:<key_width$} ");
            let width = padded_key.chars().count() + label.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(
                        padded_key,
                        Style::default()
                            .fg(palette.mauve)
                            .bg(palette.panel_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        label.into_owned(),
                        Style::default().fg(palette.text).bg(palette.panel_bg),
                    ),
                ]),
            ));
        }
        lines.push((0, Line::raw("")));
    }
    lines
}

fn render_help_overlay(
    b: &mut Buffer,
    h: &ClientHelpOverlay,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    let q = popup(b.area, 76, 22)?;
    let i = panel(b, q, p.accent, p.panel_bg)?;
    if i.width < 20 || i.height < 6 {
        return None;
    }
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        "keybinds",
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(i.right() - 13, i.y, 13, 1);
    button(
        b,
        close,
        if h.search_focused {
            " esc back "
        } else {
            " esc close "
        },
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let sy = i.y + 1;
    put_text(
        b,
        i.x,
        sy,
        i.width,
        &if h.search_focused {
            " / ".to_owned()
        } else {
            " / press / to filter by command or shortcut".to_owned()
        },
        Style::default()
            .fg(if h.search_focused { p.text } else { p.overlay0 })
            .bg(p.panel_bg),
    );
    let cursor = if h.search_focused {
        text_editor::render(
            b,
            Rect::new(i.x + 3, sy, i.width.saturating_sub(3), 1),
            &h.query,
            Style::default().fg(p.text).bg(p.panel_bg),
        )
    } else {
        None
    };

    let body = Rect::new(i.x, i.y + 3, i.width, i.height.saturating_sub(5));
    let lines = help_lines(k, &h.query, p);
    let viewport_rows = usize::from(body.height.max(1));
    let wrapped_rows = |width: u16| {
        let width = usize::from(width.max(1));
        lines
            .iter()
            .map(|(line_width, _)| line_width.max(&1).div_ceil(width))
            .sum::<usize>()
    };
    let needs_scrollbar = wrapped_rows(body.width) > viewport_rows;
    let text_area = if needs_scrollbar {
        Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height)
    } else {
        body
    };
    let total_rows = wrapped_rows(text_area.width);
    let max_scroll = total_rows.saturating_sub(viewport_rows);
    let scroll = h.scroll.min(max_scroll);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    };
    let scrollbar = needs_scrollbar.then_some(Rect::new(
        body.right().saturating_sub(1),
        body.y,
        1,
        body.height,
    ));
    Widget::render(
        Paragraph::new(lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        text_area,
        b,
    );
    if let Some(track) = scrollbar {
        if let Some(thumb) = crate::ui::scrollbar_thumb(metrics, track) {
            for y in track.y..track.bottom() {
                b[(track.x, y)]
                    .set_symbol("▐")
                    .set_style(Style::default().fg(p.overlay0).bg(p.panel_bg));
            }
            for y in thumb.top..thumb.top.saturating_add(thumb.len) {
                b[(track.x, y)]
                    .set_symbol("▐")
                    .set_style(Style::default().fg(p.overlay1).bg(p.panel_bg));
            }
        }
    }

    put_text(
        b,
        i.x,
        i.bottom() - 1,
        i.width,
        if h.search_focused {
            " edit ←→/home/end · kill ^u/^k · yank ^y · scroll ↑↓ · back esc"
        } else {
            " search / · scroll j/k/↑↓/pgup/pgdn · close esc/enter"
        },
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    Some(OverlayRender {
        area: q,
        cancel: close,
        help_popup: q,
        help_scrollbar: scrollbar.unwrap_or_default(),
        help_scroll_metrics: Some(metrics),
        help_max_scroll: max_scroll,
        cursor,
        ..OverlayRender::default()
    })
}
fn render_confirm_close_overlay(
    b: &mut Buffer,
    c: &ClientConfirmCloseOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let running_rows = u16::from(c.running.is_some());
    let q = popup(b.area, 64, 6 + running_rows)?;
    let i = panel(b, q, p.red, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        &format!(" {}", c.title),
        Style::default()
            .fg(p.red)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &format!(" {}", c.detail),
        Style::default().fg(p.text).bg(p.panel_bg),
    );
    if let Some(running) = &c.running {
        put_text(
            b,
            i.x,
            i.y + 2,
            i.width,
            &format!(" stops: {running}"),
            Style::default().fg(p.yellow).bg(p.panel_bg),
        );
    }
    let rs = row(i, &[13, 12], 2, 3 + running_rows);
    let [ok, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *ok,
        " ↵ confirm ",
        Style::default()
            .fg(contrast(p))
            .bg(p.red)
            .add_modifier(Modifier::BOLD),
    );
    button(
        b,
        *cancel,
        " esc cancel ",
        Style::default()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(OverlayRender {
        area: q,
        primary: *ok,
        clear: Rect::default(),
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: None,
        ..OverlayRender::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_list_names_each_day_once_above_its_rows() {
        let row = |day: &str, time: &str, text: &str| -> LogRow {
            (
                Some(day.to_owned()),
                time.to_owned(),
                text.to_owned(),
                false,
                None,
            )
        };
        let rows = vec![
            row("Today", "00:08", "newest"),
            row("Today", "00:01", "after midnight"),
            row("Oct 2", "23:59", "before midnight"),
        ];
        let mut buffer = Buffer::empty(Rect::new(0, 0, 70, 12));
        let rendered = render_notification_log(
            &mut buffer,
            Rect::new(0, 0, 4, 1),
            None,
            &rows,
            &Palette::catppuccin(),
        )
        .expect("the list fits");
        let lines = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let line_of = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} missing: {lines:#?}"))
        };
        // One separator per day, right above its first row, and the times
        // carry no date.
        assert_eq!(
            lines.iter().filter(|line| line.contains("Today")).count(),
            1
        );
        assert_eq!(line_of("Today") + 1, line_of("00:08 newest"));
        assert_eq!(line_of("00:01 after midnight") + 1, line_of("Oct 2"));
        assert_eq!(line_of("Oct 2") + 1, line_of("23:59 before midnight"));
        // Separators are not rows: the hits map each notification to its own
        // line, and the box is tall enough for the two separators.
        let hit_lines = rendered
            .menu_rows
            .iter()
            .map(|(rect, index)| (rect.y as usize, *index))
            .collect::<Vec<_>>();
        assert_eq!(
            hit_lines,
            vec![
                (line_of("newest"), 0),
                (line_of("after midnight"), 1),
                (line_of("before midnight"), 2),
            ]
        );
        assert_eq!(rendered.area.height, 3 + 2 + 2);
    }

    #[test]
    fn reset_credits_list_live_expiries_soonest_first() {
        let credit =
            |id: &str, title: &str, expires_at: u64| crate::api::schema::UsageResetCredit {
                id: id.into(),
                kind: "codexRateLimits".into(),
                title: title.into(),
                expires_at: Some(expires_at),
            };
        let now = 1_790_263_799; // 2026-09-24T15:29:59Z, a Thursday.
        let mut codex = crate::api::schema::ProviderUsage::pending("codex", "Codex");
        codex.reset_credits = vec![
            credit("expired", "Full reset", now - 1),
            credit("soon", "Full reset", now + 30 * 3_600),
            credit("later", "Full reset", now + 20 * 86_400),
        ];
        let text = |lines: Vec<ratatui::text::Line<'static>>| {
            lines.iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        let p = Palette::catppuccin();

        let lines = reset_credit_lines(&codex, now, 0, Style::default(), &p);
        assert_eq!(lines[1].spans[0].style.fg, Some(p.yellow));
        assert_eq!(lines[2].spans[0].style.fg, Some(p.overlay0));
        assert_eq!(
            text(lines),
            vec![
                "   2 reset credits · Full reset · redeem in Codex",
                "     expires in 1d 6h (Fri 21:29)",
                "     expires in 20d (Oct 14 15:29)",
            ]
        );

        codex.reset_credits[2].title = "5 hr reset".into();
        assert_eq!(
            text(reset_credit_lines(&codex, now, 0, Style::default(), &p)),
            vec![
                "   2 reset credits · redeem in Codex",
                "     Full reset: expires in 1d 6h (Fri 21:29)",
                "     5 hr reset: expires in 20d (Oct 14 15:29)",
            ]
        );

        codex.reset_credits.truncate(1);
        assert!(reset_credit_lines(&codex, now, 0, Style::default(), &p).is_empty());
    }
}
