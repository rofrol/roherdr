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

/// The build row's tooltip: the whole commit line, and the client's build
/// when it differs from the server's.
pub(in crate::client::shell) fn build_row_tooltip(
    area: Rect,
    row: BuildRow,
) -> super::tooltip::TooltipTarget {
    let text = match row.differing_client {
        Some(client) => format!("server {} · client {client}", row.commit),
        None => row.commit.to_owned(),
    };
    super::tooltip::TooltipTarget {
        rect: Rect::new(
            area.x,
            area.y,
            area.width.saturating_sub(BUILD_ROW_RIGHT_RESERVE),
            area.height.min(1),
        ),
        id: "build".to_owned(),
        text,
        bg: None,
        starts_at_target: false,
    }
}

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
    if let Some(held) = state.held_space_order {
        entries = super::space_sort::held_entries(snapshot, entries, held);
    }
    hits.space_order = super::space_sort::root_ids(snapshot, &entries);
    // The filter bar narrows the list; the order held above stays whole.
    let total_spaces = entries.len();
    if let Some(view) = state
        .space_filter
        .as_ref()
        .and_then(|filter| filter.view.as_ref())
    {
        entries = view.filter_entries(snapshot, entries);
    }
    let shown_spaces = entries.len();
    // While a space is dragged the list shows where it would land; the
    // header keeps its sort buttons. With the pointer outside the list the
    // order stays, the block stays lifted and the header says a release
    // cancels.
    let drag = state
        .dragged_workspace_id
        .and_then(|source| match state.workspace_drop_before {
            Some(before) => {
                let preview = entries_with_drag(snapshot, &entries, source, before)?;
                Some((Some(preview), None))
            }
            None => Some((None, Some("release cancels · Esc"))),
        });
    let tab_drag_hint = state.tab_line_drag.map(|(tab_id, insert_index)| {
        super::space_tabs::tab_drag_hint(snapshot, tab_id, insert_index)
    });
    let header_hint = drag
        .as_ref()
        .and_then(|(_, hint)| *hint)
        .or(tab_drag_hint.as_deref())
        .or(state
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
        // While the filter is open its bar takes this row (drawn below, once
        // the match count is known); the sort button and indicators wait.
        None if state.space_filter.is_some() => {}
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
            // The indicators sit right of the sort buttons, right to left: the
            // history button, then the agents asking, then the agents working;
            // one that does not fit is left out.
            let mut limit = hits
                .space_sort_buttons
                .iter()
                .map(|(rect, _)| rect.right())
                .max()
                .unwrap_or(workspace_area.x);
            // The filter button follows the sort button: a magnifier, no key
            // hint (the key only works with the sidebar focused).
            if config.mouse_capture && limit + 3 <= workspace_area.right() {
                put_text(
                    buffer,
                    limit + 1,
                    workspace_area.y,
                    1,
                    "⌕",
                    Style::default().fg(palette.overlay1),
                );
                hits.space_filter_button = Rect::new(limit, workspace_area.y, 3, 1);
                limit += 2;
            }
            let mut right = workspace_area.right();
            if let Some(unread) = state
                .notification_log_button
                .filter(|_| config.mouse_capture)
            {
                hits.notification_log_button = render_notification_log_button(
                    buffer,
                    workspace_area,
                    unread,
                    state.open_list == Some(super::notification_log::NotificationLogView::History),
                    palette,
                );
                right = hits.notification_log_button.x;
            }
            if config.mouse_capture {
                let (working, asking) = state.agent_counts;
                // The colours of the tab lines: the question mark's, the
                // working yellow; the star is neutral (mauve means waiting on
                // a job, yellow means work).
                let asking_style = Style::default()
                    .fg(super::agent_color(
                        crate::api::schema::AgentStatus::Done,
                        super::AgentMark::AwaitsReply,
                        palette,
                    ))
                    .add_modifier(Modifier::BOLD);
                let working_style = Style::default().fg(super::status_color(
                    crate::api::schema::AgentStatus::Working,
                    palette,
                ));
                let bookmark_style = Style::default().fg(palette.subtext0);
                use super::notification_log::NotificationLogView as View;
                for (count, glyph, style, slot, view) in [
                    (
                        asking,
                        "?",
                        asking_style,
                        &mut hits.asking_list_button,
                        View::Asking,
                    ),
                    (
                        working,
                        crate::ui::motion::working_glyph(),
                        working_style,
                        &mut hits.working_list_button,
                        View::Working,
                    ),
                    (
                        state.bookmark_count,
                        "★",
                        bookmark_style,
                        &mut hits.bookmarks_list_button,
                        View::Bookmarks,
                    ),
                ] {
                    if count == 0 {
                        continue;
                    }
                    let label = format!("{glyph}{}", count.min(99));
                    let width = display_width(&label);
                    if right < limit + width + 2 {
                        continue;
                    }
                    let x = right - width - 1;
                    // An open list's button is a filled pill, one cell wider on
                    // each side than its label.
                    let style = if state.open_list == Some(view) {
                        let pill = Rect::new(x.saturating_sub(1), workspace_area.y, width + 2, 1);
                        open_button_style(buffer, pill, style, palette)
                    } else {
                        style
                    };
                    put_text(buffer, x, workspace_area.y, width, &label, style);
                    // The pill, so the list opens under its left edge.
                    *slot = Rect::new(x.saturating_sub(1), workspace_area.y, width + 2, 1);
                    right = x.saturating_sub(1);
                }
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
    // The filter bar takes the header row.
    let header_rows = WORKSPACE_HEADER_ROWS;
    if let Some(filter) = state.space_filter.as_ref() {
        let bar = Rect::new(workspace_area.x, workspace_area.y, workspace_area.width, 1)
            .intersection(workspace_area);
        let (bar, close) = super::space_filter::render_filter_bar(
            buffer,
            bar,
            filter.query,
            filter.focused,
            filter.view.as_ref().map(|_| (shown_spaces, total_spaces)),
            palette,
        );
        hits.space_filter_bar = bar;
        hits.space_filter_close = close;
    }
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(header_rows),
        workspace_area.width,
        workspace_area.height.saturating_sub(header_rows + 1),
    );
    hits.workspace_body = body;
    if entries.is_empty()
        && state
            .space_filter
            .as_ref()
            .is_some_and(|f| f.view.is_some())
    {
        put_text(
            buffer,
            body.x.saturating_add(1),
            body.y,
            body.width.saturating_sub(1),
            "no match",
            Style::default().fg(palette.overlay0),
        );
    }
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
                        let tab_lines = super::space_tabs::space_tab_lines_filtered(
                            snapshot,
                            workspace,
                            state.collapsed_groups,
                            state.unfolded_squares,
                            state.held_squares,
                            state.kept_jobs,
                            state
                                .space_filter
                                .as_ref()
                                .and_then(|filter| filter.view.as_ref()),
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
                        let tab_rows =
                            tab_lines
                                .iter()
                                .map(|line| {
                                    usize::from(line.height(squares_width.saturating_sub(
                                        super::space_tabs::tab_indent(entry.indented),
                                    )))
                                })
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
    // The list scrolls by rows, so a space taller than the list (a tab with
    // many job squares) can be scrolled through; `workspace_scroll` is the
    // first content row shown.
    let tops = entries
        .iter()
        .enumerate()
        .scan(0usize, |top, (index, _)| {
            let this = *top;
            *top += usize::from(row_heights[index]) + usize::from(gaps[index]);
            Some(this)
        })
        .collect::<Vec<_>>();
    let content_rows = tops
        .last()
        .zip(row_heights.last())
        .map_or(0, |(top, height)| top + usize::from(*height));
    let viewport = usize::from(body.height);
    let max_scroll = content_rows.saturating_sub(viewport);
    // Keep the first row shown on the same space when rows above it come or
    // go (squares folding or closing), unless the list was scrolled since.
    if let Some(anchor) = state
        .workspace_scroll_anchor
        .as_ref()
        .filter(|anchor| anchor.scroll == *state.workspace_scroll)
    {
        if let Some(index) = entries.iter().position(|entry| {
            snapshot
                .workspaces
                .get(entry.index)
                .is_some_and(|workspace| workspace.workspace_id == anchor.workspace_id)
        }) {
            let inner = anchor
                .row
                .min(usize::from(row_heights[index]).saturating_sub(1));
            *state.workspace_scroll = tops[index] + inner;
        }
    }
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = entries
            .iter()
            .position(|entry| snapshot.workspaces[entry.index].focused)
        {
            // The name row and the focused tab line (or its open square);
            // when both do not fit, the deeper one.
            let top = tops[target];
            let depth = snapshot
                .workspaces
                .get(entries[target].index)
                .map_or(0, |workspace| {
                    focus_depth(
                        snapshot,
                        workspace,
                        &entries[target],
                        state,
                        squares_width,
                        config,
                    )
                });
            let bottom = top + usize::from(depth);
            let top = if bottom - top >= viewport {
                bottom
            } else {
                top
            };
            *state.workspace_scroll =
                super::scroll::rows_start_to_reveal(*state.workspace_scroll, viewport, top, bottom);
        }
    }
    // A tab line just unfolded: show it with its squares and the empty row
    // after them, moving as little as possible; a block taller than the list
    // puts the line at the top.
    if let Some(tab_id) = state
        .reveal_unfolded_tab
        .take()
        .filter(|_| !body.is_empty())
    {
        let found = snapshot
            .tabs
            .iter()
            .find(|tab| tab.tab_id == tab_id)
            .and_then(|tab| {
                entries.iter().position(|entry| {
                    snapshot.workspaces[entry.index].workspace_id == tab.workspace_id
                })
            })
            .and_then(|target| {
                let workspace = snapshot.workspaces.get(entries[target].index)?;
                let (offset, height) = tab_line_extent(
                    snapshot,
                    workspace,
                    &entries[target],
                    state,
                    squares_width,
                    config,
                    &tab_id,
                )?;
                Some((tops[target] + offset, usize::from(height)))
            });
        if let Some((top, height)) = found {
            *state.workspace_scroll = if height >= viewport {
                top
            } else {
                super::scroll::rows_start_to_reveal(
                    *state.workspace_scroll,
                    viewport,
                    top,
                    top + height - 1,
                )
            };
        }
    }
    *state.workspace_scroll = (*state.workspace_scroll).min(max_scroll);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll - *state.workspace_scroll,
        max_offset_from_bottom: max_scroll,
        viewport_rows: viewport.min(content_rows),
    };
    hits.workspace_max_scroll = max_scroll;
    hits.workspace_scroll_metrics = Some(metrics);
    let scroll = *state.workspace_scroll;
    *state.workspace_scroll_anchor = tops
        .iter()
        .zip(&row_heights)
        .zip(&entries)
        .find(|((top, height), _)| **top + usize::from(**height) > scroll)
        .and_then(|((top, _), entry)| {
            Some(ScrollAnchor {
                workspace_id: snapshot.workspaces.get(entry.index)?.workspace_id.clone(),
                row: scroll.saturating_sub(*top),
                scroll,
            })
        })
        .filter(|_| scroll > 0);
    let show_scrollbar = max_scroll > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    // Where every space is, in screen rows, drawn or not: space drag and drop
    // and revealing a space work from it.
    let screen_row = |row: usize| i32::from(body.y) + row as i32 - scroll as i32;
    hits.workspace_layout = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let workspace = snapshot.workspaces.get(entry.index)?;
            Some(WorkspaceLayout {
                workspace_id: workspace.workspace_id.clone(),
                indented: entry.indented,
                top: screen_row(tops[index]),
                bottom: screen_row(tops[index] + usize::from(row_heights[index])),
            })
        })
        .collect();
    let mut scratch = None::<Buffer>;
    for (entry_position, entry) in entries.iter().enumerate() {
        let top = tops[entry_position];
        let row_height = row_heights[entry_position];
        let block_end = top + usize::from(row_height);
        if block_end <= scroll || row_height == 0 {
            continue;
        }
        if top >= scroll + viewport {
            break;
        }
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let status = displayed_workspace_status(snapshot, workspace, state.collapsed_groups);
        let tab_lines = super::space_tabs::space_tab_lines_filtered(
            snapshot,
            workspace,
            state.collapsed_groups,
            state.unfolded_squares,
            state.held_squares,
            state.kept_jobs,
            state
                .space_filter
                .as_ref()
                .and_then(|filter| filter.view.as_ref()),
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
        // Rows of the block above the list's top, and where it starts.
        let cut = scroll.saturating_sub(top);
        let y = body.y + (top + cut - scroll) as u16;
        let shown = (usize::from(row_height) - cut).min(usize::from(body.bottom() - y)) as u16;
        let visible = Rect::new(body.x, y, content_width, shown);
        // A block cut at either edge is drawn whole off screen, then its
        // visible rows are copied.
        let partial = cut > 0 || shown < row_height;
        let mut block_hits = ShellHitMap::default();
        let target: &mut Buffer = if partial {
            // Full body width: a tab fill continues under the scrollbar.
            let area = Rect::new(body.x, 0, body.width, row_height);
            let scratch = scratch.get_or_insert_with(|| Buffer::empty(area));
            scratch.resize(area);
            scratch.reset();
            super::render::render_sidebar_background(scratch, area, palette);
            scratch
        } else {
            &mut *buffer
        };
        let rect = if partial {
            Rect::new(body.x, 0, content_width, row_height)
        } else {
            visible
        };
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
            target.set_style(rect, Style::default().bg(bg));
        } else if selected && state.space_filter.is_none() {
            target.set_style(rect, Style::default().bg(palette.selection_bg));
        } else if workspace.focused && !config.spaces.tabs {
            // With vertical tabs only the tab lines have a background.
            target.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        let icon = aggregate_icon(snapshot, status, config.status_indicators, |agent| {
            displayed_workspaces(snapshot, workspace, state.collapsed_groups)
                .any(|shown| shown.workspace_id == agent.workspace_id)
        });
        let push_status = render_workspace_rows(
            target,
            rect,
            status,
            icon,
            entry,
            rows,
            workspace.focused,
            selected && state.space_filter.is_none(),
            state.selected_workspace_id.is_some(),
            grab_color
                .filter(|_| dragged || pressed)
                .map(|name| (name, drag_bg)),
            true,
            config,
        );
        if let Some(chip) = push_status.filter(|_| config.mouse_capture) {
            // Lit under the pointer, like the buttons after it: that it
            // opens the branch menu is shown, not told.
            if state.hovered_name_button
                == Some((workspace.workspace_id.as_str(), NameLineButton::PushStatus))
            {
                target.set_style(chip, Style::default().bg(palette.surface1));
            }
            block_hits
                .space_push_status
                .push((chip, workspace.workspace_id.clone()));
        }
        if let Some(color) = grab_color {
            // A grip at the name line's right edge, left of the group
            // chevron (with vertical tabs, the last column, right of the
            // new-tab `+`), in a spacer column the name never reaches.
            let hovered = state.hovered_workspace_id == Some(workspace.workspace_id.as_str());
            if (dragged || pressed || hovered) && rect.width >= 6 {
                put_text(
                    target,
                    rect.right()
                        .saturating_sub(if config.spaces.tabs { 1 } else { 2 }),
                    rect.y,
                    1,
                    "⋮",
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                );
            }
        }
        if launch_button_shown(config, rect.width) {
            // The agent last launched here, left of the `+`: a click starts
            // it in a new tab, a right click picks another.
            let kind = super::agent_launch::space_launch_agent(
                snapshot,
                state.launched_agents,
                &workspace.workspace_id,
            );
            // Next to the `+`: the two ways to open a tab sit together.
            let button = Rect::new(rect.right().saturating_sub(7), rect.y, 3, 1);
            // The chip starts an agent, unlike the plain `+`; its colour is
            // the agent's, which the tooltip names.
            let hovered = state.hovered_name_button
                == Some((workspace.workspace_id.as_str(), NameLineButton::Launch));
            let (fg, tooltip) = match kind.as_deref() {
                Some(kind) => (
                    super::agent_launch::agent_badge_color(kind, palette),
                    format!("Start {kind} in a new tab · right click: another agent"),
                ),
                None => (palette.overlay1, "Start an agent in a new tab".to_owned()),
            };
            // No background at rest; hovered, the chip's three columns, all
            // of them clickable, show as one.
            let style = Style::default().fg(fg).add_modifier(Modifier::BOLD);
            let style = if hovered {
                style.bg(palette.surface1)
            } else {
                style
            };
            put_text(
                target,
                button.x,
                button.y,
                button.width,
                LAUNCH_LABEL,
                style,
            );
            block_hits
                .space_launch_agent
                .push((button, workspace.workspace_id.clone()));
            block_hits.tooltips.push(super::tooltip::TooltipTarget {
                rect: button,
                id: format!("launch-agent:{}", workspace.workspace_id),
                text: tooltip,
                bg: None,
                starts_at_target: false,
            });
        }
        if config.spaces.tabs && config.mouse_capture && rect.width >= 6 {
            // A new tab in this space, whichever space is focused.
            // ` + `: like the launch chip, three columns to click, lit on
            // hover.
            let button = Rect::new(rect.right().saturating_sub(4), rect.y, 3, 1);
            let style = Style::default().fg(palette.overlay1);
            let style = if state.hovered_name_button
                == Some((workspace.workspace_id.as_str(), NameLineButton::NewTab))
            {
                style.bg(palette.surface1)
            } else {
                style
            };
            put_text(target, button.x, button.y, button.width, " + ", style);
            block_hits
                .space_new_tab
                .push((button, workspace.workspace_id.clone()));
        }
        let tab_hits = super::space_tabs::render_space_tab_lines(
            target,
            Rect::new(
                rect.x,
                rect.y.saturating_add(own_rows),
                rect.width,
                rect.height.saturating_sub(own_rows),
            ),
            &tab_lines,
            workspace.focused,
            squares_width.saturating_sub(super::space_tabs::tab_indent(entry.indented)),
            super::space_tabs::TabLinePointer {
                square: state.hovered_square,
                fold: state.hovered_fold,
            },
            u16::from(show_scrollbar),
            super::space_tabs::tab_indent(entry.indented),
            state.tab_line_drag,
            state
                .space_filter
                .as_ref()
                .filter(|filter| filter.view.is_some())
                .map(|filter| filter.query),
            config,
        );
        render_worktree_trunk(
            target,
            rect,
            entry,
            entries
                .get(entry_position + 1)
                .is_some_and(|next| next.indented),
            palette,
        );
        // The filter's cursor is a bar down the whole block, not a fill.
        if selected && state.space_filter.is_some() {
            for row in rect.y..rect.bottom() {
                put_text(
                    target,
                    rect.x,
                    row,
                    1,
                    "▍",
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
        block_hits.space_tabs.extend(tab_hits.lines);
        block_hits.space_tab_folds.extend(tab_hits.folds);
        block_hits.space_tab_squares.extend(tab_hits.squares);
        block_hits.space_tab_gone.extend(tab_hits.gone);
        block_hits.tooltips.extend(tab_hits.tooltips);
        block_hits.space_tab_square_order.extend(tab_hits.order);
        let group_toggle = if config.spaces.tabs {
            super::space_tabs::render_space_disclosure(
                target,
                rect,
                snapshot,
                entry,
                workspace,
                state.collapsed_groups,
                config,
            )
        } else {
            render_parent_group_toggle(
                target,
                rect,
                snapshot,
                entry.index,
                state.collapsed_groups,
                palette,
            )
        };
        block_hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: workspace.workspace_id.clone(),
            indented: entry.indented,
            group_toggle,
        });
        if partial {
            if let Some(scratch) = scratch.as_ref() {
                for row in 0..shown {
                    for x in body.left()..body.right() {
                        buffer[(x, y + row)] = scratch[(x, cut as u16 + row)].clone();
                    }
                }
            }
            block_hits.shift_space_block(i32::from(y) - cut as i32, visible);
        }
        hits.merge_space_block(block_hits);
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
        hits.tooltips.push(build_row_tooltip(build_area, build));
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

/// The notification history button at the right end of the spaces header:
/// `✉` and the unread count, accent while there are unread ones.
fn render_notification_log_button(
    buffer: &mut Buffer,
    area: Rect,
    unread: usize,
    open: bool,
    palette: &Palette,
) -> Rect {
    let label = if unread > 0 {
        format!("✉{}", unread.min(99))
    } else {
        "✉".to_owned()
    };
    let width = display_width(&label).saturating_add(1);
    if area.width < width + 8 || area.height == 0 {
        return Rect::default();
    }
    let rect = Rect::new(area.right().saturating_sub(width + 1), area.y, width + 1, 1);
    let style = if unread > 0 {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.overlay1)
    };
    let style = if open {
        open_button_style(buffer, rect, style, palette)
    } else {
        style
    };
    put_text(
        buffer,
        rect.x.saturating_add(1),
        rect.y,
        width,
        &label,
        style,
    );
    rect
}

/// Fills `pill` with a light accent tint (a sixth of the accent on light
/// themes, a quarter on dark ones) and returns `style` made bold on it: a
/// header button whose list is open keeps its own colour. Without an RGB
/// palette the pill is solid accent with contrasting text.
fn open_button_style(buffer: &mut Buffer, pill: Rect, style: Style, palette: &Palette) -> Style {
    let dark = matches!(palette.panel_bg, ratatui::style::Color::Rgb(r, g, b)
        if 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) < 128_000);
    let open = match super::render::tabs::blend(
        palette.accent,
        palette.panel_bg,
        1,
        if dark { 4 } else { 6 },
    ) {
        Some(tint) => style.bg(tint).add_modifier(Modifier::BOLD),
        None => Style::default()
            .fg(super::panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    };
    buffer.set_style(pill.intersection(buffer.area), open);
    open
}

/// Column of the worktree tree's trunk, left of the tab lines' state icons.
const WORKTREE_TRUNK_COLUMN: u16 = 1;

/// Draws the worktree tree's trunk down a space's block below its name line:
/// through a parent space's rows and tab lines when worktree spaces follow
/// it, and through a worktree space that is not the last one. Without it the
/// first child's connector hangs under the parent's last tab line and reads
/// as that tab's child.
pub(in crate::client::shell) fn render_worktree_trunk(
    buffer: &mut Buffer,
    area: Rect,
    entry: &WorkspaceEntry,
    // The next entry is a worktree space of this group.
    children_follow: bool,
    palette: &Palette,
) {
    let continues = if entry.indented {
        !entry.last_child
    } else {
        children_follow
    };
    if !continues || area.width <= WORKTREE_TRUNK_COLUMN {
        return;
    }
    let x = area.x + WORKTREE_TRUNK_COLUMN;
    for y in area.y.saturating_add(1)..area.bottom() {
        put_text(buffer, x, y, 1, "│", Style::default().fg(palette.overlay0));
    }
}

/// Columns the name line of a space leaves at its right with vertical tabs:
/// the new-tab ` + ` (its padding is the gaps) and the drag grip.
const NAME_LINE_ACTIONS_WIDTH: u16 = 4;

/// The launch button: a bold `A` (agent) padded to three columns, all of
/// them the click target. ASCII, so it keeps its
/// width in every terminal.
const LAUNCH_LABEL: &str = " A ";

/// The launch chip, left of the `+`; its padding is the gaps around it.
const LAUNCH_BUTTON_WIDTH: u16 = 3;

/// Narrower space blocks keep their name rather than show the button.
const LAUNCH_BUTTON_MIN_WIDTH: u16 = 16;

/// The launch button shows on a space's name line next to its `+`.
pub(in crate::client::shell) fn launch_button_shown(
    config: &ClientShellConfig,
    width: u16,
) -> bool {
    config.spaces.tabs && config.mouse_capture && width >= LAUNCH_BUTTON_MIN_WIDTH
}

#[allow(clippy::too_many_arguments)] // one space's drawing inputs, from two sidebars
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
    // The name line ends in the launch button (the local machine's spaces).
    launch_button: bool,
    config: &ClientShellConfig,
) -> Option<Rect> {
    // Where the push status chip was drawn, for its click and tooltip.
    let mut push_status_rect = None;
    let palette = &config.palette;
    // With vertical tabs (`spaces.tabs`): leave two columns in front of the
    // name for `render_space_disclosure`, and give a focused space no
    // background; only its tab lines have one.
    let vertical_tabs = config.spaces.tabs;
    // Columns left free at the right: the grip's; with vertical tabs the
    // name line also ends in a new-tab `+`, a column apart from the grip.
    let launch_button = launch_button && launch_button_shown(config, area.width);
    let reserved = |row_index: usize| {
        if vertical_tabs && row_index == 0 {
            NAME_LINE_ACTIONS_WIDTH
                + if launch_button {
                    LAUNCH_BUTTON_WIDTH
                } else {
                    0
                }
        } else {
            2
        }
    };
    for (row_index, row) in rows.iter().enumerate() {
        let y = area.y + row_index as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x;
        if entry.indented {
            // The connector starts in the trunk's column (see
            // `render_worktree_trunk`), left of the parent's tab lines, so it
            // cannot read as a child of the tab above.
            let prefix = if row_index == 0 {
                if entry.last_child {
                    " └─── "
                } else {
                    " ├─── "
                }
            } else {
                "        "
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
            .fg(grabbed.unwrap_or(if highlighted || vertical_tabs {
                palette.text
            } else {
                palette.subtext0
            }))
            .add_modifier(if highlighted || vertical_tabs {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let secondary_style = Style::default().fg(if focused {
            palette.mauve
        } else {
            palette.overlay0
        });
        // The push status chip and the job summary (`↑3 ◐ 2 !2`) are
        // right-aligned, just left of the actions at the row's end; the name
        // is cut first, never the chip or the counts.
        let is_jobs = |token: &crate::ui::ResolvedToken| {
            matches!(token.kind, crate::ui::ResolvedTokenKind::TabJobs { .. })
        };
        let is_push = |token: &crate::ui::ResolvedToken| {
            matches!(token.kind, crate::ui::ResolvedTokenKind::PushStatus { .. })
        };
        let (push, rest): (Vec<_>, Vec<_>) = row.iter().cloned().partition(is_push);
        let (jobs, rest): (Vec<_>, Vec<_>) = rest.into_iter().partition(is_jobs);
        let right_edge = area.right().saturating_sub(reserved(row_index));
        let style_for = |tokens: &[crate::ui::ResolvedToken], width: usize| {
            crate::ui::resolved_token_spans(
                tokens,
                (icon, Style::default().fg(status_color(status, palette))),
                Style::default().fg(status_color(status, palette)),
                workspace_style,
                secondary_style,
                Style::default().fg(palette.overlay1),
                palette,
                width,
            )
        };
        let jobs_spans = style_for(&jobs, usize::from(right_edge.saturating_sub(x)));
        let jobs_width = jobs_spans
            .iter()
            .map(|span| display_width(&span.content))
            .sum::<u16>();
        let jobs_x = if jobs_width > 0 {
            right_edge.saturating_sub(jobs_width).max(x)
        } else {
            right_edge
        };
        // A column after the chip pads it like the name line's buttons; the
        // gap before it pads its left.
        let push_right = if jobs_width > 0 {
            jobs_x.saturating_sub(1).max(x)
        } else {
            right_edge
        }
        .saturating_sub(u16::from(!push.is_empty()));
        let push_spans = style_for(&push, usize::from(push_right.saturating_sub(x)));
        let push_width = push_spans
            .iter()
            .map(|span| display_width(&span.content))
            .sum::<u16>();
        let push_x = push_right.saturating_sub(push_width).max(x);
        if push_width > 0 {
            Paragraph::new(Line::from(push_spans)).render(
                Rect::new(push_x, y, push_right.saturating_sub(push_x), 1),
                buffer,
            );
            let pad_x = push_x.saturating_sub(1).max(area.x);
            push_status_rect = Some(Rect::new(
                pad_x,
                y,
                push_right.saturating_add(1).saturating_sub(pad_x),
                1,
            ));
        }
        let name_end = if push_width > 0 {
            push_x.saturating_sub(1).max(x)
        } else if jobs_width > 0 {
            jobs_x.saturating_sub(1).max(x)
        } else {
            right_edge
        };
        let spans = style_for(&rest, usize::from(name_end.saturating_sub(x)));
        Paragraph::new(Line::from(spans))
            .render(Rect::new(x, y, name_end.saturating_sub(x), 1), buffer);
        if jobs_width > 0 {
            Paragraph::new(Line::from(jobs_spans)).render(
                Rect::new(jobs_x, y, right_edge.saturating_sub(jobs_x), 1),
                buffer,
            );
        }
    }

    // With vertical tabs a space starts where its name row has a bar in the
    // first column: the accent for the focused space, a muted tone for the
    // others. The name is bold. No fill, so it cannot be mistaken for a tab
    // line (the tab lines are the filled ones). A nested worktree space has
    // its tree connector instead.
    if vertical_tabs && grabbed.is_none() && !entry.indented && area.height > 0 {
        let color = if focused {
            palette.accent
        } else {
            super::tabs::blend(palette.text, palette.panel_bg, 1, 2).unwrap_or(palette.overlay1)
        };
        put_text(buffer, area.x, area.y, 1, "▍", Style::default().fg(color));
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
    push_status_rect
}

/// Where a tab line is inside its space: the rows above it (the space's own
/// rows and the lines before it) and its height with squares.
fn tab_line_extent(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    entry: &WorkspaceEntry,
    state: &ShellRenderState<'_>,
    squares_width: u16,
    config: &ClientShellConfig,
    tab_id: &str,
) -> Option<(usize, u16)> {
    let tab_lines = super::space_tabs::space_tab_lines_filtered(
        snapshot,
        workspace,
        state.collapsed_groups,
        state.unfolded_squares,
        state.held_squares,
        state.kept_jobs,
        state
            .space_filter
            .as_ref()
            .and_then(|filter| filter.view.as_ref()),
        config,
    );
    let own_rows = workspace_rows(
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
    let squares_width = squares_width.saturating_sub(super::space_tabs::tab_indent(entry.indented));
    let mut offset = own_rows;
    for line in &tab_lines {
        let height = line.height(squares_width);
        if line.tab_id == tab_id {
            return Some((offset, height));
        }
        offset += usize::from(height);
    }
    None
}

/// Rows from a space's name row down to its focused tab line, or to the
/// open job's square under it.
fn focus_depth(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    entry: &WorkspaceEntry,
    state: &ShellRenderState<'_>,
    squares_width: u16,
    config: &ClientShellConfig,
) -> u16 {
    let tab_lines = super::space_tabs::space_tab_lines_filtered(
        snapshot,
        workspace,
        state.collapsed_groups,
        state.unfolded_squares,
        state.held_squares,
        state.kept_jobs,
        state
            .space_filter
            .as_ref()
            .and_then(|filter| filter.view.as_ref()),
        config,
    );
    let own_rows = workspace_rows(
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
    let squares_width = squares_width.saturating_sub(super::space_tabs::tab_indent(entry.indented));
    let mut depth = own_rows;
    for line in &tab_lines {
        if line.active {
            depth += line
                .focused_square_row(squares_width)
                .map_or(0, |row| row + 1);
            return depth.min(usize::from(u16::MAX)) as u16;
        }
        depth += usize::from(line.height(squares_width));
    }
    0
}
