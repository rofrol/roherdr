use super::*;
use ratatui::{
    text::Line,
    widgets::{Paragraph, Widget},
};

fn workspace_selection_background(palette: &Palette) -> ratatui::style::Color {
    if palette.selection_bg == ratatui::style::Color::Reset {
        palette.active_row_bg
    } else {
        palette.selection_bg
    }
}

pub(in crate::client::shell) fn workspace_active_background(
    palette: &Palette,
    navigating: bool,
) -> ratatui::style::Color {
    // The fallback cursor shares the active-row color; only fill the cursor while navigating.
    if navigating && palette.selection_bg == ratatui::style::Color::Reset {
        palette.sidebar_bg
    } else {
        palette.active_row_bg
    }
}

pub(in crate::client::shell) fn collapsed_sidebar_sections(
    area: Rect,
) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.is_empty() {
        return (Rect::default(), None, Rect::default());
    }
    if content.height < 7 {
        return (content, None, Rect::default());
    }
    let workspace_height = content.height.div_ceil(2);
    let divider_y = content.y + workspace_height;
    let detail_height = content.height.saturating_sub(workspace_height + 1);
    (
        Rect::new(content.x, content.y, content.width, workspace_height),
        Some(divider_y),
        Rect::new(content.x, divider_y + 1, content.width, detail_height),
    )
}

pub(crate) fn render_collapsed_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    selected_workspace_id: Option<&str>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    let selection_background = workspace_selection_background(palette);
    let active_background = workspace_active_background(palette, selected_workspace_id.is_some());
    render_sidebar_background(buffer, area, palette);
    let (workspace_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    for (index, workspace) in snapshot
        .workspaces
        .iter()
        .take(workspace_area.height as usize)
        .enumerate()
    {
        let rect = Rect::new(
            workspace_area.x,
            workspace_area.y + index as u16,
            workspace_area.width,
            1,
        );
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str());
        if selected {
            buffer.set_style(rect, Style::default().bg(selection_background));
        } else if workspace.focused {
            buffer.set_style(rect, Style::default().bg(active_background));
        }
        let number_style = if selected {
            Style::default()
                .fg(palette.overlay1)
                .bg(selection_background)
        } else if workspace.focused {
            Style::default().fg(palette.text).bg(active_background)
        } else {
            Style::default().fg(palette.overlay0)
        };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            number_style,
        );
        let status = workspace.agent_status;
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            aggregate_icon(snapshot, status, config.status_indicators, |agent| {
                agent.workspace_id == workspace.workspace_id
            }),
            Style::default().fg(status_color(status, palette)),
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: false,
            group_toggle: None,
        });
    }

    if let Some(divider_y) = divider_y {
        put_text(
            buffer,
            workspace_area.x,
            divider_y,
            workspace_area.width,
            &"─".repeat(workspace_area.width as usize),
            Style::default().fg(palette.surface_dim),
        );
    }

    let detail_content = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    for (index, pane_id) in super::ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .take(detail_content.height as usize)
        .enumerate()
    {
        let Some(agent) = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
        else {
            continue;
        };
        let rect = Rect::new(
            detail_content.x,
            detail_content.y + index as u16,
            detail_content.width,
            1,
        );
        if agent.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            Style::default().fg(if agent.focused {
                palette.text
            } else {
                palette.overlay0
            }),
        );
        let mark = agent_mark(snapshot, agent);
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            agent_icon(agent.agent_status, mark, config.status_indicators),
            Style::default().fg(agent_color(agent.agent_status, mark, palette)),
        );
        hits.agents.push((rect, pane_id));
    }
    hits.sidebar_toggle = if area.is_empty() || workspace_area.width == 0 {
        Rect::default()
    } else {
        Rect::new(
            workspace_area.x + workspace_area.width / 2,
            area.bottom().saturating_sub(1),
            1,
            1,
        )
    };
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "»",
        if super::super::global_menu::global_menu_attention(snapshot) {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        },
    );
}

/// The commit shown in the sidebar's last row: the endpoint's, since it runs
/// the session, and this client's too when the two builds differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::client::shell) struct BuildRow<'a> {
    commit: &'a str,
    differing_client: Option<&'a str>,
}

