//! Vertical tabs under their space (`ui.sidebar.spaces.tabs`): one line per
//! top-level tab in tab order, with the tab's agent state, the tab bar's
//! label and the running and failed counts of the job tabs nested under it.
//! Nested tabs get no line of their own: the disclosure triangle before the
//! counts unfolds them as squares on the lines under it, one per nested tab,
//! and folds them again.

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use super::state::WorkspaceEntry;
use super::*;
use crate::api::schema::TabStatus;
use crate::protocol::{ClientShellSnapshot, ClientShellTab, ClientShellWorkspace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpaceTabLine {
    pub(super) tab_id: String,
    /// The tab's agent state and mark, as on agent rows; none for tabs
    /// without an agent.
    pub(super) state: Option<(crate::api::schema::AgentStatus, AgentMark)>,
    pub(super) label: String,
    /// The space's active tab is this one or nested under it.
    pub(super) active: bool,
    /// Running, failed and succeeded counts of the tab and its nested tabs,
    /// e.g. `⧖ 1 !1 ✓2`.
    pub(super) jobs: Vec<(Option<TabStatus>, String)>,
    /// The nested tabs, in tab order, drawn as squares while unfolded.
    pub(super) squares: Vec<TabSquare>,
    pub(super) unfolded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TabSquare {
    pub(super) tab_id: String,
    pub(super) label: String,
    /// The tab closed while the pointer was over the sidebar: its square
    /// keeps its slot, blank and inert, so the others do not move.
    pub(super) gone: bool,
    pub(super) status: Option<TabStatus>,
    /// The client's focused tab: the open job.
    pub(super) focused: bool,
}

/// A square is ` ⧖ `: the glyph with a column of padding on each side.
const SQUARE_WIDTH: u16 = 3;
const SQUARE_GAP: u16 = 1;
/// Squares start where the tab line's fill starts, past its state icon.
const SQUARES_INDENT: u16 = 5;
/// Columns a worktree space's tab lines move right, so they sit under the
/// worktree's name (after its `   └─ ` tree prefix) and not level with the
/// parent space's tab lines.
const WORKTREE_TAB_INDENT: u16 = 5;

/// The columns an entry's tab lines are indented by; callers take them off
/// the width they lay squares out in.
pub(super) fn tab_indent(indented: bool) -> u16 {
    if indented {
        WORKTREE_TAB_INDENT
    } else {
        0
    }
}
impl SpaceTabLine {
    /// Rows the line takes: its own and, while unfolded, its squares'.
    /// Unfolded squares are followed by an empty row, so they do not run
    /// into the next tab line.
    pub(super) fn height(&self, width: u16) -> u16 {
        let squares = if self.unfolded {
            self.squares.len().div_ceil(squares_per_row(width)) + 1
        } else {
            0
        };
        (1 + squares).min(u16::MAX as usize) as u16
    }
}

impl SpaceTabLine {
    /// The row, among this line's square rows, of the open job's square.
    pub(super) fn focused_square_row(&self, width: u16) -> Option<usize> {
        let index = self
            .squares
            .iter()
            .position(|square| square.focused)
            .filter(|_| self.unfolded)?;
        Some(index / squares_per_row(width))
    }
}

/// Each unfolded tab line's square order as last drawn, `(tab id, label)`
/// by line tab id, kept while the pointer is over the sidebar.
pub(super) type HeldSquares = std::collections::HashMap<String, Vec<(String, String)>>;

/// Squares that fit on a row of a space block `width` columns wide, from the
/// indent to the right edge, as the tab fill. Callers pass
/// the same width to [`SpaceTabLine::height`] and
/// [`render_space_tab_lines`], so the rows laid out are the rows drawn.
fn squares_per_row(width: u16) -> usize {
    let room = width.saturating_sub(SQUARES_INDENT) + SQUARE_GAP;
    usize::from((room / (SQUARE_WIDTH + SQUARE_GAP)).max(1))
}

/// The tab lines shown under `workspace`, or none when the setting is off. A
/// collapsed worktree parent stands for its whole group and lists no tabs:
/// its row keeps the group's job counts.
pub(super) fn space_tab_lines(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
    unfolded_squares: &HashSet<String>,
    held_squares: &HeldSquares,
    config: &ClientShellConfig,
) -> Vec<SpaceTabLine> {
    space_tab_lines_filtered(
        snapshot,
        workspace,
        collapsed_groups,
        unfolded_squares,
        held_squares,
        None,
        config,
    )
}

/// [`space_tab_lines`] narrowed by the spaces filter: only the tabs the query
/// shows (see [`super::space_filter::FilterView::shows_tab`]).
pub(super) fn space_tab_lines_filtered(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
    unfolded_squares: &HashSet<String>,
    held_squares: &HeldSquares,
    filter: Option<&super::space_filter::FilterView>,
    config: &ClientShellConfig,
) -> Vec<SpaceTabLine> {
    if !config.spaces.tabs
        || stands_for_a_group(snapshot, workspace, collapsed_groups)
        || collapsed_groups.contains(&tabs_collapse_key(&workspace.workspace_id))
    {
        return Vec::new();
    }
    let active_group = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == workspace.active_tab_id)
        .map(|tab| tab.parent_tab_id.as_deref().unwrap_or(&tab.tab_id));
    top_level_tabs(snapshot, workspace)
        .filter(|tab| filter.is_none_or(|view| view.shows_tab(workspace, &tab.tab_id)))
        .map(|tab| {
            let group = snapshot
                .tabs
                .iter()
                .filter(|candidate| {
                    candidate.tab_id == tab.tab_id
                        || candidate.parent_tab_id.as_deref() == Some(tab.tab_id.as_str())
                })
                // Succeeded jobs count too: one kept open (`--keep`) would
                // otherwise leave the line with a bare triangle.
                .filter(|candidate| {
                    matches!(
                        candidate.status,
                        Some(TabStatus::Running | TabStatus::Failed | TabStatus::Succeeded)
                    )
                })
                .collect::<Vec<_>>();
            let live = super::tab_groups::child_tabs(snapshot, &tab.tab_id);
            let square = |child: &ClientShellTab| TabSquare {
                tab_id: child.tab_id.clone(),
                label: child.label.clone(),
                gone: false,
                status: child.status,
                focused: child.focused,
            };
            // The order the squares had while the pointer stays over the
            // sidebar: closed tabs keep a blank slot, new ones come last.
            let held = held_squares
                .get(&tab.tab_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let squares = held
                .iter()
                .map(|(tab_id, label)| {
                    live.iter()
                        .find(|child| child.tab_id == *tab_id)
                        .map(|child| square(child))
                        .unwrap_or_else(|| TabSquare {
                            tab_id: tab_id.clone(),
                            label: label.clone(),
                            gone: true,
                            status: None,
                            focused: false,
                        })
                })
                .chain(
                    live.iter()
                        .filter(|child| held.iter().all(|(tab_id, _)| *tab_id != child.tab_id))
                        .map(|child| square(child)),
                )
                .collect::<Vec<_>>();
            SpaceTabLine {
                tab_id: tab.tab_id.clone(),
                state: tab_state(snapshot, tab),
                label: super::render::tabs::sidebar_tab_label(tab, snapshot, config),
                active: active_group == Some(tab.tab_id.as_str()),
                jobs: super::tab_groups::children_summary_segments(&group),
                unfolded: !squares.is_empty() && unfolded_squares.contains(&tab.tab_id),
                squares,
            }
        })
        .collect()
}

/// The header text while a tab line is dragged: which tab moves and where it
/// lands among its space's top-level tabs, as `2 → 4 · build · before review`.
pub(super) fn tab_drag_hint(
    snapshot: &ClientShellSnapshot,
    tab_id: &str,
    insert_index: Option<usize>,
) -> String {
    let Some(insert_index) = insert_index else {
        return "release cancels · Esc".into();
    };
    let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id) else {
        return "release cancels · Esc".into();
    };
    let tops = snapshot
        .tabs
        .iter()
        .filter(|other| other.workspace_id == tab.workspace_id && other.parent_tab_id.is_none())
        .collect::<Vec<_>>();
    let Some(source) = tops.iter().position(|other| other.tab_id == tab_id) else {
        return "release cancels · Esc".into();
    };
    if insert_index == source || insert_index == source + 1 {
        return format!("no change · {} · Esc", tab.label);
    }
    let landing = if insert_index > source {
        insert_index - 1
    } else {
        insert_index
    };
    let others = tops
        .iter()
        .filter(|other| other.tab_id != tab_id)
        .collect::<Vec<_>>();
    let place = match others.get(landing) {
        Some(next) => format!("before {}", next.label),
        None => "at the end".into(),
    };
    // Positions first: a narrow sidebar cuts the text from the right.
    format!("{} → {} · {} · {place}", source + 1, landing + 1, tab.label)
}

/// The key that hides a space's tab lines, kept with the collapsed worktree
/// groups (keyed by repository path, so they cannot clash) and saved with them.
pub(super) fn tabs_collapse_key(workspace_id: &str) -> String {
    format!("tabs:{workspace_id}")
}

/// A disclosure triangle in front of the space's name, as in tree-style tab
/// lists: it hides or shows the space's tab lines, and for a worktree parent
/// its child spaces too (a collapsed group lists no tabs). Returns its hit
/// rect, with the space after it, and the collapse key for the click. None
/// for a space with nothing to hide; its column stays reserved so names line
/// up. `rect` is the space's block, whose name line `render_workspace_rows`
/// starts past this column.
pub(super) fn render_space_disclosure(
    buffer: &mut Buffer,
    rect: Rect,
    snapshot: &ClientShellSnapshot,
    entry: &WorkspaceEntry,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
    config: &ClientShellConfig,
) -> Option<(Rect, String)> {
    let key = super::sidebar::parent_group_key(snapshot, entry.index).or_else(|| {
        top_level_tabs(snapshot, workspace)
            .next()
            .is_some()
            .then(|| tabs_collapse_key(&workspace.workspace_id))
    })?;
    // After the tree prefix (`   ├─ `) of a worktree child.
    let x = rect.x.saturating_add(if entry.indented { 6 } else { 1 });
    if x.saturating_add(2) > rect.right() {
        return None;
    }
    super::render::put_text(
        buffer,
        x,
        rect.y,
        1,
        if collapsed_groups.contains(&key) {
            "►"
        } else {
            "▼"
        },
        Style::default().fg(config.palette.overlay1),
    );
    Some((Rect::new(x, rect.y, 2, 1), key))
}

/// The running and failed tab counts for the space row's `tab_jobs`: all of
/// them while no tab lines are listed, else only the tabs no line counts
/// (nested under a tab that is gone).
pub(super) fn space_row_tab_jobs(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
    lines: &[SpaceTabLine],
) -> (usize, usize) {
    if lines.is_empty() {
        return super::sidebar::displayed_workspace_tab_jobs(snapshot, workspace, collapsed_groups);
    }
    let listed = lines
        .iter()
        .map(|line| line.tab_id.as_str())
        .collect::<HashSet<_>>();
    let mut counts = (0, 0);
    for tab in snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace.workspace_id)
        .filter(|tab| {
            !listed.contains(tab.tab_id.as_str())
                && !tab
                    .parent_tab_id
                    .as_deref()
                    .is_some_and(|parent| listed.contains(parent))
        })
    {
        match tab.status {
            Some(TabStatus::Running) => counts.0 += 1,
            Some(TabStatus::Failed) => counts.1 += 1,
            _ => {}
        }
    }
    counts
}

