//! The Supervision tab pinned at the top of the sidebar: what the server's
//! supervision sweep (`supervise.report`) saw of the coordinators, and the
//! findings that hold. Its row sits above the spaces, outside their scroll,
//! and is never dragged, closed, filtered, sorted or folded; a click shows
//! its view in the main pane, read-only, like a worker's tab
//! ([`super::worker_view`]). The data comes in the snapshot
//! ([`crate::protocol::ClientShellSupervision`]), which the server sends
//! again after each sweep and each change of a finding: the client never
//! asks for it. A server that sends none has no tab. `[supervision]
//! show_tab = false` hides it.
//!
//! No finding is not "healthy": the sweep never establishes progress, so
//! the row draws no all-clear mark and the view says so. A sweep older than
//! twice the server's interval shows as stale, and a failed one as
//! unavailable.

use crossterm::event::{MouseButton, MouseEventKind};

use super::*;
use crate::api::schema::{Method, PaneTarget};
use crate::protocol::{ClientShellSupervision, ClientShellSupervisionFinding};

/// Rows a wheel notch scrolls.
const WHEEL_ROWS: usize = 3;

/// The tab's name, on its row and its view's header.
pub(super) const LABEL: &str = "Supervision";

/// The glyphs of the two severities, and of no finding (not a check mark:
/// no finding does not mean progress).
const CRITICAL_GLYPH: &str = "◆";
const WARNING_GLYPH: &str = "▲";
const NONE_GLYPH: &str = "○";

/// The `worker_id` prefix of a finding's `?` entry
/// (`crate::workers::supervision`), which links it to the same finding here.
pub(super) const FINDING_WORKER_PREFIX: &str = crate::workers::supervision::WORKER_PREFIX;

/// Counts wider than this show as this, so the counts keep their width.
const MAX_COUNT: usize = 99;

/// Whether what the tab shows is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Freshness {
    /// The server has not swept yet.
    NoSweep,
    /// The latest sweep is within twice the server's interval.
    Fresh,
    /// The latest sweep is older than twice the server's interval.
    Stale,
    /// The latest sweep failed: what it saw before is all there is.
    Unavailable,
}

/// What the tab's row says: how current it is and the findings that hold,
/// by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SupervisionSummary {
    pub(super) freshness: Freshness,
    pub(super) critical: usize,
    pub(super) warning: usize,
}

impl SupervisionSummary {
    pub(super) fn of(supervision: &ClientShellSupervision, now_ms: u64) -> Self {
        let critical = supervision
            .findings
            .iter()
            .filter(|finding| is_critical(finding))
            .count();
        let freshness = if supervision.sweep_error.is_some() {
            Freshness::Unavailable
        } else if supervision.swept_ms.is_none() {
            Freshness::NoSweep
        } else if stale_at(supervision).is_some_and(|stale_at| now_ms >= stale_at) {
            Freshness::Stale
        } else {
            Freshness::Fresh
        };
        Self {
            freshness,
            critical,
            warning: supervision.findings.len() - critical,
        }
    }

    /// The worst severity that holds, as its glyph and whether it is
    /// critical; none when no finding holds.
    fn worst(&self) -> Option<(&'static str, bool)> {
        if self.critical > 0 {
            Some((CRITICAL_GLYPH, true))
        } else if self.warning > 0 {
            Some((WARNING_GLYPH, false))
        } else {
            None
        }
    }

    /// What the row says after the name: nothing while current.
    fn state_text(&self) -> Option<&'static str> {
        match self.freshness {
            Freshness::Fresh => None,
            Freshness::Stale => Some("stale"),
            Freshness::NoSweep => Some("no sweep yet"),
            Freshness::Unavailable => Some("unavailable"),
        }
    }
}

/// An unknown severity counts as a warning.
fn is_critical(finding: &ClientShellSupervisionFinding) -> bool {
    finding.severity == "critical"
}

/// When the latest sweep turns stale: twice the server's interval after
/// it. None before the first sweep.
pub(super) fn stale_at(supervision: &ClientShellSupervision) -> Option<u64> {
    let swept = supervision.swept_ms?;
    (supervision.sweep_interval_ms > 0)
        .then(|| swept.saturating_add(supervision.sweep_interval_ms.saturating_mul(2)))
}

