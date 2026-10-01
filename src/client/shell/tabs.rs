use super::super::tab_groups;
use super::*;

const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;
const MIN_TAB_STRIP_WIDTH: u16 =
    MIN_TAB_WIDTH + NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2);

pub(crate) fn render_tab_bar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    stale: bool,
    tab_scroll: &mut usize,
    reveal_focused_tab: &mut bool,
    tab_drag_insert_index: Option<usize>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    // Child tabs have their own row; a parent shows a summary of them.
    let tabs = tab_groups::main_row_tabs(snapshot);
    let active_tab_id = tab_groups::active_main_tab_id(snapshot);
    let labels = tabs
        .iter()
        .map(|tab| {
            let summary =
                tab_groups::children_summary(&tab_groups::child_tabs(snapshot, &tab.tab_id));
            let label = match tab_state_icon(tab, config) {
                Some(icon) => format!("{icon} {}", tab_label(tab, snapshot, config)),
                None => tab_label(tab, snapshot, config),
            };
            if summary.is_empty() {
                label
            } else {
                format!("{label} {summary}")
            }
        })
        .collect::<Vec<_>>();
    let desired_widths = labels
        .iter()
        .map(|label| display_width(label).saturating_add(4).max(MIN_TAB_WIDTH))
        .collect::<Vec<_>>();
    let content = tab_bar_content_area(snapshot, area);
    let mouse_chrome = config.mouse_capture;
    let new_tab_width = if mouse_chrome { NEW_TAB_WIDTH } else { 0 };
    let desired_total = desired_widths
        .iter()
        .copied()
        .fold(0_u16, u16::saturating_add)
        .saturating_add(tabs.len().saturating_sub(1).min(u16::MAX as usize) as u16)
        .saturating_add(new_tab_width);
    let overflow =
        desired_total > content.width && (!mouse_chrome || content.width >= MIN_TAB_STRIP_WIDTH);
    let available = if overflow && mouse_chrome {
        content
            .width
            .saturating_sub(NEW_TAB_WIDTH)
            .saturating_sub(TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2))
    } else {
        content.width.saturating_sub(new_tab_width)
    };
    let max_scroll = max_tab_scroll(&desired_widths, available);
    if !overflow {
        *tab_scroll = 0;
    } else if *reveal_focused_tab {
        if let Some(focused) = tabs
            .iter()
            .position(|tab| Some(tab.tab_id.as_str()) == active_tab_id)
        {
            *tab_scroll = centered_tab_scroll(focused, &desired_widths, available).min(max_scroll);
        }
    } else {
        *tab_scroll = (*tab_scroll).min(max_scroll);
    }
    *reveal_focused_tab = false;

    let mut x = content.x;
    let tab_right = if overflow && mouse_chrome {
        hits.tab_scroll_left = Rect::new(
            content.x,
            content.y,
            TAB_SCROLL_BUTTON_WIDTH.min(content.width),
            1,
        );
        put_text(
            buffer,
            hits.tab_scroll_left.x,
            content.y,
            hits.tab_scroll_left.width,
            " < ",
            Style::default()
                .fg(if *tab_scroll > 0 {
                    palette.overlay1
                } else {
                    palette.overlay0
                })
                .bg(palette.surface0),
        );
        x = hits.tab_scroll_left.right();
        content
            .right()
            .saturating_sub(NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH)
    } else {
        content.right().saturating_sub(new_tab_width)
    };

    let mut first_visible = None;
    let mut last_visible = None;
    for (index, tab) in tabs.iter().enumerate().skip(*tab_scroll) {
        let name = labels[index].clone();
        let desired = desired_widths[index];
        let remaining = tab_right.saturating_sub(x);
        let width = desired.min(remaining);
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        // Full accent marks what is on screen. A parent whose children fill the
        // second row is only tinted there, like a folder tab opening into it,
        // so the bar never shows two accent blocks.
        let accent_filled = Some(tab.tab_id.as_str()) == active_tab_id
            && tab_groups::child_tabs(snapshot, &tab.tab_id).is_empty();
        let style = if Some(tab.tab_id.as_str()) != active_tab_id {
            Style::default().fg(palette.overlay1).bg(palette.surface0)
        } else if accent_filled {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(palette.accent)
                .bg(accent_tint(palette))
                .add_modifier(Modifier::BOLD)
        };
        let padding = width.saturating_sub(display_width(&name));
        let left = padding / 2;
        let text = format!(
            "{empty:left$}{name}{empty:right_padding$}",
            empty = "",
            left = left as usize,
            right_padding = padding.saturating_sub(left) as usize,
        );
        put_text(buffer, rect.x, rect.y, rect.width, &text, style);
        // Color the agent state like the sidebar does, except on the accent
        // fill, where the palette's state colors can vanish. A disconnected
        // endpoint's state is stale, so it is dimmed like in the sidebar.
        if let Some(icon) = tab_state_icon(tab, config) {
            let fg = if stale {
                Some(palette.overlay0)
            } else {
                (!accent_filled).then(|| status_color(tab.agent_status, palette))
            };
            if let (Some(fg), true) = (fg, left < rect.width) {
                put_text(
                    buffer,
                    rect.x + left,
                    rect.y,
                    rect.width - left,
                    icon,
                    style.fg(fg),
                );
            }
        }
        hits.tabs.push((rect, tab.tab_id.clone()));
        first_visible.get_or_insert(index);
        last_visible = Some(index);
        x = x.saturating_add(width + 1);
        if width < desired {
            break;
        }
    }

    if overflow && mouse_chrome {
        hits.tab_scroll_right = Rect::new(tab_right, area.y, TAB_SCROLL_BUTTON_WIDTH, 1);
        let can_scroll_right = *tab_scroll < max_scroll;
        put_text(
            buffer,
            hits.tab_scroll_right.x,
            area.y,
            hits.tab_scroll_right.width,
            " > ",
            Style::default()
                .fg(if can_scroll_right {
                    palette.overlay1
                } else {
                    palette.overlay0
                })
                .bg(palette.surface0),
        );
        hits.new_tab = Rect::new(
            hits.tab_scroll_right.right(),
            area.y,
            content
                .right()
                .saturating_sub(hits.tab_scroll_right.right())
                .min(NEW_TAB_WIDTH),
            1,
        );
    } else if mouse_chrome {
        hits.new_tab = Rect::new(
            x.min(content.right()),
            area.y,
            content.right().saturating_sub(x).min(NEW_TAB_WIDTH),
            1,
        );
    }
    if mouse_chrome {
        put_text(
            buffer,
            hits.new_tab.x,
            area.y,
            hits.new_tab.width,
            " + ",
            Style::default().fg(palette.overlay1).bg(palette.panel_bg),
        );
    }

    if first_visible.is_some_and(|index| index > 0) {
        let ellipsis_x = if hits.tab_scroll_left.width > 0 {
            hits.tab_scroll_left.right()
        } else {
            content.x
        };
        put_text(
            buffer,
            ellipsis_x,
            area.y,
            u16::from(ellipsis_x < content.right()),
            "…",
            Style::default().fg(palette.overlay0),
        );
    }
    if last_visible.is_some_and(|index| index + 1 < tabs.len()) {
        let ellipsis_x = if hits.tab_scroll_right.width > 0 {
            hits.tab_scroll_right.x.saturating_sub(1)
        } else {
            content.right().saturating_sub(1)
        };
        put_text(
            buffer,
            ellipsis_x,
            area.y,
            u16::from(ellipsis_x >= content.x && ellipsis_x < content.right()),
            "…",
            Style::default().fg(palette.overlay0),
        );
    }

    if let Some(insert_index) = tab_drag_insert_index {
        if let Some(indicator_x) = tab_drop_indicator_x(hits, &tabs, insert_index) {
            put_text(
                buffer,
                indicator_x.min(content.right().saturating_sub(1)),
                area.y,
                1,
                "│",
                Style::default().fg(palette.accent),
            );
        }
    }
    render_tab_bar_status(buffer, area, snapshot, palette);
}