pub(in crate::client::shell) fn build_row<'a>(
    endpoint: Option<&'a str>,
    client: Option<&'a str>,
) -> Option<BuildRow<'a>> {
    let hash = |line: &'a str| line.split(' ').next().unwrap_or(line);
    match (endpoint, client) {
        (Some(endpoint), Some(client)) if hash(endpoint) != hash(client) => Some(BuildRow {
            commit: endpoint,
            differing_client: Some(hash(client)),
        }),
        // Endpoints older than the commit field leave only the client's.
        (commit, other) => commit.or(other).map(|commit| BuildRow {
            commit,
            differing_client: None,
        }),
    }
}

/// Splits the sidebar's last row off for the build commit; the sections
/// share what is left.
pub(in crate::client::shell) fn split_build_row(area: Rect, row: Option<BuildRow>) -> (Rect, Rect) {
    if row.is_none() || area.height == 0 {
        return (area, Rect::default());
    }
    let sections = Rect::new(area.x, area.y, area.width, area.height - 1);
    let row = Rect::new(area.x, sections.bottom(), area.width, 1);
    (sections, row)
}

/// Columns at the right of the build row kept for the sidebar toggle and the
/// sidebar's own divider.
const BUILD_ROW_RIGHT_RESERVE: u16 = 3;

pub(in crate::client::shell) fn render_build_row(
    buffer: &mut Buffer,
    area: Rect,
    row: BuildRow,
    palette: &Palette,
) {
    let right = area.right().saturating_sub(BUILD_ROW_RIGHT_RESERVE);
    let (hash, subject) = row.commit.split_once(' ').unwrap_or((row.commit, ""));
    let x = super::render::put_segment(
        buffer,
        area.x.saturating_add(1),
        area.y,
        right,
        hash,
        Style::default().fg(palette.overlay1),
    );
    // A different client build matters more than the subject.
    let (tail, style) = match row.differing_client {
        Some(client) => (format!("≠ cli {client}"), Style::default().fg(palette.red)),
        None => (subject.to_owned(), Style::default().fg(palette.overlay0)),
    };
    let room = right.saturating_sub(x).saturating_sub(1);
    if tail.is_empty() || room == 0 {
        return;
    }
    super::render::put_segment(
        buffer,
        x.saturating_add(1),
        area.y,
        right,
        &crate::ui::truncate_end(&tail, room as usize),
        style,
    );
}

