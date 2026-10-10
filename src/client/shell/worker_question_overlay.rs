//! Draws a worker question's dialog
//! ([`crate::client::shell::worker_question`]).

use super::*;
use crate::client::shell::worker_question::{
    WorkerQuestionButton, WorkerQuestionLine, WorkerQuestionView,
};
use unicode_width::UnicodeWidthChar;

const MODAL_WIDTH: u16 = 100;

/// Where the dialog drew what a click or a key acts on.
#[derive(Debug, Default)]
pub(crate) struct WorkerQuestionRender {
    pub(crate) popup: Rect,
    pub(crate) buttons: Vec<(Rect, WorkerQuestionButton)>,
    /// Rows of input the body can scroll past.
    pub(crate) max_scroll: usize,
    /// Rows the body shows, a page.
    pub(crate) body_rows: usize,
    pub(crate) cursor: Option<crate::protocol::CursorState>,
}

/// One drawn row of the body.
struct BodyRow {
    text: String,
    /// The row continues the line above it.
    continued: bool,
    kind: RowKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Text,
    Heading,
    Option(WorkerQuestionButton, bool),
}

/// Shows a character the way it is: a tab as spaces, another control
/// character as its control picture (`␍`), so none is lost or moves the
/// text.
fn shown_char(c: char, out: &mut String) {
    match c {
        '\t' => out.push_str("    "),
        c if (c as u32) < 0x20 => out.push(char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}')),
        '\u{7f}' => out.push('␡'),
        c if c.is_control() => out.push('\u{fffd}'),
        c => out.push(c),
    }
}

/// `text` cut into rows of at most `width` cells; never drops a character.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut shown = String::new();
    for c in text.chars() {
        shown_char(c, &mut shown);
    }
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in shown.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        if let Some(row) = rows.last_mut() {
            row.push(c);
        }
        used += w;
    }
    rows
}

fn body_rows(lines: &[WorkerQuestionLine], width: usize) -> Vec<BodyRow> {
    let mut rows = Vec::new();
    for line in lines {
        let (text, kind) = match line {
            WorkerQuestionLine::Text(text) => (text.clone(), RowKind::Text),
            WorkerQuestionLine::Heading(text) => (text.clone(), RowKind::Heading),
            WorkerQuestionLine::Option {
                button,
                label,
                picked,
                multi,
            } => {
                let mark = match (multi, picked) {
                    (true, true) => "[x]",
                    (true, false) => "[ ]",
                    (false, true) => "(•)",
                    (false, false) => "( )",
                };
                (format!("{mark} {label}"), RowKind::Option(*button, *picked))
            }
        };
        for (index, row) in wrap(&text, width).into_iter().enumerate() {
            rows.push(BodyRow {
                text: row,
                continued: index > 0,
                kind,
            });
        }
    }
    rows
}