/// The second row: the active tab's own content, marked as the parent, then
/// its children, each with its status icon. It stays empty while the active
/// tab has no children.
/// It has no new-tab button or drag and drop; tabs past the edge are cut
/// off with `…`, starting from the focused one when it would not fit.
pub(crate) fn render_child_tab_bar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    // The parent's own content comes first, so exactly one entry of the row is
    // the tab on screen.
    let tabs = tab_groups::active_row_entries(snapshot);
    if tabs.is_empty() {
        return;
    }
    let labels = tabs
        .iter()
        .map(|tab| {
            if tab.parent_tab_id.is_none() {
                return format!("◆ {}", tab_groups::parent_entry_label(snapshot, tab));
            }
            match tab_groups::status_icon(tab.status) {
                Some(icon) => format!("{icon} {}", tab_label(tab, snapshot, config)),
                None => tab_label(tab, snapshot, config),
            }
        })
        .collect::<Vec<_>>();
    let widths = labels
        .iter()
        .map(|label| display_width(label).saturating_add(2))
        .collect::<Vec<_>>();
    // The right-hand status stays in the main row, so children get the full
    // width. The row shares the tint of its parent tab above, and the entry on
    // screen is the only one in the accent colour.
    let band = accent_tint(palette);
    buffer.set_style(area, Style::default().bg(band));
    let content = Rect {
        x: area.x.saturating_add(1),
        width: area.width.saturating_sub(1),
        ..area
    };
    let focused = tabs.iter().position(|tab| tab.focused).unwrap_or(0);
    let mut first = 0;
    while first < focused
        && widths[first..=focused]
            .iter()
            .fold(0_u16, |sum, width| sum.saturating_add(width + 1))
            > content.width
    {
        first += 1;
    }
    let mut x = content.x;
    if first > 0 {
        put_text(
            buffer,
            x,
            area.y,
            1,
            "…",
            Style::default().fg(palette.overlay0),
        );
        x = x.saturating_add(2);
    }
    for (index, tab) in tabs.iter().enumerate().skip(first) {
        let remaining = content.right().saturating_sub(x);
        if remaining == 0 {
            break;
        }
        let width = widths[index].min(remaining);
        let rect = Rect::new(x, area.y, width, 1);
        let style = if tab.focused {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
        } else {
            Style::default().fg(palette.overlay1).bg(band)
        };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(" {} ", labels[index]),
            style,
        );
        hits.child_tabs.push((rect, tab.tab_id.clone()));
        x = x.saturating_add(width);
        // A divider in the gap after the parent sets it apart from its children.
        if tab.parent_tab_id.is_none() && x < content.right() {
            put_text(
                buffer,
                x,
                area.y,
                1,
                "│",
                Style::default().fg(palette.overlay0).bg(band),
            );
        }
        x = x.saturating_add(1);
        if width < widths[index] {
            put_text(
                buffer,
                content.right().saturating_sub(1),
                area.y,
                1,
                "…",
                Style::default().fg(palette.overlay0),
            );
            break;
        }
    }
}