pub(crate) fn render_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    hits.sidebar_divider = if area.is_empty() {
        Rect::default()
    } else {
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height)
    };
    let build = build_row(
        snapshot.build_commit.as_deref(),
        crate::build_info::commit_line(),
    );
    let (sections, build_area) = split_build_row(area, build);
    hits.sidebar_sections = sections;
    let agents_panel = config.show_agents_panel;
    let (workspace_area, detail_area) = if agents_panel {
        hits.sidebar_section_divider =
            crate::ui::sidebar_section_divider_rect(sections, state.sidebar_section_split);
        crate::ui::expanded_sidebar_sections(sections, state.sidebar_section_split)
    } else {
        super::usage::split_spaces_and_footer(sections, state.usage)
    };
    let mut entries = super::space_sort::sorted_entries(
        snapshot,
        workspace_entries(snapshot, state.collapsed_groups),
        state.collapsed_groups,
        state.space_sort,
    );
    // While a space is dragged the list shows where it would land, and the
    // header says so in words. With the pointer outside the list the order
    // stays, the block stays lifted and the header says a release cancels.
    let drag = state
        .dragged_workspace_id
        .and_then(|source| match state.workspace_drop_before {
            Some(before) => {
                let preview = entries_with_drag(snapshot, &entries, source, before)?;
                let hint = drag_hint(snapshot, &entries, &preview, source);
                Some((Some(preview), hint))
            }
            None => Some((None, "release cancels · Esc".to_owned())),
        });
    let header_hint = drag.as_ref().map(|(_, hint)| hint.as_str()).or(state
        .workspace_drag_refusal
        .map(super::WorkspaceDragRefusal::hint));
    match header_hint {
        Some(hint) => put_text(
            buffer,
            workspace_area.x,
            workspace_area.y,
            workspace_area.width,
            &format!(" {hint}"),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        None => {
            let buttons = super::space_sort::render_sort_header(
                buffer,
                Rect::new(workspace_area.x, workspace_area.y, workspace_area.width, 1)
                    .intersection(workspace_area),
                state.space_sort,
                palette,
            );
            if config.mouse_capture {
                hits.space_sort_buttons = buttons;
            }
        }
    }
    let mut dragged_family = HashSet::new();
    if let Some((preview, _)) = drag {
        if let Some(preview) = preview {
            entries = preview;
        }
        if let Some(source) = state.dragged_workspace_id {
            dragged_family = family_ids(snapshot, &entries, source);
        }
    }
    let pressed_family = state
        .pressed_workspace_id
        .map(|pressed| family_ids(snapshot, &entries, pressed))
        .unwrap_or_default();
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    hits.workspace_body = body;
    // Unfolded squares wrap at the block's width, which loses a column to the
    // scrollbar when the list overflows: measure without it first, and again
    // with it if it is needed (narrower only adds rows, so that settles it).
    let measure = |squares_width: u16| -> Vec<u16> {
        entries
            .iter()
            .map(|entry| {
                snapshot
                    .workspaces
                    .get(entry.index)
                    .map(|workspace| {
                        let tab_lines = super::space_tabs::space_tab_lines(
                            snapshot,
                            workspace,
                            state.collapsed_groups,
                            state.unfolded_squares,
                            config,
                        );
                        let rows = workspace_rows(
                            workspace,
                            displayed_workspace_status(snapshot, workspace, state.collapsed_groups),
                            super::space_tabs::space_row_tab_jobs(
                                snapshot,
                                workspace,
                                state.collapsed_groups,
                                &tab_lines,
                            ),
                            entry.indented,
                            &config.spaces,
                        )
                        .len()
                        .max(1);
                        let tab_rows = tab_lines
                            .iter()
                            .map(|line| usize::from(line.height(squares_width)))
                            .sum::<usize>();
                        (rows + tab_rows).min(u16::MAX as usize) as u16
                    })
                    .unwrap_or(1)
            })
            .collect::<Vec<_>>()
    };
    let gaps = entries
        .iter()
        .enumerate()
        .map(|(index, _)| {
            entries
                .get(index + 1)
                .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap)
        })
        .collect::<Vec<_>>();
    let mut squares_width = body.width;
    let mut row_heights = measure(squares_width);
    let total = row_heights
        .iter()
        .chain(&gaps)
        .fold(0u16, |sum, rows| sum.saturating_add(*rows));
    if total > body.height && body.width > 1 {
        squares_width = body.width - 1;
        row_heights = measure(squares_width);
    }
    let mut metrics = super::scroll::list_scroll_metrics(
        &row_heights,
        &gaps,
        body.height,
        *state.workspace_scroll,
    );
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = entries
            .iter()
            .position(|entry| snapshot.workspaces[entry.index].focused)
        {
            *state.workspace_scroll = super::scroll::list_scroll_start_to_reveal(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
                target,
            );
            metrics = super::scroll::list_scroll_metrics(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
            );
        }
    }
    hits.workspace_max_scroll = metrics.max_offset_from_bottom;
    hits.workspace_scroll_metrics = Some(metrics);
    *state.workspace_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (entry_position, entry) in entries.iter().enumerate().skip(*state.workspace_scroll) {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let status = displayed_workspace_status(snapshot, workspace, state.collapsed_groups);
        let tab_lines = super::space_tabs::space_tab_lines(
            snapshot,
            workspace,
            state.collapsed_groups,
            state.unfolded_squares,
            config,
        );
        let tab_jobs = super::space_tabs::space_row_tab_jobs(
            snapshot,
            workspace,
            state.collapsed_groups,
            &tab_lines,
        );
        let rows = workspace_rows(workspace, status, tab_jobs, entry.indented, &config.spaces);
        let own_rows = rows.len().max(1).min(u16::MAX as usize) as u16;
        let row_height = row_heights
            .get(entry_position)
            .copied()
            .unwrap_or(own_rows)
            .min(body.height);
        if y.saturating_add(row_height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, row_height);
        let selected = state.selected_workspace_id.is_some_and(|target| {
            target.matches(state.active_endpoint_id, &workspace.workspace_id)
        });
        let dragged = dragged_family.contains(workspace.workspace_id.as_str());
        let pressed = pressed_family.contains(workspace.workspace_id.as_str());
        // Grey on hover, accent while pressed or dragged; the name takes the
        // same colour, so the block is found after it jumps.
        let grab_color = if dragged || pressed {
            Some(palette.accent)
        } else if state.hovered_workspace_id == Some(workspace.workspace_id.as_str()) {
            Some(palette.overlay1)
        } else {
            None
        };
        // A pressed or dragged block gets its own background, whichever
        // space it is, together with the accent grip; themes without one
        // (terminal) rely on the accent bar.
        let drag_bg = Some(palette.drag_bg)
            .filter(|bg| (dragged || pressed) && *bg != ratatui::style::Color::Reset);
        if let Some(bg) = drag_bg {
            buffer.set_style(rect, Style::default().bg(bg));
        } else if selected {
            buffer.set_style(rect, Style::default().bg(palette.selection_bg));
        } else if workspace.focused && !config.spaces.tabs {
            // With vertical tabs only the tab lines have a background.
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        let icon = aggregate_icon(snapshot, status, config.status_indicators, |agent| {
            displayed_workspaces(snapshot, workspace, state.collapsed_groups)
                .any(|shown| shown.workspace_id == agent.workspace_id)
        });
        render_workspace_rows(
            buffer,
            rect,
            status,
            icon,
            entry,
            rows,
            workspace.focused,
            selected,
            state.selected_workspace_id.is_some(),
            grab_color
                .filter(|_| dragged || pressed)
                .map(|name| (name, drag_bg)),
            config,
        );
        if let Some(color) = grab_color {
            // A grip at the name line's right edge, left of the group
            // chevron, in the spacer column the name never reaches.
            let hovered = state.hovered_workspace_id == Some(workspace.workspace_id.as_str());
            if (dragged || pressed || hovered) && rect.width >= 4 {
                put_text(
                    buffer,
                    rect.right().saturating_sub(2),
                    rect.y,
                    1,
                    "⋮",
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                );
            }
        }
        let tab_hits = super::space_tabs::render_space_tab_lines(
            buffer,
            Rect::new(
                rect.x,
                rect.y.saturating_add(own_rows),
                rect.width,
                rect.height.saturating_sub(own_rows),
            ),
            &tab_lines,
            workspace.focused,
            squares_width,
            config,
        );
        hits.space_tabs.extend(tab_hits.lines);
        hits.space_tab_folds.extend(tab_hits.folds);
        hits.space_tab_squares.extend(tab_hits.squares);
        let group_toggle = if config.spaces.tabs {
            super::space_tabs::render_space_disclosure(
                buffer,
                rect,
                snapshot,
                entry,
                workspace,
                state.collapsed_groups,
                config,
            )
        } else {
            render_parent_group_toggle(
                buffer,
                rect,
                snapshot,
                entry.index,
                state.collapsed_groups,
                palette,
            )
        };
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: entry.indented,
            group_toggle,
        });
        let gap = entries
            .get(entry_position + 1)
            .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap);
        y = y.saturating_add(row_height + gap);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.workspace_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    if config.mouse_capture {
        hits.new_workspace = Rect::new(
            workspace_area.x,
            footer_y,
            5.min(workspace_area.width),
            u16::from(workspace_area.height > 0),
        );
        put_text(
            buffer,
            workspace_area.x,
            footer_y,
            workspace_area.width,
            " new",
            Style::default().fg(palette.overlay0),
        );
        let attention = super::super::global_menu::global_menu_attention(snapshot);
        let launcher_width = if attention { 8 } else { 6 }.min(workspace_area.width);
        hits.global_launcher = Rect::new(
            workspace_area.right().saturating_sub(launcher_width),
            footer_y,
            launcher_width,
            1,
        );
        if attention {
            let start_x = workspace_area.right().saturating_sub(6);
            put_text(
                buffer,
                start_x,
                footer_y,
                2,
                "● ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            put_text(
                buffer,
                start_x.saturating_add(2),
                footer_y,
                4,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        } else {
            put_right_text(
                buffer,
                workspace_area,
                footer_y,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        }
    }

    let (detail_area, usage_area) = if agents_panel {
        super::usage::split_usage_footer(detail_area, state.usage)
    } else {
        (Rect::default(), detail_area)
    };
    if let Some(report) = state.usage.filter(|_| !usage_area.is_empty()) {
        super::usage::render_usage_footer(
            buffer,
            usage_area,
            report,
            crate::usage::now_unix(),
            palette,
            hits,
        );
    }
    if agents_panel {
        super::render_agent_panel(
            buffer,
            detail_area,
            snapshot,
            config,
            state.agent_scroll,
            hits,
        );
    }

    if let Some(build) = build {
        render_build_row(buffer, build_area, build, palette);
    }
    hits.sidebar_toggle = Rect::new(
        area.right().saturating_sub(2),
        area.bottom().saturating_sub(1),
        u16::from(area.width > 1),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "«",
        Style::default().fg(palette.overlay0),
    );
}

/// `entries` as the drag would leave them: the dragged space and its
/// indented worktrees moved before `before`, or to the end. `None` when the
/// dragged space or `before` is not a top-level entry.
fn entries_with_drag(
    snapshot: &ClientShellSnapshot,
    entries: &[WorkspaceEntry],
    source: &str,
    before: Option<&str>,
) -> Option<Vec<WorkspaceEntry>> {
    let id = |entry: &WorkspaceEntry| {
        snapshot
            .workspaces
            .get(entry.index)
            .map(|workspace| workspace.workspace_id.as_str())
    };
    let start = entries
        .iter()
        .position(|entry| !entry.indented && id(entry) == Some(source))?;
    let end = entries[start + 1..]
        .iter()
        .position(|entry| !entry.indented)
        .map_or(entries.len(), |offset| start + 1 + offset);
    let mut rest = entries.to_vec();
    let family = rest.drain(start..end).collect::<Vec<_>>();
    let at = match before {
        Some(before) => rest
            .iter()
            .position(|entry| !entry.indented && id(entry) == Some(before))?,
        None => rest.len(),
    };
    rest.splice(at..at, family);
    Some(rest)
}

/// Ids of the dragged space and its indented worktrees.
fn family_ids<'a>(
    snapshot: &'a ClientShellSnapshot,
    entries: &[WorkspaceEntry],
    source: &str,
) -> HashSet<&'a str> {
    let mut ids = HashSet::new();
    let mut inside = false;
    for entry in entries {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        if !entry.indented {
            inside = workspace.workspace_id == source;
        }
        if inside {
            ids.insert(workspace.workspace_id.as_str());
        }
    }
    ids
}