/// The tab's row as the sidebar draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupervisionRow {
    pub(super) summary: SupervisionSummary,
    /// Its view shows in the main pane.
    pub(super) shown: bool,
}

/// The counts in a fixed width (`◆ 1 ▲12`), so a sweep that changes them
/// never moves the rest of the row.
pub(super) fn counts_text(summary: &SupervisionSummary) -> String {
    format!(
        "{CRITICAL_GLYPH}{:>2} {WARNING_GLYPH}{:>2}",
        summary.critical.min(MAX_COUNT),
        summary.warning.min(MAX_COUNT)
    )
}

/// Takes the tab's row off the top of `area` when there is one, draws it
/// and records its hit; returns what is left for the spaces.
pub(super) fn render_row(
    buffer: &mut Buffer,
    area: Rect,
    row: Option<SupervisionRow>,
    palette: &Palette,
    hits: &mut ShellHitMap,
) -> Rect {
    let Some(row) = row.filter(|_| area.height > 1 && area.width > 4) else {
        return area;
    };
    let rect = Rect::new(area.x, area.y, area.width, 1);
    // The divider column at the right stays the sidebar's.
    let width = area.width.saturating_sub(1);
    let base = if row.shown {
        Style::default().bg(palette.selection_bg)
    } else {
        Style::default()
    };
    buffer.set_style(Rect::new(area.x, area.y, width, 1), base);
    let summary = row.summary;
    let (glyph, glyph_style) = match summary.worst() {
        Some((glyph, true)) => (glyph, base.fg(palette.red).add_modifier(Modifier::BOLD)),
        Some((glyph, false)) => (glyph, base.fg(palette.peach).add_modifier(Modifier::BOLD)),
        None => (NONE_GLYPH, base.fg(palette.overlay0)),
    };
    render::put_text(buffer, area.x + 1, area.y, 1, glyph, glyph_style);
    // The counts keep their place at the right, blank before a sweep.
    let counts = counts_text(&summary);
    let counts_width = render::display_width(&counts);
    let counts_x = (area.x + width).saturating_sub(counts_width + 1);
    let name_x = area.x + 3;
    let name_width = counts_x.saturating_sub(name_x + 1);
    let name_style = if row.shown {
        base.fg(palette.text).add_modifier(Modifier::BOLD)
    } else {
        base.fg(palette.subtext0).add_modifier(Modifier::BOLD)
    };
    let end = render::put_segment(
        buffer,
        name_x,
        area.y,
        name_x + name_width,
        LABEL,
        name_style,
    );
    if let Some(state) = summary.state_text() {
        let style = match summary.freshness {
            Freshness::Stale | Freshness::Unavailable => base.fg(palette.peach),
            _ => base.fg(palette.overlay0),
        };
        render::put_segment(buffer, end + 1, area.y, name_x + name_width, state, style);
    }
    if summary.freshness != Freshness::NoSweep && counts_x > name_x {
        let mut x = counts_x;
        for (index, part) in counts.split(' ').enumerate() {
            let count = if index == 0 {
                summary.critical
            } else {
                summary.warning
            };
            let style = match (count, index) {
                (0, _) => base.fg(palette.surface1),
                (_, 0) => base.fg(palette.red),
                _ => base.fg(palette.peach),
            };
            render::put_text(buffer, x, area.y, render::display_width(part), part, style);
            x += render::display_width(part) + 1;
        }
    }
    hits.supervision_row = rect;
    hits.tooltips.push(super::tooltip::TooltipTarget {
        rect,
        id: "supervision".into(),
        text: "what herdr's supervision sweep saw: critical and warning findings".into(),
        bg: None,
        starts_at_target: false,
        multiline: false,
    });
    Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    )
}

/// What a row of the view is, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewRowKind {
    Heading,
    Text,
    Dim,
    Critical,
    Warning,
}

/// One screen row of the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewRow {
    pub(super) text: String,
    pub(super) kind: ViewRowKind,
    /// A coordinator's row: its pane, when the snapshot has it, with the
    /// column its link starts at.
    pub(super) pane: Option<(String, u16)>,
    /// A finding's rows: its id, for the highlight.
    pub(super) finding: Option<String>,
    /// The row's text is cut to the width instead of wrapped.
    pub(super) cut: bool,
}