/// A pale accent for the active parent tab and its child row: the accent
/// mixed into the tab bar background, or a surface colour when either is not
/// an RGB colour.
fn accent_tint(palette: &Palette) -> ratatui::style::Color {
    use ratatui::style::Color;
    match (palette.accent, palette.panel_bg) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix = |accent: u8, base: u8| {
                let accent = u16::from(accent);
                let base = u16::from(base);
                // A quarter of the accent over the background.
                ((accent + base * 3 + 2) / 4) as u8
            };
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ => palette.surface1,
    }
}

pub(crate) fn tab_bar_status_width(snapshot: &ClientShellSnapshot) -> u16 {
    let content = snapshot.tab_bar_right.iter().fold(0u16, |width, segment| {
        width.saturating_add(display_width(&segment.text))
    });
    let separators = snapshot.tab_bar_right.len().saturating_sub(1);
    content.saturating_add(
        display_width(&snapshot.tab_bar_right_separator)
            .saturating_mul(separators.min(u16::MAX as usize) as u16),
    )
}

fn tab_bar_status_area(snapshot: &ClientShellSnapshot, area: Rect) -> Option<Rect> {
    let width = tab_bar_status_width(snapshot);
    if width == 0 {
        return None;
    }
    let reserved = width.saturating_add(1);
    (area.width.saturating_sub(reserved) >= MIN_TAB_STRIP_WIDTH)
        .then(|| Rect::new(area.right().saturating_sub(width), area.y, width, 1))
}

fn tab_bar_content_area(snapshot: &ClientShellSnapshot, area: Rect) -> Rect {
    let reserved = tab_bar_status_area(snapshot, area)
        .map(|status| status.width.saturating_add(1))
        .unwrap_or(0);
    Rect {
        width: area.width.saturating_sub(reserved),
        ..area
    }
}

fn render_tab_bar_status(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
) {
    let Some(status) = tab_bar_status_area(snapshot, area) else {
        return;
    };
    let separator_width = display_width(&snapshot.tab_bar_right_separator);
    let mut x = status.x;
    for (index, segment) in snapshot.tab_bar_right.iter().enumerate() {
        if index > 0 && separator_width > 0 {
            put_text(
                buffer,
                x,
                area.y,
                separator_width,
                &snapshot.tab_bar_right_separator,
                Style::default().fg(palette.overlay0).bg(palette.panel_bg),
            );
            x = x.saturating_add(separator_width);
        }
        let width = display_width(&segment.text);
        let style = if segment.accent {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay1).bg(palette.panel_bg)
        };
        put_text(buffer, x, area.y, width, &segment.text, style);
        x = x.saturating_add(width);
    }
}