/// `herdr → before try-roguix`, `herdr → end` or `no change · Esc`; the
/// sidebar is narrow, so the words are few.
fn drag_hint(
    snapshot: &ClientShellSnapshot,
    entries: &[WorkspaceEntry],
    preview: &[WorkspaceEntry],
    source: &str,
) -> String {
    let order = |entries: &[WorkspaceEntry]| {
        entries
            .iter()
            .filter(|entry| !entry.indented)
            .map(|entry| entry.index)
            .collect::<Vec<_>>()
    };
    let (before, after) = (order(entries), order(preview));
    if before == after {
        return "no change · Esc".to_owned();
    }
    let label = |index: usize| {
        snapshot
            .workspaces
            .get(index)
            .map_or("?", |workspace| workspace.label.as_str())
    };
    let Some(position) = after.iter().position(|index| {
        snapshot
            .workspaces
            .get(*index)
            .is_some_and(|workspace| workspace.workspace_id == source)
    }) else {
        return "no change · Esc".to_owned();
    };
    let name = label(after[position]);
    match after.get(position + 1) {
        Some(next) => format!("{name} → before {}", label(*next)),
        None => format!("{name} → end"),
    }
}

pub(crate) fn workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let mut members = HashMap::<&str, Vec<usize>>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            members.entry(&worktree.key).or_default().push(index);
        }
    }
    let grouped = members
        .iter()
        .filter(|(_, indices)| {
            indices.iter().any(|index| {
                snapshot.workspaces[*index]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            }) && indices.iter().any(|index| {
                snapshot.workspaces[*index]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| !worktree.is_linked_worktree)
            })
        })
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    let mut emitted = HashSet::<&str>::new();
    let mut entries = Vec::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let Some(worktree) = workspace
            .worktree
            .as_ref()
            .filter(|worktree| grouped.contains(worktree.key.as_str()))
        else {
            entries.push(WorkspaceEntry {
                index,
                indented: false,
                last_child: false,
            });
            continue;
        };
        if !emitted.insert(&worktree.key) {
            continue;
        }
        let Some(group_members) = members.get(worktree.key.as_str()) else {
            continue;
        };
        for parent in group_members.iter().copied().filter(|member| {
            snapshot.workspaces[*member]
                .worktree
                .as_ref()
                .is_some_and(|worktree| !worktree.is_linked_worktree)
        }) {
            entries.push(WorkspaceEntry {
                index: parent,
                indented: false,
                last_child: false,
            });
        }
        if collapsed_groups.contains(&worktree.key) {
            if let Some(active) = group_members.iter().copied().find(|member| {
                let workspace = &snapshot.workspaces[*member];
                workspace.focused
                    && workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
            }) {
                entries.push(WorkspaceEntry {
                    index: active,
                    indented: true,
                    last_child: true,
                });
            }
            continue;
        }
        let children = group_members
            .iter()
            .copied()
            .filter(|member| {
                snapshot.workspaces[*member]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        for (child_index, child) in children.iter().enumerate() {
            entries.push(WorkspaceEntry {
                index: *child,
                indented: true,
                last_child: child_index + 1 == children.len(),
            });
        }
    }
    entries
}