/// The tab's aggregate state, marked like an agent row: `?` when that state
/// is done because one of its agents awaits a reply, else the waiting mark
/// while a job nested under it runs.
fn tab_state(
    snapshot: &ClientShellSnapshot,
    tab: &ClientShellTab,
) -> Option<(crate::api::schema::AgentStatus, AgentMark)> {
    use crate::api::schema::AgentStatus;
    let status = tab.agent_status;
    if status == AgentStatus::Unknown {
        return None;
    }
    let awaits_reply = status == AgentStatus::Done
        && snapshot.agents.iter().any(|agent| {
            agent.tab_id == tab.tab_id
                && agent.awaiting_reply
                && agent.agent_status == AgentStatus::Done
        });
    let mark = if awaits_reply {
        AgentMark::AwaitsReply
    } else if waits_on_job(snapshot, &tab.tab_id, status) {
        AgentMark::WaitsOnJob
    } else {
        AgentMark::None
    };
    Some((status, mark))
}

/// The state icon of a tab line and its colour, as the sidebar draws it: the
/// agent's state (with the question or job mark), or the program mark of a
/// tab without an agent.
pub(super) fn tab_state_icon(
    snapshot: &ClientShellSnapshot,
    tab: &ClientShellTab,
    config: &ClientShellConfig,
) -> (&'static str, Color) {
    let palette = &config.palette;
    match tab_state(snapshot, tab) {
        Some((status, mark)) => (
            agent_icon(status, mark, config.status_indicators),
            agent_color(status, mark, palette),
        ),
        None => (PROGRAM_ICON, palette.overlay0),
    }
}