fn tab_drop_indicator_x(
    hits: &ShellHitMap,
    tabs: &[&ClientShellTab],
    insert_index: usize,
) -> Option<u16> {
    let visible = hits
        .tabs
        .iter()
        .filter_map(|(rect, tab_id)| {
            tabs.iter()
                .position(|tab| tab.tab_id == *tab_id)
                .map(|index| (index, *rect))
        })
        .collect::<Vec<_>>();
    let (first_index, first_rect) = *visible.first()?;
    let (last_index, last_rect) = *visible.last()?;
    if insert_index == 0 {
        return Some(if first_index == 0 {
            first_rect.x
        } else {
            hits.tab_scroll_left.right()
        });
    }
    if let Some((_, rect)) = visible.iter().find(|(index, _)| *index == insert_index) {
        return Some(rect.x.saturating_sub(1));
    }
    if insert_index >= tabs.len() {
        return Some(if last_index + 1 >= tabs.len() {
            last_rect.right()
        } else {
            hits.tab_scroll_right.x.saturating_sub(1)
        });
    }
    None
}

fn centered_tab_scroll(focused: usize, widths: &[u16], available: u16) -> usize {
    let mut best = focused;
    let mut best_distance = u16::MAX;
    for start in 0..=focused {
        let before = widths
            .iter()
            .copied()
            .enumerate()
            .skip(start)
            .take(focused.saturating_sub(start))
            .fold(0u16, |width, (_, tab)| width.saturating_add(tab + 1));
        if before >= available {
            continue;
        }
        let focused_width = widths[focused].min(available.saturating_sub(before));
        let center = before.saturating_mul(2).saturating_add(focused_width);
        let distance = center.abs_diff(available);
        if distance <= best_distance {
            best_distance = distance;
            best = start;
        }
    }
    best
}

fn max_tab_scroll(widths: &[u16], available: u16) -> usize {
    let Some((&last, preceding)) = widths.split_last() else {
        return 0;
    };
    let mut start = preceding.len();
    let mut used = u32::from(last);
    // Keep the longest fully visible suffix, not merely a sliver of the last tab.
    // An oversized last tab must still be reachable at the start of the strip.
    for width in preceding.iter().rev() {
        let required = used + 1 + u32::from(*width);
        if required > u32::from(available) {
            break;
        }
        used = required;
        start -= 1;
    }
    start
}

/// The tab's agent state in the sidebar's indicator style; none for tabs
/// without a detected agent.
fn tab_state_icon(tab: &ClientShellTab, config: &ClientShellConfig) -> Option<&'static str> {
    (tab.agent_status != crate::api::schema::AgentStatus::Unknown)
        .then(|| status_icon(tab.agent_status, config.status_indicators))
}

/// Task titles are model-written sentences; a fixed width keeps the tab bar
/// from shifting every time an agent retitles itself.
const TAB_TITLE_WIDTH: usize = 16;

fn tab_label(
    tab: &ClientShellTab,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
) -> String {
    let label = match agent_task_title(tab, snapshot, config) {
        Some(title) => {
            let title = crate::ui::truncate_end(title, TAB_TITLE_WIDTH);
            let pad = TAB_TITLE_WIDTH.saturating_sub(display_width(&title) as usize);
            format!("{title}{:pad$}", "")
        }
        None => tab.label.clone(),
    };
    if tab.zoomed {
        format!("{label} Z")
    } else {
        label
    }
}

/// With `ui.tab_label = "title"`, an unnamed tab shows the terminal title of
/// its focused agent (else its first one); names given by the user win.
fn agent_task_title<'a>(
    tab: &ClientShellTab,
    snapshot: &'a ClientShellSnapshot,
    config: &ClientShellConfig,
) -> Option<&'a str> {
    if config.tab_label != crate::config::TabLabelConfig::Title || tab.custom_label {
        return None;
    }
    let mut agents = snapshot
        .agents
        .iter()
        .filter(|agent| agent.tab_id == tab.tab_id);
    let first = agents.next()?;
    let agent = if first.focused {
        first
    } else {
        agents.find(|agent| agent.focused).unwrap_or(first)
    };
    agent
        .terminal_title_stripped
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
}

#[cfg(test)]
mod tests {
    use super::max_tab_scroll;

    #[test]
    fn trailing_scroll_limit_accounts_for_full_widths_and_separators() {
        for (widths, available, expected) in [
            (&[][..], 0, 0),
            (&[8, 13][..], 0, 1),
            (&[8, 13][..], 1, 1),
            (&[8, 13][..], 12, 1),
            (&[8, 13][..], 21, 1),
            (&[8, 13][..], 22, 0),
            (&[8, 13][..], 30, 0),
            (&[8, u16::MAX][..], u16::MAX, 1),
        ] {
            assert_eq!(
                max_tab_scroll(widths, available),
                expected,
                "widths={widths:?}, available={available}"
            );
        }
    }
}