impl ViewRow {
    fn new(text: impl Into<String>, kind: ViewRowKind) -> Self {
        Self {
            text: text.into(),
            kind,
            pane: None,
            finding: None,
            cut: false,
        }
    }
}

/// The last part of a path, which names a repository.
fn repo_name(repo: &str) -> String {
    std::path::Path::new(repo)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.to_owned())
}

/// The link text of a coordinator's pane.
const OPEN_PANE: &str = "open pane";

/// The table's columns: their headings and widest cells.
const COLUMNS: [(&str, usize); 6] = [
    ("REPO", 18),
    ("STATE", 9),
    ("PROGRESS", 26),
    ("RUN", 30),
    ("WORKERS", 16),
    ("PANE", 20),
];

/// The view's rows, before wrapping: the coordinators' table and the
/// findings with their evidence.
pub(super) fn view_rows(
    supervision: &ClientShellSupervision,
    snapshot: &ClientShellSnapshot,
    now_ms: u64,
) -> Vec<ViewRow> {
    let mut rows = Vec::new();
    let coordinators = &supervision.coordinators;
    rows.push(ViewRow::new(
        format!("Coordinators ({})", coordinators.len()),
        ViewRowKind::Heading,
    ));
    if coordinators.is_empty() {
        let text = if supervision.swept_ms.is_some() {
            "  no coordination tenure is active"
        } else {
            "  no sweep yet"
        };
        rows.push(ViewRow::new(text, ViewRowKind::Dim));
    } else {
        let cells: Vec<([String; 6], Option<String>)> = coordinators
            .iter()
            .map(|coordinator| {
                let state = match (&coordinator.agent_status, &coordinator.pane_id) {
                    (Some(status), _) => status.clone(),
                    (None, None) => "headless".to_owned(),
                    (None, Some(_)) => "no pane".to_owned(),
                };
                let progress = if coordinator.findings.is_empty() {
                    coordinator.note.clone().unwrap_or_else(|| {
                        crate::workers::supervision::CANNOT_ESTABLISH_PROGRESS.to_owned()
                    })
                } else {
                    format!("findings {}", coordinator.findings.join(", "))
                };
                let run = coordinator.active_run.as_ref().map_or_else(
                    || "–".to_owned(),
                    |run_id| match snapshot.todo_runs.iter().find(|run| &run.run_id == run_id) {
                        Some(run) => format!("{run_id} · {} · {}", run.step, run.status),
                        None => run_id.clone(),
                    },
                );
                let workers = if coordinator.workers.is_empty() {
                    "–".to_owned()
                } else {
                    coordinator.workers.join(", ")
                };
                let live = coordinator
                    .pane_id
                    .as_ref()
                    .filter(|pane_id| snapshot.panes.iter().any(|pane| &pane.pane_id == *pane_id));
                let pane = match (&coordinator.pane_id, live) {
                    (_, Some(pane_id)) => format!("{OPEN_PANE} {pane_id}"),
                    (Some(pane_id), None) => format!("{pane_id} (closed)"),
                    (None, None) => "–".to_owned(),
                };
                (
                    [
                        repo_name(&coordinator.repo),
                        state,
                        progress,
                        run,
                        workers,
                        pane,
                    ],
                    live.cloned(),
                )
            })
            .collect();
        let widths: Vec<usize> = COLUMNS
            .iter()
            .enumerate()
            .map(|(index, (heading, cap))| {
                cells
                    .iter()
                    .map(|(row, _)| unicode_width::UnicodeWidthStr::width(row[index].as_str()))
                    .chain([heading.len()])
                    .max()
                    .unwrap_or(0)
                    .min(*cap)
            })
            .collect();
        let line = |values: [&str; 6]| -> (String, u16) {
            let mut text = String::from("  ");
            let mut last_start = 0;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    text.push_str("  ");
                }
                last_start = unicode_width::UnicodeWidthStr::width(text.as_str());
                let cell = super::space_tabs::truncate(value, widths[index]);
                let pad = widths[index]
                    .saturating_sub(unicode_width::UnicodeWidthStr::width(cell.as_str()));
                text.push_str(&cell);
                if index + 1 < values.len() {
                    text.push_str(&" ".repeat(pad));
                }
            }
            (text, last_start.min(u16::MAX as usize) as u16)
        };
        let (heading, _) = line(COLUMNS.map(|(heading, _)| heading));
        let mut heading = ViewRow::new(heading, ViewRowKind::Dim);
        heading.cut = true;
        rows.push(heading);
        for (values, live) in &cells {
            let (text, pane_x) = line([
                &values[0], &values[1], &values[2], &values[3], &values[4], &values[5],
            ]);
            let mut row = ViewRow::new(text, ViewRowKind::Text);
            row.cut = true;
            row.pane = live.clone().map(|pane_id| (pane_id, pane_x));
            rows.push(row);
        }
    }
    rows.push(ViewRow::new("", ViewRowKind::Text));
    let findings = &supervision.findings;
    if findings.is_empty() {
        rows.push(ViewRow::new("Findings (0)", ViewRowKind::Heading));
        rows.push(ViewRow::new(
            "  none holds; that does not mean progress: the sweep cannot establish it",
            ViewRowKind::Dim,
        ));
        return rows;
    }
    rows.push(ViewRow::new(
        format!("Findings ({})", findings.len()),
        ViewRowKind::Heading,
    ));
    for finding in findings {
        let critical = is_critical(finding);
        let glyph = if critical {
            CRITICAL_GLYPH
        } else {
            WARNING_GLYPH
        };
        let mut title = format!(
            "{glyph} {} · {} · {} · {}",
            finding.finding_id,
            finding.severity,
            finding.status,
            finding.predicate.replace('_', " ")
        );
        if let Some(repo) = &finding.repo {
            title.push_str(&format!(" · {}", repo_name(repo)));
        }
        if finding.status == "open" {
            title.push_str(" · in the ? list");
        }
        let kind = if critical {
            ViewRowKind::Critical
        } else {
            ViewRowKind::Warning
        };
        let mut lines = vec![
            ViewRow::new(title, kind),
            ViewRow::new(format!("    {}", finding.summary), ViewRowKind::Text),
            ViewRow::new(
                format!("    expected: {}", finding.expected_event),
                ViewRowKind::Text,
            ),
        ];
        let mut seen = vec![
            format!(
                "first seen {} ago",
                super::workers::age_text(now_ms.saturating_sub(finding.first_seen_ms))
            ),
            format!(
                "last seen {} ago",
                super::workers::age_text(now_ms.saturating_sub(finding.last_seen_ms))
            ),
        ];
        if let Some(by) = &finding.acknowledged_by {
            seen.push(format!("acknowledged by {by}"));
        }
        let subjects = [
            finding
                .coordinator_id
                .as_ref()
                .map(|id| format!("tenure {id}")),
            finding.pane_id.as_ref().map(|id| format!("pane {id}")),
            finding.run_id.as_ref().map(|id| format!("run {id}")),
        ];
        seen.extend(subjects.into_iter().flatten());
        lines.push(ViewRow::new(
            format!("    {}", seen.join(" · ")),
            ViewRowKind::Dim,
        ));
        lines.extend(
            finding
                .event_ids
                .iter()
                .map(|id| ViewRow::new(format!("    event: {id}"), ViewRowKind::Dim)),
        );
        lines.extend(
            finding
                .facts
                .iter()
                .map(|fact| ViewRow::new(format!("    {fact}"), ViewRowKind::Dim)),
        );
        for mut line in lines {
            line.finding = Some(finding.finding_id.clone());
            rows.push(line);
        }
        rows.push(ViewRow::new("", ViewRowKind::Text));
    }
    rows
}