pub(in crate::client::shell) fn parent_group_key(
    snapshot: &ClientShellSnapshot,
    index: usize,
) -> Option<String> {
    let workspace = snapshot.workspaces.get(index)?;
    let worktree = workspace.worktree.as_ref()?;
    if worktree.is_linked_worktree {
        return None;
    }
    snapshot
        .workspaces
        .iter()
        .any(|candidate| {
            candidate.worktree.as_ref().is_some_and(|candidate| {
                candidate.key == worktree.key && candidate.is_linked_worktree
            })
        })
        .then(|| worktree.key.clone())
}

pub(in crate::client::shell) fn workspace_close_is_group(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
) -> bool {
    let Some(worktree) = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
    else {
        return false;
    };
    let mut has_child = false;
    for member in &snapshot.workspaces {
        if member.workspace_id == workspace.workspace_id {
            continue;
        }
        if let Some(candidate) = member
            .worktree
            .as_ref()
            .filter(|candidate| candidate.key == worktree.key)
        {
            if !candidate.is_linked_worktree {
                return false;
            }
            has_child = true;
        }
    }
    has_child
}

pub(in crate::client::shell) fn render_parent_group_toggle(
    buffer: &mut Buffer,
    workspace_rect: Rect,
    snapshot: &ClientShellSnapshot,
    workspace_index: usize,
    collapsed_groups: &HashSet<String>,
    palette: &Palette,
) -> Option<(Rect, String)> {
    let key = parent_group_key(snapshot, workspace_index)?;
    let toggle = Rect::new(
        workspace_rect.right().saturating_sub(1),
        workspace_rect.y,
        1,
        1,
    );
    put_text(
        buffer,
        toggle.x,
        toggle.y,
        toggle.width,
        if collapsed_groups.contains(&key) {
            "▸"
        } else {
            "▾"
        },
        Style::default().fg(palette.accent),
    );
    Some((toggle, key))
}