fn stands_for_a_group(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> bool {
    super::sidebar::displayed_workspaces(snapshot, workspace, collapsed_groups)
        .any(|shown| shown.workspace_id != workspace.workspace_id)
}

fn top_level_tabs<'a>(
    snapshot: &'a ClientShellSnapshot,
    workspace: &'a ClientShellWorkspace,
) -> impl Iterator<Item = &'a ClientShellTab> + 'a {
    snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace.workspace_id && tab.parent_tab_id.is_none())
}

/// U+274F: one cell, no emoji form, in the system symbol fonts.
const PROGRAM_ICON: &str = "❏";

/// The fills of the tab lines. Only the focused space's active tab is blue,
/// a light accent tint so its job counts keep their colours; the active
/// tabs of other spaces are a grey darker than the inactive ones, so they
/// cannot pass for it.
struct TabLineFills {
    /// None when the palette is not RGB: the tab then takes the solid
    /// accent, and its job counts the text colour.
    focused_active: Option<Color>,
    active: Color,
    inactive: Color,
}

impl TabLineFills {
    fn new(palette: &Palette) -> Self {
        use super::render::tabs::blend;
        let dark = is_dark(palette.panel_bg);
        Self {
            // A third of the accent on a light background, half on a dark one:
            // the tint must stand out among the greys (a sixth was too
            // faint), and the job counts keep their colours on it.
            focused_active: blend(
                palette.accent,
                palette.panel_bg,
                1,
                if dark == Some(true) { 2 } else { 3 },
            ),
            // On dark themes surface1 is brighter than the tint and would
            // draw the eye from it.
            active: match dark {
                Some(true) => {
                    blend(palette.surface1, palette.surface0, 1, 2).unwrap_or(palette.surface1)
                }
                _ => palette.surface1,
            },
            inactive: blend(palette.surface0, palette.panel_bg, 1, 2).unwrap_or(palette.surface0),
        }
    }
}