/// `rows` fitted to `width`: cut ones are cut, the others wrapped.
fn fit_rows(rows: Vec<ViewRow>, width: u16) -> Vec<ViewRow> {
    let mut fitted = Vec::new();
    for row in rows {
        if row.cut {
            let text = super::space_tabs::truncate(&row.text, usize::from(width));
            fitted.push(ViewRow { text, ..row });
            continue;
        }
        let parts = wrap(&row.text, width);
        for text in parts {
            fitted.push(ViewRow {
                text,
                pane: None,
                ..row.clone()
            });
        }
    }
    fitted
}

/// `line` cut into rows of at most `width` columns.
fn wrap(line: &str, width: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let width = usize::from(width.max(1));
    let mut rows = vec![String::new()];
    let mut used = 0;
    for ch in line.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        if let Some(row) = rows.last_mut() {
            row.push(ch);
        }
        used += ch_width;
    }
    rows
}

/// The Supervision tab's view, shown in the main pane.
pub(crate) struct ClientSupervisionView {
    pub(super) endpoint_id: ClientEndpointId,
    /// The tab focused when the view opened: once another one is, the view
    /// closes, like a worker's tab.
    pub(super) focused_tab_id: Option<String>,
    /// The finding the view opened on (from its `?` entry): highlighted, and
    /// scrolled to once.
    pub(super) highlight: Option<String>,
    pub(super) reveal_highlight: bool,
    /// The first row shown.
    pub(super) scroll: usize,
}