pub(in crate::client::shell) fn displayed_workspace_status(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> crate::api::schema::AgentStatus {
    displayed_workspaces(snapshot, workspace, collapsed_groups)
        .map(|candidate| candidate.agent_status)
        .max_by_key(|status| status_priority(*status))
        .unwrap_or(workspace.agent_status)
}

/// Running and failed tabs of the workspace, or of its whole group while the
/// group is collapsed, so a job in a hidden worktree still shows.
pub(in crate::client::shell) fn displayed_workspace_tab_jobs(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> (usize, usize) {
    use crate::api::schema::TabStatus;
    let mut counts = (0, 0);
    for candidate in displayed_workspaces(snapshot, workspace, collapsed_groups) {
        for tab in snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == candidate.workspace_id)
        {
            match tab.status {
                Some(TabStatus::Running) => counts.0 += 1,
                Some(TabStatus::Failed) => counts.1 += 1,
                _ => {}
            }
        }
    }
    counts
}

/// The workspace itself, or every workspace of its group when it is the
/// collapsed parent row that stands for them.
pub(in crate::client::shell) fn displayed_workspaces<'a>(
    snapshot: &'a ClientShellSnapshot,
    workspace: &'a ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> impl Iterator<Item = &'a ClientShellWorkspace> + 'a {
    let group_key = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
        .filter(|worktree| collapsed_groups.contains(&worktree.key))
        .map(|worktree| worktree.key.as_str());
    snapshot
        .workspaces
        .iter()
        .filter(move |candidate| match group_key {
            Some(key) => candidate.worktree.as_ref().is_some_and(|member| {
                member.key == key
                    && (member.is_linked_worktree
                        || candidate.workspace_id == workspace.workspace_id)
            }),
            None => candidate.workspace_id == workspace.workspace_id,
        })
}