/// Whether an RGB colour is dark; none for other colours.
fn is_dark(color: Color) -> Option<bool> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    let luma = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b);
    Some(luma < 128 * 1000)
}

/// Where a click on the drawn tab lines lands: each line's rect with its tab,
/// each line's disclosure triangle and counts with its tab, and each square's
/// rect with its nested tab.
#[derive(Debug, Default)]
pub(super) struct SpaceTabHits {
    pub(super) lines: Vec<(Rect, String)>,
    pub(super) folds: Vec<(Rect, String)>,
    pub(super) squares: Vec<(Rect, String)>,
    /// Blank slots of closed tabs; a click there does nothing.
    pub(super) gone: Vec<Rect>,
    /// Lines whose label is cut, with the whole label.
    pub(super) tooltips: Vec<super::tooltip::TooltipTarget>,
    /// The square order of each unfolded line, to hold while the pointer
    /// stays over the sidebar.
    pub(super) order: HeldSquares,
}

/// Draws `lines` from the top of `area`, below the space's own rows, each
/// followed by its squares while unfolded, wrapped at `squares_width` (see
/// [`squares_per_row`]). Each line is a tab filled from the label (see
/// [`TabLineFills`]). The state icon stays left of the fill, on the panel
/// background, so it keeps its colour on every line. While the pointer is
/// over one of a line's squares (`hovered_square`), the line names that job
/// in place of its label: a square shows only a glyph.
pub(super) fn render_space_tab_lines(
    buffer: &mut Buffer,
    area: Rect,
    lines: &[SpaceTabLine],
    focused_space: bool,
    squares_width: u16,
    hovered_square: Option<&str>,
    // Columns right of `area` the tab fill continues into: the scrollbar's,
    // whose thin glyph then sits on the fill instead of a white gap.
    fill_past: u16,
    // Columns the lines move right of `area` (see [`tab_indent`]); a line's
    // click rect still starts at `area`, so the gutter selects its tab.
    indent: u16,
    // The tab dragged from one of these lines and where it would land among
    // them (see [`ClientChromeDrag::TabLine`]); ignored for other spaces.
    tab_drag: Option<(&str, Option<usize>)>,
    // The spaces filter query: its matched characters in a label stand out.
    highlight: Option<&str>,
    config: &ClientShellConfig,
) -> SpaceTabHits {
    let palette = &config.palette;
    let fills = TabLineFills::new(palette);
    let x = area.x.saturating_add(3 + indent);
    // A column of the panel background between the icon and the fill.
    let fill_x = x.saturating_add(2);
    // To the right edge, level with the `+` on the space name lines.
    let fill_right = area.right();
    // One column of padding inside the fill on each side.
    let text_x = fill_x.saturating_add(1);
    let right = fill_right.saturating_sub(1);
    let mut hits = SpaceTabHits::default();
    let mut y = area.y;
    // While a tab is dragged, its block is drawn at the slot it would land
    // in; the hits follow what is drawn, so the drop is measured against the
    // geometry frozen at the drag start (see `ClientChromeDrag::TabLine`).
    let dragged =
        tab_drag.and_then(|(tab_id, _)| lines.iter().position(|line| line.tab_id == tab_id));
    let mut order = (0..lines.len()).collect::<Vec<_>>();
    if let Some((source, slot)) = dragged.zip(tab_drag.and_then(|(_, slot)| slot)) {
        let landing = if slot > source { slot - 1 } else { slot };
        order.remove(source);
        order.insert(landing.min(order.len()), source);
    }
    for line_index in order {
        let line = &lines[line_index];
        if y >= area.bottom() {
            break;
        }
        hits.lines
            .push((Rect::new(area.x, y, area.width, 1), line.tab_id.clone()));
        let active_text = Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD);
        let solid = line.active && focused_space && fills.focused_active.is_none();
        let (bg, text_style) = match (line.active, focused_space, fills.focused_active) {
            (true, true, Some(tint)) => (tint, active_text),
            (true, true, None) => (
                palette.accent,
                Style::default()
                    .fg(panel_contrast_fg(palette))
                    .add_modifier(Modifier::BOLD),
            ),
            (true, false, _) => (fills.active, active_text),
            (false, ..) => (fills.inactive, Style::default().fg(palette.overlay1)),
        };
        // The lifted line takes the drag background; themes without one rely
        // on its accent text.
        let lifted = dragged == Some(line_index);
        let (bg, text_style) = if lifted {
            (
                Some(palette.drag_bg)
                    .filter(|drag_bg| *drag_bg != Color::Reset)
                    .unwrap_or(bg),
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (bg, text_style)
        };
        buffer.set_style(
            Rect::new(
                fill_x,
                y,
                fill_right.saturating_add(fill_past).saturating_sub(fill_x),
                1,
            )
            .intersection(buffer.area),
            Style::default().bg(bg),
        );
        // On the solid accent the job colours can vanish, so they take the
        // text colour, as on the tab bar.
        let on_accent = solid.then_some(text_style);
        // A tab without an agent runs a program (a shell, lazygit): a window
        // mark, a square so it cannot pass for an agent state's circle.
        let (icon, icon_style) = match line.state {
            Some((status, mark)) => (
                agent_icon(status, mark, config.status_indicators),
                Style::default().fg(agent_color(status, mark, palette)),
            ),
            None => (PROGRAM_ICON, Style::default().fg(palette.overlay0)),
        };
        super::render::put_text(buffer, x, y, 1, icon, icon_style);
        // The focused space's active tab also has an accent bar in the
        // fill's first column, so it is found by shape, not only by colour.
        if line.active && focused_space && fills.focused_active.is_some() && !lifted {
            super::render::put_text(
                buffer,
                fill_x,
                y,
                1,
                "▌",
                Style::default().fg(palette.accent).bg(bg),
            );
        }
        // A tab with nested tabs ends in a disclosure triangle and their
        // counts, `► ⧖ 1 !1`, which fold and unfold its squares. They keep
        // their room; the label is cut first, then the counts.
        let available = right.saturating_sub(text_x);
        let foldable = !line.squares.is_empty() && available >= 3;
        let jobs_width = segments_width(&line.jobs);
        let jobs_width = if foldable && jobs_width > 0 && jobs_width + 4 <= available {
            jobs_width
        } else {
            0
        };
        let fold_width = match (foldable, jobs_width) {
            (false, _) => 0,
            (true, 0) => 1,
            (true, jobs) => jobs + 2,
        };
        let label_width = available.saturating_sub(if fold_width > 0 { fold_width + 1 } else { 0 });
        let label = truncate(&line.label, label_width as usize);
        if label != line.label {
            hits.tooltips.push(super::tooltip::TooltipTarget {
                rect: Rect::new(text_x, y, label_width, 1),
                id: format!("tab:{}", line.tab_id),
                text: line.label.clone(),
                // The line's own fill, active or not: only its width grows.
                bg: Some(bg),
            });
        }
        super::render::put_text(buffer, text_x, y, label_width, &label, text_style);
        if let Some(positions) =
            highlight.and_then(|query| super::space_filter::match_positions(query, &label))
        {
            let chars = label.chars().collect::<Vec<_>>();
            let mut column = 0u16;
            for (at, c) in chars.iter().enumerate() {
                let width = unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0) as u16;
                if positions.contains(&at) && column + width <= label_width {
                    buffer.set_style(
                        Rect::new(text_x + column, y, width.max(1), 1).intersection(buffer.area),
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    );
                }
                column += width;
            }
        }
        if fold_width > 0 {
            let fold_x = right.saturating_sub(fold_width);
            super::render::put_text(
                buffer,
                fold_x,
                y,
                1,
                if line.unfolded { "▼" } else { "►" },
                on_accent.unwrap_or_else(|| Style::default().fg(palette.overlay1)),
            );
            // From the triangle to the fill's end, so the padding counts too.
            hits.folds.push((
                Rect::new(fold_x, y, fill_right.saturating_sub(fold_x), 1),
                line.tab_id.clone(),
            ));
        }
        if jobs_width > 0 {
            // Right-aligned, so the counts of all lines form a column.
            render_segments(
                buffer,
                right.saturating_sub(jobs_width),
                y,
                right,
                &line.jobs,
                on_accent,
                palette,
            );
        }
        y = y.saturating_add(1);
        if line.unfolded {
            hits.order.insert(
                line.tab_id.clone(),
                line.squares
                    .iter()
                    .map(|square| (square.tab_id.clone(), square.label.clone()))
                    .collect(),
            );
        }
        if line.unfolded {
            let per_row = squares_per_row(squares_width);
            for row in line.squares.chunks(per_row) {
                if y >= area.bottom() {
                    break;
                }
                let mut square_x = area.x.saturating_add(SQUARES_INDENT + indent);
                for square in row {
                    let rect = Rect::new(square_x, y, SQUARE_WIDTH, 1).intersection(area);
                    render_square(buffer, rect, square, &fills, palette);
                    if square.gone {
                        hits.gone.push(rect);
                    } else {
                        hits.squares.push((rect, square.tab_id.clone()));
                    }
                    // The hovered square's job is named in a tooltip right
                    // of it. The tooltip takes no hits, so moving onto a
                    // square it covers names that one instead.
                    // It has the square's fill, so the two read as one;
                    // the square already shows the state.
                    if hovered_square == Some(square.tab_id.as_str()) && !square.gone {
                        hits.tooltips.push(super::tooltip::TooltipTarget {
                            rect,
                            id: super::tooltip::square_tooltip_id(&square.tab_id),
                            text: square.label.clone(),
                            bg: Some(square_fill(square, &fills, palette)),
                        });
                    }
                    square_x = square_x.saturating_add(SQUARE_WIDTH + SQUARE_GAP);
                }
                y = y.saturating_add(1);
            }
            // The empty row after the squares.
            y = y.saturating_add(1);
        }
    }
    hits
}