/// The buttons laid out left to right, wrapping onto more rows; each with
/// its rectangle relative to the first row.
fn button_layout(
    buttons: &[(WorkerQuestionButton, &'static str, bool)],
    width: u16,
) -> Vec<(u16, u16, u16)> {
    let mut placed = Vec::new();
    let (mut x, mut row) = (1u16, 0u16);
    for (_, label, _) in buttons {
        let w = display_width(label).saturating_add(2);
        if x > 1 && x.saturating_add(w) > width {
            x = 1;
            row += 1;
        }
        placed.push((x, row, w));
        x = x.saturating_add(w).saturating_add(1);
    }
    placed
}

pub(crate) fn render_worker_question(
    b: &mut Buffer,
    view: &WorkerQuestionView,
    p: &Palette,
) -> Option<WorkerQuestionRender> {
    for y in b.area.y..b.area.bottom() {
        for x in b.area.x..b.area.right() {
            let c = &mut b[(x, y)];
            c.set_style(c.style().add_modifier(Modifier::DIM));
        }
    }
    let width = MODAL_WIDTH.min(b.area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);
    // A left margin, a gutter (`↪` on a continued row) and a right margin.
    let text_width = usize::from(inner_width.saturating_sub(4));
    let rows = body_rows(&view.lines, text_width);
    let placed = button_layout(&view.buttons, inner_width);
    let button_rows = placed.last().map_or(1, |(_, row, _)| row + 1);
    let top = 4 + u16::from(view.escalation.is_some());
    let bottom = 1 + u16::from(view.deny_message.is_some()) + button_rows + 1;
    let wanted = u16::try_from(rows.len().max(1)).unwrap_or(u16::MAX);
    let outer = popup(
        b.area,
        width,
        top.saturating_add(wanted)
            .saturating_add(bottom)
            .saturating_add(2),
    )?;
    let border = if view.confirm_stop { p.red } else { p.accent };
    let inner = panel(b, outer, border, p.panel_bg)?;
    let body_height = inner.height.checked_sub(top + bottom).filter(|h| *h > 0)?;
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM | Modifier::BOLD);
    let dim = base.fg(p.overlay0);

    // Header.
    let position = view
        .position
        .map(|(index, total)| format!(" {index} of {total} "))
        .unwrap_or_default();
    let position_width = display_width(&position);
    put_text(
        b,
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(position_width + 2),
        &view.title,
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        inner.right().saturating_sub(position_width),
        inner.y,
        position_width,
        &position,
        base.fg(p.accent).add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(2),
        &view.context,
        dim,
    );
    put_text(
        b,
        inner.x + 1,
        inner.y + 2,
        inner.width.saturating_sub(2),
        &view.owner,
        base.fg(p.subtext0),
    );
    if let Some(escalation) = view.escalation.as_deref() {
        put_text(
            b,
            inner.x + 1,
            inner.y + 3,
            inner.width.saturating_sub(2),
            escalation,
            base.fg(p.yellow),
        );
    }

    // The body: the whole input, scrolled, on the panel's surface.
    let body = Rect::new(inner.x, inner.y + top, inner.width, body_height);
    let mut buttons = Vec::new();
    b.set_style(body, base.bg(p.surface_dim));
    let max_scroll = rows.len().saturating_sub(usize::from(body_height));
    let scroll = view.scroll.min(max_scroll);
    for (offset, row) in rows
        .iter()
        .skip(scroll)
        .take(usize::from(body_height))
        .enumerate()
    {
        let y = body.y + offset as u16;
        let surface = base.bg(p.surface_dim);
        if row.continued {
            put_text(b, body.x + 1, y, 1, "↪", surface.fg(p.overlay0));
        }
        let style = match row.kind {
            RowKind::Text => surface.fg(p.text),
            RowKind::Heading => surface.fg(p.text).add_modifier(Modifier::BOLD),
            RowKind::Option(button, picked) => {
                let rect = Rect::new(body.x + 3, y, body.width.saturating_sub(4), 1);
                buttons.push((rect, button));
                let focused = view.focused == Some(button);
                match (focused, picked) {
                    (true, _) => Style::default()
                        .fg(contrast(p))
                        .bg(p.accent)
                        .add_modifier(Modifier::BOLD),
                    (false, true) => surface.fg(p.accent).add_modifier(Modifier::BOLD),
                    (false, false) => surface.fg(p.text),
                }
            }
        };
        put_text(
            b,
            body.x + 3,
            y,
            body.width.saturating_sub(4),
            &row.text,
            style,
        );
    }

    // The status line, Deny's message field and the buttons.
    let mut y = body.bottom();
    if let Some((notice, warns)) = view.notice.as_ref() {
        let color = if *warns { p.yellow } else { p.overlay1 };
        put_text(
            b,
            inner.x + 1,
            y,
            inner.width.saturating_sub(2),
            notice,
            base.fg(color),
        );
    }
    y += 1;
    let mut cursor = None;
    if let Some(editor) = view.deny_message.as_ref() {
        let label = "message: ";
        let label_width = display_width(label);
        put_text(b, inner.x + 1, y, label_width, label, base.fg(p.subtext0));
        let field = Rect::new(
            inner.x + 1 + label_width,
            y,
            inner.width.saturating_sub(label_width + 2),
            1,
        );
        cursor = crate::client::shell::text_editor::render(
            b,
            field,
            editor,
            Style::default().fg(p.text).bg(p.surface0),
        );
        y += 1;
    }
    for ((target, label, enabled), (x, row, w)) in view.buttons.iter().zip(&placed) {
        let rect = Rect::new(inner.x + x, y + row, (*w).min(inner.width), 1);
        let danger = matches!(
            target,
            WorkerQuestionButton::Stop | WorkerQuestionButton::ConfirmStop
        );
        let style = if !enabled {
            Style::default().fg(p.overlay0).bg(p.surface_dim)
        } else if view.focused == Some(*target) {
            Style::default()
                .fg(contrast(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if danger {
            Style::default()
                .fg(contrast(p))
                .bg(p.red)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD)
        };
        button(b, rect, &format!(" {label} "), style);
        if *enabled {
            buttons.push((rect, *target));
        }
    }
    let scrolled = if max_scroll > 0 {
        format!(
            "{}–{} of {} rows · ",
            scroll + 1,
            (scroll + usize::from(body_height)).min(rows.len()),
            rows.len()
        )
    } else {
        String::new()
    };
    put_text(
        b,
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.width.saturating_sub(2),
        &if view.deny_message.is_some() {
            format!("{scrolled}↵ deny · esc cancel")
        } else {
            format!("{scrolled}tab choose · ↵ press chosen · j/k scroll · l log · esc close")
        },
        dim,
    );
    Some(WorkerQuestionRender {
        popup: outer,
        buttons,
        max_scroll,
        body_rows: usize::from(body_height),
        cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_keeps_every_character_and_shows_control_ones() {
        assert_eq!(wrap("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(wrap("a\tb\r", 20), vec!["a    b␍"]);
        // A wide character never splits.
        assert_eq!(wrap("ab界", 3), vec!["ab", "界"]);
    }
}
