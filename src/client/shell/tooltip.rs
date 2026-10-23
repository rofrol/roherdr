//! Tooltips: hovering a target whose text is cut shows it whole, after a
//! short dwell. Client presentation state only. A tooltip is drawn last,
//! over panes too, takes no clicks, and goes on a key, a click, a scroll, a
//! drag, an overlay, when its target is no longer drawn, and after a while
//! (a lost leave event must not leave it stuck). Hover is never the only way
//! to see the text: terminals and tmux can drop plain motion events.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::Style;

use super::*;

/// How long the pointer rests on a target before its tooltip shows.
const DWELL: Duration = Duration::from_millis(450);
/// How long a tooltip stays without another move.
const MAX_SHOWN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Tooltip {
    target: String,
    since: Instant,
    pub(super) shown: bool,
}

/// A drawn target that has a tooltip: its rect, an id stable across frames,
/// and the full text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TooltipTarget {
    pub(super) rect: Rect,
    pub(super) id: String,
    pub(super) text: String,
    /// The tooltip's fill; the default is `surface1`.
    pub(super) bg: Option<ratatui::style::Color>,
    /// The box starts at the target's left edge, without a padding column,
    /// and its text is cut at the screen edge instead of moving the box
    /// left, so a mark just left of the target (the active tab's accent
    /// bar, the state icon) stays visible.
    pub(super) starts_at_target: bool,
    /// Each `\n` in the text starts another row of the box, which grows
    /// down from the target's row and moves up to stay on screen. Without
    /// it a label's line breaks are spaces.
    pub(super) multiline: bool,
}

impl ClientShellState {
    /// Follows the pointer: a plain move onto a target starts its dwell (a
    /// move within the same target keeps it); anything else hides it.
    pub(super) fn update_tooltip(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) {
        let point = (mouse.column, mouse.row);
        let target = (mouse.kind == crossterm::event::MouseEventKind::Moved
            && self.overlay.is_none())
        .then(|| {
            self.hovered_square
                .as_deref()
                .map(square_tooltip_id)
                .or_else(|| {
                    self.hits
                        .tooltips
                        .iter()
                        .find(|target| super::contains(target.rect, point))
                        .map(|target| target.id.clone())
                })
        })
        .flatten();
        match target {
            Some(id) if self.tooltip.as_ref().is_some_and(|tip| tip.target == id) => {}
            Some(id) => {
                outcome.repaint |= self.tooltip.as_ref().is_some_and(|tip| tip.shown);
                self.tooltip = Some(Tooltip {
                    target: id,
                    since: Instant::now(),
                    shown: false,
                });
            }
            None => outcome.repaint |= self.clear_tooltip(),
        }
    }

    /// Hides the tooltip; whether one was shown.
    pub(super) fn clear_tooltip(&mut self) -> bool {
        self.tooltip.take().is_some_and(|tip| tip.shown)
    }

    /// Shows a tooltip once its dwell is over, and hides it after
    /// [`MAX_SHOWN`].
    pub(super) fn tick_tooltip(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        if self.overlay.is_some()
            || self.tooltip.as_ref().is_some_and(|tip| {
                !self
                    .hits
                    .tooltips
                    .iter()
                    .any(|target| target.id == tip.target)
            })
        {
            outcome.repaint |= self.clear_tooltip();
            return;
        }
        let Some(tip) = self.tooltip.as_mut() else {
            return;
        };
        if !tip.shown && now >= tip.since + DWELL {
            tip.shown = true;
            tip.since = now;
            outcome.repaint = true;
        } else if tip.shown && now >= tip.since + MAX_SHOWN {
            self.tooltip = None;
            outcome.repaint = true;
        }
    }

    /// When the tooltip next needs a tick.
    pub(super) fn tooltip_deadline(&self) -> Option<Instant> {
        self.tooltip
            .as_ref()
            .map(|tip| tip.since + if tip.shown { MAX_SHOWN } else { DWELL })
    }

    /// Whether a tooltip has completed its dwell.
    pub(super) fn tooltip_visible(&self) -> bool {
        self.tooltip.as_ref().is_some_and(|tip| tip.shown)
    }