/// A square's fill: the inactive tab fill, or the active tab's tint while
/// its job is open.
fn square_fill(square: &TabSquare, fills: &TabLineFills, palette: &Palette) -> Color {
    match (square.focused, fills.focused_active) {
        (true, Some(tint)) => tint,
        (true, None) => palette.accent,
        (false, _) => fills.inactive,
    }
}

/// A nested tab's square: its status glyph in the status colour on the
/// inactive tab fill, or on the active tab's tint while it is open. `!` and
/// `✓` are bold, as a finished job's outcome.
fn render_square(
    buffer: &mut Buffer,
    rect: Rect,
    square: &TabSquare,
    fills: &TabLineFills,
    palette: &Palette,
) {
    if rect.is_empty() {
        return;
    }
    let solid = square.focused && fills.focused_active.is_none();
    let bg = square_fill(square, fills, palette);
    buffer.set_style(rect, Style::default().bg(bg));
    if square.gone {
        return;
    }
    let glyph = super::tab_groups::status_icon(square.status).unwrap_or("•");
    let mut style = Style::default().fg(if solid {
        panel_contrast_fg(palette)
    } else {
        super::render::tabs::tab_status_color(square.status, palette).unwrap_or(palette.overlay0)
    });
    if matches!(
        square.status,
        Some(TabStatus::Failed | TabStatus::Succeeded)
    ) {
        style = style.add_modifier(Modifier::BOLD);
    }
    super::render::put_text(buffer, rect.x.saturating_add(1), rect.y, 1, glyph, style);
}