/// What the view drew, for the pointer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SupervisionViewHits {
    pub(super) area: Rect,
    pub(super) body: Rect,
    /// The coordinators' pane links, with their panes.
    pub(super) panes: Vec<(Rect, String)>,
    /// How far the body scrolls.
    pub(super) max_scroll: usize,
}

/// The header's and the details row's text: the findings by severity, and
/// when the sweep ran.
pub(super) fn header_texts(
    supervision: Option<&ClientShellSupervision>,
    now_ms: u64,
) -> (String, String) {
    let Some(supervision) = supervision else {
        return (
            format!(" {NONE_GLYPH} {LABEL} · unavailable"),
            " this server does not send what its supervision sweep sees".to_owned(),
        );
    };
    let summary = SupervisionSummary::of(supervision, now_ms);
    let findings = match (summary.critical, summary.warning) {
        (0, 0) => "no findings (not a sign of progress)".to_owned(),
        (critical, warning) => format!("{critical} critical · {warning} warning"),
    };
    let glyph = summary.worst().map_or(NONE_GLYPH, |(glyph, _)| glyph);
    let header = format!(" {glyph} {LABEL} · {findings}");
    let interval = super::workers::age_text(supervision.sweep_interval_ms);
    let details = match (supervision.swept_ms, &supervision.sweep_error) {
        (_, Some(error)) => format!(" unavailable: the latest sweep failed: {error}"),
        (None, None) => " no sweep yet".to_owned(),
        (Some(swept), None) => {
            let age = super::workers::age_text(now_ms.saturating_sub(swept));
            let trigger = supervision
                .trigger
                .as_deref()
                .map(|trigger| format!(" ({trigger})"))
                .unwrap_or_default();
            if summary.freshness == Freshness::Stale {
                format!(" stale: last sweep {age} ago{trigger}, expected every {interval}")
            } else {
                format!(
                    " last sweep {age} ago{trigger} · every {interval} and on coordinator events"
                )
            }
        }
    };
    (header, details)
}

