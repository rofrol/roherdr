//! Sidebar footer row with pseudo-terminal usage: `PTY 65 · sys ~108/511`.
//!
//! The numbers come with the snapshot the server already sends (the server
//! samples the system figure); the client never asks for them.

use crate::protocol::ClientShellPtyUsage;
use crate::pty::usage::{AMBER_PERCENT, RED_PERCENT};

use super::render::put_segment;
use super::*;

/// Columns at the right kept for the sidebar toggle and divider, as the
/// build row keeps them.
const RIGHT_RESERVE: u16 = 3;

/// Herdr's exact count first; the system figure gets `~` when the platform
/// only estimates it.
pub(super) fn footer_text(usage: &ClientShellPtyUsage) -> String {
    match usage.system {
        Some(system) => format!(
            "PTY {} · sys {}{}/{}",
            usage.herdr,
            if system.exact { "" } else { "~" },
            system.in_use,
            system.max
        ),
        None => format!("PTY {}", usage.herdr),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PtyLevel {
    Calm,
    Amber,
    Red,
}

/// How full the system pool is; calm where its size is unknown.
pub(super) fn level(usage: &ClientShellPtyUsage) -> PtyLevel {
    let Some(percent) = usage
        .system
        .filter(|system| system.max > 0)
        .map(|system| u64::from(system.in_use) * 100 / u64::from(system.max))
    else {
        return PtyLevel::Calm;
    };
    if percent >= u64::from(RED_PERCENT) {
        PtyLevel::Red
    } else if percent >= u64::from(AMBER_PERCENT) {
        PtyLevel::Amber
    } else {
        PtyLevel::Calm
    }
}

/// Splits the last row of `area` off for the usage line, when the server
/// sent one and there is room left for the sections.
pub(super) fn split_row(area: Rect, usage: Option<&ClientShellPtyUsage>) -> (Rect, Rect) {
    if usage.is_none() || area.height < 2 {
        return (area, Rect::default());
    }
    let sections = Rect::new(area.x, area.y, area.width, area.height - 1);
    let row = Rect::new(area.x, sections.bottom(), area.width, 1);
    (sections, row)
}

pub(super) fn render_row(
    buffer: &mut Buffer,
    area: Rect,
    usage: &ClientShellPtyUsage,
    palette: &Palette,
) {
    if area.is_empty() {
        return;
    }
    let right = area.right().saturating_sub(RIGHT_RESERVE);
    let x = area.x.saturating_add(1);
    let room = right.saturating_sub(x);
    if room == 0 {
        return;
    }
    let color = match level(usage) {
        PtyLevel::Calm => palette.overlay0,
        PtyLevel::Amber => palette.peach,
        PtyLevel::Red => palette.red,
    };
    put_segment(
        buffer,
        x,
        area.y,
        right,
        &crate::ui::truncate_end(&footer_text(usage), room as usize),
        Style::default().fg(color),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientShellSystemPtyUsage;

    fn usage(in_use: u32, exact: bool) -> ClientShellPtyUsage {
        ClientShellPtyUsage {
            herdr: 65,
            system: Some(ClientShellSystemPtyUsage {
                in_use,
                max: 100,
                exact,
            }),
        }
    }

    #[test]
    fn text_puts_herdrs_count_first_and_marks_an_estimate() {
        let mut estimate = usage(108, false);
        if let Some(system) = estimate.system.as_mut() {
            system.max = 511;
        }
        assert_eq!(footer_text(&estimate), "PTY 65 · sys ~108/511");
        assert_eq!(footer_text(&usage(40, true)), "PTY 65 · sys 40/100");
        let windows = ClientShellPtyUsage {
            herdr: 3,
            system: None,
        };
        assert_eq!(footer_text(&windows), "PTY 3");
    }

    #[test]
    fn color_follows_the_system_pool() {
        assert_eq!(level(&usage(69, false)), PtyLevel::Calm);
        assert_eq!(level(&usage(70, false)), PtyLevel::Amber);
        assert_eq!(level(&usage(89, false)), PtyLevel::Amber);
        assert_eq!(level(&usage(90, false)), PtyLevel::Red);
        let unknown = ClientShellPtyUsage {
            herdr: 500,
            system: None,
        };
        assert_eq!(level(&unknown), PtyLevel::Calm);
    }

    #[test]
    fn row_is_taken_only_with_usage_and_room() {
        let area = Rect::new(0, 0, 26, 10);
        assert_eq!(split_row(area, None), (area, Rect::default()));
        let (sections, row) = split_row(area, Some(&usage(1, true)));
        assert_eq!(sections, Rect::new(0, 0, 26, 9));
        assert_eq!(row, Rect::new(0, 9, 26, 1));
        let tiny = Rect::new(0, 0, 26, 1);
        assert_eq!(split_row(tiny, Some(&usage(1, true))).1, Rect::default());
    }

    #[test]
    fn row_is_dim_amber_or_red() {
        let palette = ClientShellConfig::from_config(&Config::default()).palette;
        let area = Rect::new(0, 0, 30, 1);
        for (in_use, color) in [
            (10, palette.overlay0),
            (75, palette.peach),
            (95, palette.red),
        ] {
            let mut buffer = Buffer::empty(area);
            render_row(&mut buffer, area, &usage(in_use, false), &palette);
            let text: String = (0..area.width)
                .map(|x| buffer[(x, 0)].symbol().to_string())
                .collect();
            assert!(
                text.starts_with(&format!(" PTY 65 · sys ~{in_use}/100")),
                "{text:?}"
            );
            assert_eq!(buffer[(1, 0)].fg, color);
        }
    }

    #[test]
    fn narrow_row_is_truncated_before_the_toggle() {
        let palette = ClientShellConfig::from_config(&Config::default()).palette;
        let area = Rect::new(0, 0, 12, 1);
        let mut buffer = Buffer::empty(area);
        render_row(&mut buffer, area, &usage(10, false), &palette);
        for x in area.right() - RIGHT_RESERVE..area.right() {
            assert_eq!(buffer[(x, 0)].symbol(), " ");
        }
    }
}