pub(in crate::client::shell) fn workspace_rows(
    workspace: &ClientShellWorkspace,
    status: crate::api::schema::AgentStatus,
    tab_jobs: (usize, usize),
    indented: bool,
    config: &SpacesSidebarConfig,
) -> Vec<Vec<crate::ui::ResolvedToken>> {
    let label = if indented && !workspace.custom_label {
        workspace
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("worktree/").or(Some(branch)))
            .unwrap_or(&workspace.label)
    } else {
        &workspace.label
    };
    let token_values = workspace.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let rows = crate::ui::sidebar_space_rows(
        config,
        crate::ui::SpaceTokenContext {
            workspace: label,
            branch: workspace.branch.as_deref(),
            state_text: status_text(status),
            ahead_behind: workspace.git_ahead_behind,
            tab_jobs,
            tokens: &token_values,
            suppress_git_details: indented,
        },
    );
    if !config.tabs {
        return rows;
    }
    // Tabs listed under the space show their own states, so the space's
    // aggregate icon is redundant. Dropped for every space, also a collapsed
    // group that lists no tabs, so names stay aligned.
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .filter(|token| !matches!(token.kind, crate::ui::ResolvedTokenKind::StateIcon))
                .collect::<Vec<_>>()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

pub(in crate::client::shell) fn render_workspace_rows(
    buffer: &mut Buffer,
    area: Rect,
    status: crate::api::schema::AgentStatus,
    // `aggregate_icon` of `status`.
    icon: &'static str,
    entry: &WorkspaceEntry,
    rows: Vec<Vec<crate::ui::ResolvedToken>>,
    focused: bool,
    selected: bool,
    navigating: bool,
    // Name colour of a pressed or dragged space and, while dragged, its
    // background, which wins over selected and focused.
    grabbed: Option<(ratatui::style::Color, Option<ratatui::style::Color>)>,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    // With vertical tabs (`spaces.tabs`): leave two columns in front of the
    // name for `render_space_disclosure`, and give a focused space no
    // background; only its tab lines have one.
    let vertical_tabs = config.spaces.tabs;
    for (row_index, row) in rows.iter().enumerate() {
        let y = area.y + row_index as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x;
        if entry.indented {
            let prefix = if row_index == 0 {
                if entry.last_child {
                    "   └─ "
                } else {
                    "   ├─ "
                }
            } else if entry.last_child {
                "        "
            } else {
                "   │    "
            };
            x = put_segment(
                buffer,
                x,
                y,
                area.right(),
                prefix,
                Style::default().fg(palette.overlay0),
            );
        } else if row_index == 0 {
            x = x.saturating_add(1);
        } else {
            x = x.saturating_add(3);
        }
        if vertical_tabs && row_index == 0 {
            x = x.saturating_add(2);
        }
        let highlighted = focused || grabbed.is_some();
        let grabbed = grabbed.map(|(name, _)| name);
        let workspace_style = Style::default()
            .fg(grabbed.unwrap_or(if highlighted {
                palette.text
            } else {
                palette.subtext0
            }))
            .add_modifier(if highlighted {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let secondary_style = Style::default().fg(if focused {
            palette.mauve
        } else {
            palette.overlay0
        });
        let spans = crate::ui::resolved_token_spans(
            row,
            (icon, Style::default().fg(status_color(status, palette))),
            Style::default().fg(status_color(status, palette)),
            workspace_style,
            secondary_style,
            Style::default().fg(palette.overlay1),
            palette,
            area.right().saturating_sub(2).saturating_sub(x) as usize,
        );
        Paragraph::new(Line::from(spans)).render(
            Rect::new(x, y, area.right().saturating_sub(2).saturating_sub(x), 1),
            buffer,
        );
    }

    let drag_background = grabbed.and_then(|(_, background)| background);
    let background = if drag_background.is_some() {
        drag_background
    } else if selected {
        Some(workspace_selection_background(palette))
    } else if focused && !vertical_tabs {
        Some(workspace_active_background(palette, navigating))
    } else {
        None
    };
    if let Some(background) = background {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].set_bg(background);
            }
        }
    }
}