    /// Draws the shown tooltip on the row of its target, from the target's
    /// left edge (right edge for job squares), shifted left to stay on screen.
    pub(super) fn render_tooltip(&self, buffer: &mut ratatui::buffer::Buffer) -> Option<Rect> {
        let tip = self.tooltip.as_ref().filter(|tip| tip.shown)?;
        let target = self
            .hits
            .tooltips
            .iter()
            .find(|target| target.id == tip.target)?;
        let lines = if target.multiline {
            target.text.split('\n').map(sanitize).collect::<Vec<_>>()
        } else {
            vec![sanitize(&target.text)]
        };
        let area = buffer.area;
        let left_pad = u16::from(!target.starts_at_target);
        let natural = (lines
            .iter()
            .map(|line| unicode_width::UnicodeWidthStr::width(line.as_str()))
            .max()
            .unwrap_or(0) as u16)
            .saturating_add(left_pad + 1);
        let (x, width, lines) = if target.starts_at_target {
            let x = target.rect.x.min(area.right());
            let width = natural.min(area.right() - x);
            let lines = lines
                .iter()
                .map(|line| super::space_tabs::truncate(line, width.saturating_sub(1) as usize))
                .collect();
            (x, width, lines)
        } else {
            let width = natural.min(area.width);
            // The box's padding column sits left of the target, so the text
            // starts where the target's text does.
            let anchor_x = if target.id.starts_with("square:") {
                target.rect.right()
            } else {
                target.rect.x
            };
            let x = anchor_x
                .saturating_sub(1)
                .min(area.right().saturating_sub(width));
            (x, width, lines)
        };
        let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let y = target
            .rect
            .y
            .min(area.bottom().saturating_sub(height))
            .max(area.y);
        let rect = Rect::new(x, y, width, height).intersection(area);
        let palette = &self.config.palette;
        let bg = target.bg.unwrap_or(palette.surface1);
        let fg = if bg == palette.accent {
            panel_contrast_fg(palette)
        } else {
            palette.text
        };
        let style = Style::default().fg(fg).bg(bg);
        buffer.set_style(rect, style);
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                buffer[(x, y)].set_symbol(" ");
            }
        }
        for (y, line) in (rect.top()..rect.bottom()).zip(&lines) {
            super::render::put_text(
                buffer,
                rect.x.saturating_add(left_pad),
                y,
                rect.width.saturating_sub(left_pad + 1),
                line,
                style,
            );
        }
        Some(rect)
    }
}

/// The tooltip id of a job square's name.
pub(super) fn square_tooltip_id(tab_id: &str) -> String {
    format!("square:{tab_id}")
}

/// `text` on one line, with control characters (a label may carry them)
/// replaced by spaces.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown_tooltip(text: &str, multiline: bool, y: u16) -> Vec<String> {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        state.hits.tooltips.push(TooltipTarget {
            rect: Rect::new(4, y, 1, 1),
            id: "usage:claude:day:0".into(),
            text: text.into(),
            bg: None,
            starts_at_target: false,
            multiline,
        });
        state.tooltip = Some(Tooltip {
            target: "usage:claude:day:0".into(),
            since: Instant::now(),
            shown: true,
        });
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 30, 6));
        state.render_tooltip(&mut buffer);
        (0..6)
            .map(|row| {
                (0..30)
                    .map(|x| buffer[(x, row)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn a_multiline_tooltip_draws_a_row_per_line_and_stays_on_screen() {
        let rows = shown_tooltip("first line\nsecond", true, 1);
        assert_eq!(rows[1], "    first line");
        assert_eq!(rows[2], "    second");
        assert_eq!(rows[3], "");
        // At the bottom row the box moves up so both lines show.
        let rows = shown_tooltip("first line\nsecond", true, 5);
        assert_eq!(rows[4], "    first line");
        assert_eq!(rows[5], "    second");
        // Other tooltips keep a label's line break on one row.
        let rows = shown_tooltip("a\nb", false, 1);
        assert_eq!(rows[1], "    a b");
        assert_eq!(rows[2], "");
    }
}