fn segments_width(segments: &[(Option<TabStatus>, String)]) -> u16 {
    let text = segments
        .iter()
        .map(|(_, text)| unicode_width::UnicodeWidthStr::width(text.as_str()))
        .sum::<usize>();
    (text + segments.len().saturating_sub(1)).min(u16::MAX as usize) as u16
}

/// Job counts from `x` up to `right`, coloured like the space row's counts
/// and the child-tab row.
fn render_segments(
    buffer: &mut Buffer,
    mut x: u16,
    y: u16,
    right: u16,
    segments: &[(Option<TabStatus>, String)],
    // Replaces the status colours, e.g. on an accent fill.
    style: Option<Style>,
    palette: &Palette,
) {
    for (index, (status, text)) in segments.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(1);
        }
        let width = right.saturating_sub(x);
        if width == 0 {
            break;
        }
        let text = truncate(text, width as usize);
        super::render::put_text(
            buffer,
            x,
            y,
            width,
            &text,
            style.unwrap_or_else(|| {
                Style::default().fg(super::render::tabs::tab_status_color(*status, palette)
                    .unwrap_or(palette.overlay0))
            }),
        );
        x = x.saturating_add(unicode_width::UnicodeWidthStr::width(text.as_str()) as u16);
    }
}

/// `text` cut to `width` columns, ending in `…` when cut.
fn truncate(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let char_width = character.width().unwrap_or(0);
        if used + char_width + 1 > width {
            break;
        }
        out.push(character);
        used += char_width;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;

    #[test]
    fn tab_line_fills_differ_in_every_theme() {
        for name in crate::config::THEME_NAMES {
            let palette = Palette::from_name(name).expect("theme");
            let fills = TabLineFills::new(&palette);
            let focused_active = fills.focused_active.unwrap_or(palette.accent);
            assert_ne!(focused_active, fills.active, "{name}");
            assert_ne!(focused_active, fills.inactive, "{name}");
            assert_ne!(fills.active, fills.inactive, "{name}");
        }
    }

    #[test]
    fn a_palette_without_rgb_keeps_the_solid_accent() {
        let fills = TabLineFills::new(&Palette::from_name("terminal").expect("theme"));
        assert_eq!(fills.focused_active, None);
    }

    fn config(tabs: bool) -> ClientShellConfig {
        let mut config = ClientShellConfig::from_config(&crate::config::Config::default());
        config.spaces.tabs = tabs;
        config
    }

    fn tab(
        tab_id: &str,
        parent_tab_id: Option<&str>,
        status: Option<TabStatus>,
    ) -> crate::protocol::ClientShellTab {
        crate::protocol::ClientShellTab {
            tab_id: tab_id.into(),
            label: tab_id.into(),
            parent_tab_id: parent_tab_id.map(str::to_owned),
            status,
            focused: false,
            ..super::super::tests::snapshot().tabs[0].clone()
        }
    }

    fn snapshot_with(tabs: Vec<crate::protocol::ClientShellTab>) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.tabs = tabs;
        snapshot
    }

    #[test]
    fn lists_top_level_tabs_with_their_jobs_only_when_enabled() {
        let mut snapshot = snapshot_with(vec![
            tab("tab_1", None, None),
            tab("job_1", Some("tab_1"), Some(TabStatus::Running)),
            tab("job_2", Some("tab_1"), Some(TabStatus::Failed)),
            tab("job_3", Some("tab_1"), Some(TabStatus::Succeeded)),
            tab("tab_2", None, None),
            tab("elsewhere", None, None),
        ]);
        snapshot.tabs[5].workspace_id = "ws_other".into();
        snapshot.tabs[4].agent_status = AgentStatus::Blocked;
        let mut workspace = snapshot.workspaces[0].clone();
        workspace.active_tab_id = "tab_1".into();

        assert!(space_tab_lines(
            &snapshot,
            &workspace,
            &HashSet::new(),
            &HashSet::new(),
            &HeldSquares::new(),
            &config(false)
        )
        .is_empty());
        let lines = space_tab_lines(
            &snapshot,
            &workspace,
            &HashSet::new(),
            &HashSet::new(),
            &HeldSquares::new(),
            &config(true),
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.tab_id.as_str(), line.active, line.jobs.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "tab_1",
                    true,
                    vec![
                        (Some(TabStatus::Running), "◑ 1".to_owned()),
                        (Some(TabStatus::Failed), "!1".to_owned()),
                        (Some(TabStatus::Succeeded), "✓1".to_owned()),
                    ]
                ),
                ("tab_2", false, Vec::new()),
            ]
        );
        assert_eq!(
            lines[1].state,
            Some((AgentStatus::Blocked, AgentMark::None))
        );
        // Every job is on a line, so the space row counts none.
        assert_eq!(
            space_row_tab_jobs(&snapshot, &workspace, &HashSet::new(), &lines),
            (0, 0)
        );
    }

    #[test]
    fn an_active_nested_tab_marks_its_parents_line() {
        let snapshot = snapshot_with(vec![
            tab("tab_1", None, None),
            tab("job_1", Some("tab_1"), None),
            tab("tab_2", None, None),
        ]);
        let mut workspace = snapshot.workspaces[0].clone();
        workspace.active_tab_id = "job_1".into();

        let active = space_tab_lines(
            &snapshot,
            &workspace,
            &HashSet::new(),
            &HashSet::new(),
            &HeldSquares::new(),
            &config(true),
        )
        .into_iter()
        .filter(|line| line.active)
        .map(|line| line.tab_id)
        .collect::<Vec<_>>();
        assert_eq!(active, ["tab_1"]);
    }

    #[test]
    fn jobs_nested_under_a_missing_tab_stay_on_the_space_row() {
        let snapshot = snapshot_with(vec![
            tab("tab_1", None, None),
            tab("orphan", Some("gone"), Some(TabStatus::Failed)),
        ]);
        let workspace = snapshot.workspaces[0].clone();

        let lines = space_tab_lines(
            &snapshot,
            &workspace,
            &HashSet::new(),
            &HashSet::new(),
            &HeldSquares::new(),
            &config(true),
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(
            space_row_tab_jobs(&snapshot, &workspace, &HashSet::new(), &lines),
            (0, 1)
        );
    }

    #[test]
    fn a_collapsed_space_lists_no_tabs_and_keeps_its_jobs_on_its_row() {
        let snapshot = snapshot_with(vec![
            tab("tab_1", None, None),
            tab("job_1", Some("tab_1"), Some(TabStatus::Failed)),
        ]);
        let workspace = snapshot.workspaces[0].clone();
        let collapsed = HashSet::from([tabs_collapse_key(&workspace.workspace_id)]);

        let lines = space_tab_lines(
            &snapshot,
            &workspace,
            &collapsed,
            &HashSet::new(),
            &HeldSquares::new(),
            &config(true),
        );
        assert!(lines.is_empty());
        assert_eq!(
            space_row_tab_jobs(&snapshot, &workspace, &collapsed, &lines),
            (0, 1)
        );
    }

    #[test]
    fn listing_tabs_drops_the_spaces_own_state_icon() {
        let workspace = super::super::tests::snapshot().workspaces[0].clone();
        let has_icon = |tabs: bool| {
            super::super::sidebar::workspace_rows(
                &workspace,
                AgentStatus::Working,
                (0, 0),
                false,
                &config(tabs).spaces,
            )
            .iter()
            .flatten()
            .any(|token| matches!(token.kind, crate::ui::ResolvedTokenKind::StateIcon))
        };

        assert!(has_icon(false));
        assert!(!has_icon(true));
    }

    #[test]
    fn unfolded_squares_list_the_nested_tabs_and_wrap() {
        let mut tabs = vec![tab("tab_1", None, None), tab("tab_2", None, None)];
        tabs.extend((0..7).map(|index| {
            tab(
                &format!("job_{index}"),
                Some("tab_1"),
                Some(TabStatus::Running),
            )
        }));
        let snapshot = snapshot_with(tabs);
        let workspace = snapshot.workspaces[0].clone();
        let lines = |unfolded: &[&str]| {
            let unfolded = unfolded.iter().map(|id| (*id).to_owned()).collect();
            space_tab_lines(
                &snapshot,
                &workspace,
                &HashSet::new(),
                &unfolded,
                &HeldSquares::new(),
                &config(true),
            )
        };

        let folded = lines(&[]);
        assert_eq!(folded[0].squares.len(), 7);
        assert_eq!(folded[0].height(26), 1);
        // A tab without nested tabs never unfolds.
        let unfolded = lines(&["tab_1", "tab_2"]);
        assert!(!unfolded[1].unfolded);
        assert_eq!(unfolded[1].height(26), 1);
        // 26 columns hold (26 - 5 + 1) / 4 = 5 squares a row; the line, its
        // square rows and an empty row after them.
        assert_eq!(squares_per_row(26), 5);
        assert_eq!(unfolded[0].height(26), 4);
        assert_eq!(unfolded[0].height(40), 3);
        // Too narrow for one square still lays one per row.
        assert_eq!(squares_per_row(2), 1);
    }

    #[test]
    fn truncation_keeps_the_width_and_marks_the_cut() {
        assert_eq!(truncate("Name unnamed tabs after", 10), "Name unna…");
        assert_eq!(truncate("short", 10), "short");
    }
}