/// Draws the view over `area`, the main pane's place: a header with the
/// findings by severity and a read-only label, a row with the sweep's time
/// and freshness, then the coordinators' table and the findings.
pub(super) fn render_view(
    view: &mut ClientSupervisionView,
    snapshot: &ClientShellSnapshot,
    now_ms: u64,
    palette: &Palette,
    buffer: &mut Buffer,
    area: Rect,
) -> SupervisionViewHits {
    if area.is_empty() {
        return SupervisionViewHits::default();
    }
    let base = Style::default().bg(palette.panel_bg).fg(palette.text);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buffer[(x, y)].reset();
            buffer[(x, y)].set_symbol(" ").set_style(base);
        }
    }
    let supervision = snapshot.supervision.as_ref();
    let (header, details) = header_texts(supervision, now_ms);
    let header_style = Style::default()
        .bg(palette.surface0)
        .fg(palette.text)
        .add_modifier(Modifier::BOLD);
    buffer.set_style(Rect::new(area.x, area.y, area.width, 1), header_style);
    let label = " read-only ";
    let label_width = render::display_width(label);
    let header_room = area.width.saturating_sub(label_width);
    buffer.set_stringn(
        area.x,
        area.y,
        super::space_tabs::truncate(&header, usize::from(header_room)),
        usize::from(header_room),
        header_style,
    );
    if area.width > label_width {
        buffer.set_string(
            area.right() - label_width,
            area.y,
            label,
            Style::default().bg(palette.overlay0).fg(palette.panel_bg),
        );
    }
    if area.height > 1 {
        let stale = supervision.is_some_and(|supervision| {
            SupervisionSummary::of(supervision, now_ms).freshness != Freshness::Fresh
        });
        let details_style = Style::default().bg(palette.surface0).fg(if stale {
            palette.peach
        } else {
            palette.overlay1
        });
        buffer.set_style(Rect::new(area.x, area.y + 1, area.width, 1), details_style);
        buffer.set_stringn(
            area.x,
            area.y + 1,
            super::space_tabs::truncate(&details, usize::from(area.width)),
            usize::from(area.width),
            details_style,
        );
    }
    let body = Rect::new(
        area.x,
        area.y.saturating_add(2),
        area.width,
        area.height.saturating_sub(2),
    );
    let mut hits = SupervisionViewHits {
        area,
        body,
        ..SupervisionViewHits::default()
    };
    let Some(supervision) = supervision.filter(|_| !body.is_empty()) else {
        return hits;
    };
    // One column of margin at the left.
    let width = body.width.saturating_sub(1).max(1);
    let rows = fit_rows(view_rows(supervision, snapshot, now_ms), width);
    let max_scroll = rows.len().saturating_sub(usize::from(body.height));
    if std::mem::take(&mut view.reveal_highlight) {
        if let Some(first) = view
            .highlight
            .as_ref()
            .and_then(|id| rows.iter().position(|row| row.finding.as_ref() == Some(id)))
        {
            view.scroll = first;
        }
    }
    view.scroll = view.scroll.min(max_scroll);
    hits.max_scroll = max_scroll;
    for (index, row) in rows
        .iter()
        .skip(view.scroll)
        .take(usize::from(body.height))
        .enumerate()
    {
        let y = body.y + index as u16;
        let highlighted = row.finding.is_some() && row.finding == view.highlight;
        let row_base = if highlighted {
            base.bg(palette.surface1)
        } else {
            base
        };
        if highlighted {
            buffer.set_style(Rect::new(body.x, y, body.width, 1), row_base);
        }
        let style = match row.kind {
            ViewRowKind::Heading => row_base.fg(palette.accent).add_modifier(Modifier::BOLD),
            ViewRowKind::Text => row_base.fg(palette.text),
            ViewRowKind::Dim => row_base.fg(palette.overlay1),
            ViewRowKind::Critical => row_base.fg(palette.red).add_modifier(Modifier::BOLD),
            ViewRowKind::Warning => row_base.fg(palette.peach).add_modifier(Modifier::BOLD),
        };
        buffer.set_stringn(body.x + 1, y, &row.text, usize::from(width), style);
        if let Some((pane_id, column)) = &row.pane {
            let x = body.x + 1 + column;
            let link_width = render::display_width(OPEN_PANE);
            if x + link_width <= body.right() {
                let link = Rect::new(x, y, link_width, 1);
                buffer.set_style(
                    link,
                    row_base
                        .fg(palette.accent)
                        .add_modifier(Modifier::UNDERLINED),
                );
                hits.panes.push((link, pane_id.clone()));
            }
        }
    }
    hits
}

impl ClientShellState {
    /// The Supervision tab shows on the active endpoint.
    pub(super) fn supervision_view_shown(&self) -> bool {
        self.supervision_view
            .as_ref()
            .is_some_and(|view| view.endpoint_id == self.active_endpoint_id)
    }

    /// A read-only view (a worker's tab or the Supervision tab) takes the
    /// main pane's place: the focused tab's panes are neither drawn nor
    /// sent input.
    pub(super) fn main_view_shown(&self) -> bool {
        self.worker_view_shown() || self.supervision_view_shown()
    }

