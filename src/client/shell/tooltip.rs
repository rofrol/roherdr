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
        let text = sanitize(&target.text);
        let area = buffer.area;
        let width = (unicode_width::UnicodeWidthStr::width(text.as_str()) as u16)
            .saturating_add(2)
            .min(area.width);
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
        let rect = Rect::new(x, target.rect.y, width, 1).intersection(area);
        let palette = &self.config.palette;
        let bg = target.bg.unwrap_or(palette.surface1);
        let fg = if bg == palette.accent {
            panel_contrast_fg(palette)
        } else {
            palette.text
        };
        let style = Style::default().fg(fg).bg(bg);
        buffer.set_style(rect, style);
        for x in rect.left()..rect.right() {
            buffer[(x, rect.y)].set_symbol(" ");
        }
        super::render::put_text(
            buffer,
            rect.x.saturating_add(1),
            rect.y,
            rect.width.saturating_sub(2),
            &text,
            style,
        );
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