    /// The tab's row, when the config shows it and the active server sends
    /// what its sweep sees.
    pub(super) fn supervision_row(&self, now_ms: u64) -> Option<SupervisionRow> {
        if !self.config.show_supervision_tab {
            return None;
        }
        let supervision = self.snapshot.as_deref()?.supervision.as_ref()?;
        Some(SupervisionRow {
            summary: SupervisionSummary::of(supervision, now_ms),
            shown: self.supervision_view_shown(),
        })
    }

    /// Opens the Supervision tab in the main pane, on `finding` when given.
    pub(super) fn open_supervision_view(
        &mut self,
        finding: Option<String>,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        self.close_worker_view(outcome);
        let focused_tab_id = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.clone());
        let scroll = self
            .supervision_view
            .as_ref()
            .filter(|view| view.endpoint_id == self.active_endpoint_id)
            .map_or(0, |view| view.scroll);
        self.selection = None;
        self.supervision_view = Some(ClientSupervisionView {
            endpoint_id: self.active_endpoint_id.clone(),
            focused_tab_id,
            reveal_highlight: finding.is_some(),
            highlight: finding,
            scroll,
        });
    }

    /// Leaves the Supervision tab.
    pub(super) fn close_supervision_view(&mut self, outcome: &mut ClientShellInput) {
        if self.supervision_view.take().is_some() {
            outcome.repaint = true;
        }
    }

    /// A snapshot whose focused tab is not the one the view opened over:
    /// the focus went elsewhere, so the view gives way to that tab.
    pub(super) fn close_supervision_view_on_focus_change(&mut self, focused_tab_id: Option<&str>) {
        if self.supervision_view.as_ref().is_some_and(|view| {
            view.endpoint_id == self.active_endpoint_id
                && view.focused_tab_id.as_deref() != focused_tab_id
        }) {
            self.supervision_view = None;
        }
    }

    /// "Open coordinator pane": focuses the coordinator's existing pane and
    /// its tab, opening its space when folded.
    pub(super) fn open_supervised_pane(&mut self, pane_id: &str, outcome: &mut ClientShellInput) {
        let Some(pane) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id))
            .cloned()
        else {
            return;
        };
        self.close_supervision_view(outcome);
        self.expand_for_jump(Some(&pane.workspace_id), outcome);
        self.push_endpoint_method(
            Method::PaneFocus(PaneTarget {
                pane_id: pane.pane_id,
            }),
            outcome,
        );
    }

    fn scroll_supervision_view(&mut self, delta: isize) -> bool {
        let max = self.hits.supervision_view_max_scroll;
        let Some(view) = self.supervision_view.as_mut() else {
            return false;
        };
        let before = view.scroll;
        view.scroll = if delta < 0 {
            before.saturating_sub(delta.unsigned_abs())
        } else {
            before.saturating_add(delta.unsigned_abs()).min(max)
        };
        view.scroll != before
    }

    /// The keys the view takes (arrows, pages, Home and End scroll); others
    /// are not sent anywhere, since the tab takes no input.
    pub(super) fn supervision_view_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        let page = (self.hits.supervision_view_body.height.max(2) - 1) as isize;
        let changed = match key.code {
            KeyCode::Up => self.scroll_supervision_view(-1),
            KeyCode::Down => self.scroll_supervision_view(1),
            KeyCode::PageUp => self.scroll_supervision_view(-page),
            KeyCode::PageDown => self.scroll_supervision_view(page),
            KeyCode::Home => self.scroll_supervision_view(isize::MIN / 2),
            KeyCode::End => self.scroll_supervision_view(isize::MAX / 2),
            _ => false,
        };
        outcome.repaint |= changed;
    }

    /// The keys a shown read-only view takes.
    pub(super) fn main_view_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        if self.supervision_view_shown() {
            self.supervision_view_key(key, outcome);
        } else {
            self.worker_view_key(key, outcome);
        }
    }

    /// The pointer over the shown view: the wheel scrolls and a pane link
    /// focuses its pane. Returns whether the view took the event.
    pub(super) fn supervision_view_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.supervision_view_shown() || !super::contains(self.hits.supervision_view, point) {
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                outcome.repaint |= self.scroll_supervision_view(-(WHEEL_ROWS as isize));
            }
            MouseEventKind::ScrollDown => {
                outcome.repaint |= self.scroll_supervision_view(WHEEL_ROWS as isize);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let pane = self
                    .hits
                    .supervision_pane_links
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, pane_id)| pane_id.clone());
                if let Some(pane_id) = pane {
                    self.open_supervised_pane(&pane_id, outcome);
                }
            }
            _ => {}
        }
        true
    }

    /// When the tab's row turns stale, and while its view shows, when its
    /// sweep's age next changes: the frame is drawn again then.
    pub(super) fn supervision_times_change_at(&self, now_ms: u64) -> Option<u64> {
        let supervision = self.snapshot.as_deref()?.supervision.as_ref()?;
        let stale = stale_at(supervision).filter(|stale_at| *stale_at > now_ms);
        let age = supervision
            .swept_ms
            .filter(|_| self.supervision_view_shown())
            .map(|swept| super::workers::age_text_changes_at(swept, now_ms));
        stale.into_iter().chain(age).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, severity: &str) -> ClientShellSupervisionFinding {
        ClientShellSupervisionFinding {
            finding_id: id.into(),
            predicate: "coordinator_idle".into(),
            severity: severity.into(),
            status: "open".into(),
            summary: "idle".into(),
            expected_event: "the next item".into(),
            first_seen_ms: 0,
            last_seen_ms: 0,
            ..ClientShellSupervisionFinding::default()
        }
    }

    fn swept(findings: Vec<ClientShellSupervisionFinding>) -> ClientShellSupervision {
        ClientShellSupervision {
            swept_ms: Some(1_000),
            // delay: not a wait, the interval a test server reports.
            sweep_interval_ms: 60_000,
            findings,
            ..ClientShellSupervision::default()
        }
    }

    #[test]
    fn the_counts_keep_their_width_whatever_they_are() {
        let mut summary = SupervisionSummary::of(&swept(Vec::new()), 1_000);
        let width = render::display_width(&counts_text(&summary));
        for (critical, warning) in [(1, 0), (12, 3), (150, 99)] {
            summary.critical = critical;
            summary.warning = warning;
            assert_eq!(render::display_width(&counts_text(&summary)), width);
        }
    }

    #[test]
    fn an_unknown_severity_counts_as_a_warning_and_the_worst_shows() {
        let supervision = swept(vec![finding("f-1", "warning"), finding("f-2", "loud")]);
        let summary = SupervisionSummary::of(&supervision, 1_000);
        assert_eq!((summary.critical, summary.warning), (0, 2));
        assert_eq!(summary.worst(), Some((WARNING_GLYPH, false)));
        let supervision = swept(vec![finding("f-1", "warning"), finding("f-3", "critical")]);
        let summary = SupervisionSummary::of(&supervision, 1_000);
        assert_eq!(summary.worst(), Some((CRITICAL_GLYPH, true)));
    }

    #[test]
    fn a_sweep_turns_stale_at_twice_the_interval_and_a_failure_is_unavailable() {
        let mut supervision = swept(Vec::new());
        assert_eq!(stale_at(&supervision), Some(121_000));
        assert_eq!(
            SupervisionSummary::of(&supervision, 120_999).freshness,
            Freshness::Fresh
        );
        assert_eq!(
            SupervisionSummary::of(&supervision, 121_000).freshness,
            Freshness::Stale
        );
        supervision.sweep_error = Some("disk I/O error".into());
        assert_eq!(
            SupervisionSummary::of(&supervision, 0).freshness,
            Freshness::Unavailable
        );
        supervision.sweep_error = None;
        supervision.swept_ms = None;
        assert_eq!(
            SupervisionSummary::of(&supervision, 0).freshness,
            Freshness::NoSweep
        );
        assert_eq!(stale_at(&supervision), None);
    }

    #[test]
    fn no_finding_is_said_not_to_be_progress() {
        let (header, _) = header_texts(Some(&swept(Vec::new())), 1_000);
        assert!(
            header.contains("no findings (not a sign of progress)"),
            "{header}"
        );
        assert!(!header.contains('✓'), "{header}");
    }
}
