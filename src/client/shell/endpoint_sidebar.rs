use super::render::{display_width, put_right_text, put_text, ShellRenderState};
use super::*;

fn collapsed_groups_for_endpoint<'a>(
    state: &'a ShellRenderState<'_>,
    endpoint_id: &ClientEndpointId,
) -> Option<&'a HashSet<String>> {
    if endpoint_id.is_local() {
        Some(state.collapsed_groups)
    } else {
        state.remote_collapsed_groups.get(endpoint_id)
    }
}

/// The unfolded tabs and held square order for `endpoint_id`'s tab lines:
/// the active machine's, none for the others, whose squares stay folded.
fn squares_state<'a>(
    state: &'a ShellRenderState<'_>,
    endpoint_id: &ClientEndpointId,
) -> (
    &'a HashSet<String>,
    &'a super::space_tabs::HeldSquares,
    &'a super::space_tabs::KeptJobs,
) {
    static NONE: std::sync::LazyLock<(
        HashSet<String>,
        super::space_tabs::HeldSquares,
        super::space_tabs::KeptJobs,
    )> = std::sync::LazyLock::new(Default::default);
    if endpoint_id == state.active_endpoint_id {
        (state.unfolded_squares, state.held_squares, state.kept_jobs)
    } else {
        (&NONE.0, &NONE.1, &NONE.2)
    }
}

pub(super) fn render_collapsed(
    buffer: &mut Buffer,
    area: Rect,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    super::render::render_sidebar_background(buffer, area, palette);
    let (workspace_area, divider_y, detail_area) = super::sidebar::collapsed_sidebar_sections(area);
    let mut total_rows = 0usize;
    let mut selected_row = None;
    let reveal = std::mem::take(state.reveal_navigation_workspace);
    for endpoint in state.endpoints {
        total_rows += 1;
        if state.collapsed_endpoints.contains(&endpoint.endpoint_id) {
            continue;
        }
        if let Some(snapshot) = endpoint.snapshot.as_deref() {
            if reveal {
                if let Some(target) = state
                    .selected_workspace_id
                    .filter(|target| target.endpoint_id == endpoint.endpoint_id)
                {
                    selected_row = snapshot
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.workspace_id == target.workspace_id)
                        .map(|index| total_rows + index);
                }
            }
            total_rows += snapshot.workspaces.len();
        }
    }
    let height = usize::from(workspace_area.height);
    let max_scroll = total_rows.saturating_sub(height);
    *state.workspace_scroll = (*state.workspace_scroll).min(max_scroll);
    if let Some(row) = selected_row {
        if row < *state.workspace_scroll {
            *state.workspace_scroll = row;
        } else if row >= state.workspace_scroll.saturating_add(height) {
            *state.workspace_scroll = row.saturating_add(1).saturating_sub(height).min(max_scroll);
        }
    }
    hits.workspace_max_scroll = max_scroll;
    let mut skip = *state.workspace_scroll;
    let mut y = workspace_area.y;
    for (index, endpoint) in state.endpoints.iter().enumerate() {
        if y >= workspace_area.bottom() {
            break;
        }
        let rect = Rect::new(workspace_area.x, y, workspace_area.width, 1);
        let active = &endpoint.endpoint_id == state.active_endpoint_id;
        let collapsed = state.collapsed_endpoints.contains(&endpoint.endpoint_id);
        if skip > 0 {
            skip -= 1;
        } else {
            if active && collapsed {
                buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
            }
            let label = if endpoint.endpoint_id.is_local() {
                "L".to_owned()
            } else {
                (index + 1).to_string()
            };
            let marker = if collapsed { "▸" } else { "▾" };
            put_text(
                buffer,
                rect.x,
                rect.y,
                rect.width.saturating_sub(1),
                &format!("{marker}{label}"),
                Style::default().fg(if endpoint.status == ClientEndpointStatus::Online {
                    palette.text
                } else {
                    palette.overlay0
                }),
            );
            let mut status_badge = Rect::default();
            if !endpoint.endpoint_id.is_local() {
                let (glyph, _, color) = endpoint_status_presentation(endpoint.status, palette);
                let width = display_width(glyph).min(rect.width);
                status_badge = Rect::new(rect.right().saturating_sub(width), rect.y, width, 1);
                put_right_text(
                    buffer,
                    rect,
                    rect.y,
                    glyph,
                    state.machine_diagnostics.badge_style(
                        endpoint,
                        palette,
                        Style::default().fg(color),
                    ),
                );
            }
            hits.machines.push(MachineHit {
                rect,
                status_badge,
                collapse_toggle: Rect::new(rect.x, rect.y, u16::from(rect.width > 1), 1),
                endpoint_id: endpoint.endpoint_id.clone(),
            });
            y = y.saturating_add(1);
        }
        if collapsed {
            continue;
        }
        let Some(snapshot) = endpoint.snapshot.as_deref() else {
            continue;
        };
        for workspace in &snapshot.workspaces {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if y >= workspace_area.bottom() {
                break;
            }
            let rect = Rect::new(workspace_area.x, y, workspace_area.width, 1);
            let focused = active && workspace.focused;
            let selected = state.selected_workspace_id.is_some_and(|target| {
                target.matches(&endpoint.endpoint_id, &workspace.workspace_id)
            });
            let selection_background = if palette.selection_bg == ratatui::style::Color::Reset {
                palette.active_row_bg
            } else {
                palette.selection_bg
            };
            if selected {
                buffer.set_style(rect, Style::default().bg(selection_background));
            } else if focused {
                buffer.set_style(
                    rect,
                    Style::default().bg(super::sidebar::workspace_active_background(
                        palette,
                        state.selected_workspace_id.is_some(),
                    )),
                );
            }
            let stale = endpoint.status != ClientEndpointStatus::Online;
            let number = format!(" {}", workspace.number);
            let number_width = super::render::display_width(&number).min(rect.width);
            let dim = if stale {
                Modifier::DIM
            } else {
                Modifier::empty()
            };
            put_text(
                buffer,
                rect.x,
                rect.y,
                number_width,
                &number,
                Style::default()
                    .fg(if focused && !stale {
                        palette.text
                    } else {
                        palette.overlay0
                    })
                    .add_modifier(dim),
            );
            put_text(
                buffer,
                rect.x.saturating_add(number_width),
                rect.y,
                rect.width.saturating_sub(number_width),
                aggregate_icon(
                    snapshot,
                    workspace.agent_status,
                    config.status_indicators,
                    |agent| agent.workspace_id == workspace.workspace_id,
                ),
                Style::default()
                    .fg(if stale {
                        palette.overlay0
                    } else {
                        status_color(workspace.agent_status, palette)
                    })
                    .add_modifier(dim),
            );
            hits.workspaces.push(WorkspaceHit {
                rect,
                endpoint_id: endpoint.endpoint_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                indented: false,
                group_toggle: None,
            });
            y = y.saturating_add(1);
        }
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
    super::endpoint_agents::render_collapsed(
        buffer,
        detail_area,
        state.endpoints,
        state.active_endpoint_id,
        config,
        hits,
    );
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
        Style::default().fg(palette.overlay0),
    );
}

pub(super) fn render_expanded(
    buffer: &mut Buffer,
    area: Rect,
    active_snapshot: Option<&ClientShellSnapshot>,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    super::render::render_sidebar_background(buffer, area, palette);
    hits.sidebar_divider = if area.is_empty() {
        Rect::default()
    } else {
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height)
    };
    let build = super::sidebar::build_row(
        active_snapshot.and_then(|snapshot| snapshot.build_commit.as_deref()),
        crate::build_info::commit_line(),
    );
    let (sections, build_area) = super::sidebar::split_build_row(area, build);
    hits.sidebar_sections = sections;
    let (workspace_area, detail_area) =
        crate::ui::expanded_sidebar_sections(sections, state.sidebar_section_split);
    hits.sidebar_section_divider =
        crate::ui::sidebar_section_divider_rect(sections, state.sidebar_section_split);
    put_text(
        buffer,
        workspace_area.x,
        workspace_area.y,
        workspace_area.width,
        " machines",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );

    let empty_collapsed_groups = HashSet::new();

    enum Row {
        Endpoint(usize),
        Workspace {
            endpoint: usize,
            entry: WorkspaceEntry,
        },
    }
    let mut rows = Vec::new();
    for (endpoint_index, endpoint) in state.endpoints.iter().enumerate() {
        rows.push(Row::Endpoint(endpoint_index));
        if state.collapsed_endpoints.contains(&endpoint.endpoint_id) {
            continue;
        }
        if let Some(snapshot) = endpoint.snapshot.as_deref() {
            let collapsed_groups = collapsed_groups_for_endpoint(state, &endpoint.endpoint_id)
                .unwrap_or(&empty_collapsed_groups);
            rows.extend(
                super::sidebar::workspace_entries(snapshot, collapsed_groups)
                    .into_iter()
                    .map(|entry| Row::Workspace {
                        endpoint: endpoint_index,
                        entry,
                    }),
            );
        }
    }
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    hits.workspace_body = body;
    // Tab lines are indented two columns under their machine, and the
    // scrollbar column is always left free, so heights and drawing agree.
    let squares_width = body.width.saturating_sub(3);
    let row_heights = rows
        .iter()
        .map(|row| match row {
            Row::Endpoint(_) => 1,
            Row::Workspace { endpoint, entry } => {
                let endpoint = &state.endpoints[*endpoint];
                let collapsed_groups = collapsed_groups_for_endpoint(state, &endpoint.endpoint_id)
                    .unwrap_or(&empty_collapsed_groups);
                endpoint
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| {
                        let workspace = snapshot.workspaces.get(entry.index)?;
                        let (unfolded, held, kept) = squares_state(state, &endpoint.endpoint_id);
                        let tab_lines = super::space_tabs::space_tab_lines(
                            snapshot,
                            workspace,
                            collapsed_groups,
                            unfolded,
                            held,
                            kept,
                            config,
                        );
                        let rows = super::sidebar::workspace_rows(
                            workspace,
                            super::sidebar::displayed_workspace_status(
                                snapshot,
                                workspace,
                                collapsed_groups,
                            ),
                            super::space_tabs::space_row_tab_jobs(
                                snapshot,
                                workspace,
                                collapsed_groups,
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
                        Some((rows + tab_rows).min(u16::MAX as usize) as u16)
                    })
                    .unwrap_or(1)
            }
        })
        .collect::<Vec<_>>();
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, row)| match (row, rows.get(index + 1)) {
            (
                Row::Workspace { endpoint, .. },
                Some(Row::Workspace {
                    endpoint: next_endpoint,
                    entry,
                }),
            ) if endpoint == next_endpoint => u16::from(!entry.indented) * config.spaces.row_gap,
            _ => 0,
        })
        .collect::<Vec<_>>();
    // The list scrolls by rows, as the local one: `workspace_scroll` is the
    // first content row shown, so a space taller than the list scrolls
    // through.
    let tops = row_heights
        .iter()
        .zip(&gaps)
        .scan(0usize, |top, (height, gap)| {
            let this = *top;
            *top += usize::from(*height) + usize::from(*gap);
            Some(this)
        })
        .collect::<Vec<_>>();
    let content_rows = tops
        .last()
        .zip(row_heights.last())
        .map_or(0, |(top, height)| top + usize::from(*height));
    let viewport = usize::from(body.height);
    let max_scroll = content_rows.saturating_sub(viewport);
    let reveal_navigation = !body.is_empty() && std::mem::take(state.reveal_navigation_workspace);
    let reveal_focus = !body.is_empty() && std::mem::take(state.reveal_focused_workspace);
    if reveal_navigation || reveal_focus {
        let selected_row = rows.iter().position(|row| match row {
            Row::Workspace { endpoint, entry } => {
                let endpoint = &state.endpoints[*endpoint];
                endpoint
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.workspaces.get(entry.index))
                    .is_some_and(|workspace| {
                        if reveal_navigation {
                            state.selected_workspace_id.is_some_and(|target| {
                                target.matches(&endpoint.endpoint_id, &workspace.workspace_id)
                            })
                        } else {
                            &endpoint.endpoint_id == state.active_endpoint_id
                                && active_snapshot.is_some_and(|snapshot| {
                                    snapshot.focused_workspace_id.as_deref()
                                        == Some(workspace.workspace_id.as_str())
                                })
                        }
                    })
            }
            Row::Endpoint(_) => false,
        });
        if let Some(selected_row) = selected_row {
            let row = tops[selected_row];
            *state.workspace_scroll = super::scroll::rows_start_to_reveal(
                *state.workspace_scroll,
                usize::from(body.height),
                row,
                row,
            );
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
    let show_scrollbar = max_scroll > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut scratch = None::<Buffer>;
    for (row_index, row) in rows.iter().enumerate() {
        let top = tops[row_index];
        let row_height = row_heights[row_index];
        if top + usize::from(row_height) <= scroll || row_height == 0 {
            continue;
        }
        if top >= scroll + viewport {
            break;
        }
        // Rows of the block above the list's top, where it starts, and how
        // many rows show.
        let cut = scroll.saturating_sub(top);
        let y = body.y + (top + cut - scroll) as u16;
        let shown = (usize::from(row_height) - cut).min(usize::from(body.bottom() - y)) as u16;
        match row {
            Row::Endpoint(index) => {
                let endpoint = &state.endpoints[*index];
                let rect = Rect::new(body.x, y, content_width, 1);
                let collapsed = state.collapsed_endpoints.contains(&endpoint.endpoint_id);
                let marker = if collapsed { "▸" } else { "▾" };
                let status_badge = render_endpoint_row(
                    buffer,
                    rect,
                    marker,
                    endpoint,
                    collapsed && &endpoint.endpoint_id == state.active_endpoint_id,
                    state.machine_diagnostics,
                    palette,
                );
                hits.machines.push(MachineHit {
                    rect,
                    status_badge,
                    collapse_toggle: Rect::new(
                        rect.x.saturating_add(1),
                        rect.y,
                        u16::from(rect.width > 1),
                        1,
                    ),
                    endpoint_id: endpoint.endpoint_id.clone(),
                });
            }
            Row::Workspace { endpoint, entry } => {
                let endpoint = &state.endpoints[*endpoint];
                let Some(snapshot) = endpoint.snapshot.as_deref() else {
                    continue;
                };
                let Some(workspace) = snapshot.workspaces.get(entry.index) else {
                    continue;
                };
                let collapsed_groups = collapsed_groups_for_endpoint(state, &endpoint.endpoint_id)
                    .unwrap_or(&empty_collapsed_groups);
                let status = super::sidebar::displayed_workspace_status(
                    snapshot,
                    workspace,
                    collapsed_groups,
                );
                let (unfolded, held, kept) = squares_state(state, &endpoint.endpoint_id);
                let tab_lines = super::space_tabs::space_tab_lines(
                    snapshot,
                    workspace,
                    collapsed_groups,
                    unfolded,
                    held,
                    kept,
                    config,
                );
                let tab_jobs = super::space_tabs::space_row_tab_jobs(
                    snapshot,
                    workspace,
                    collapsed_groups,
                    &tab_lines,
                );
                let tokens = super::sidebar::workspace_rows(
                    workspace,
                    status,
                    tab_jobs,
                    entry.indented,
                    &config.spaces,
                );
                let own_rows = tokens.len().max(1).min(u16::MAX as usize) as u16;
                let height = row_height;
                let visible = Rect::new(body.x, y, content_width, shown);
                // A block cut at either edge is drawn whole off screen, then
                // its visible rows are copied.
                let partial = cut > 0 || shown < height;
                let mut block_hits = ShellHitMap::default();
                let target: &mut Buffer = if partial {
                    let area = Rect::new(body.x, 0, body.width, height);
                    let scratch = scratch.get_or_insert_with(|| Buffer::empty(area));
                    scratch.resize(area);
                    scratch.reset();
                    super::render::render_sidebar_background(scratch, area, palette);
                    scratch
                } else {
                    &mut *buffer
                };
                let rect = if partial {
                    Rect::new(body.x, 0, content_width, height)
                } else {
                    visible
                };
                let nested = Rect::new(
                    rect.x.saturating_add(2),
                    rect.y,
                    rect.width.saturating_sub(2),
                    rect.height,
                );
                let endpoint_active = &endpoint.endpoint_id == state.active_endpoint_id;
                let selected = state.selected_workspace_id.is_some_and(|target| {
                    target.matches(&endpoint.endpoint_id, &workspace.workspace_id)
                });
                let icon = aggregate_icon(snapshot, status, config.status_indicators, |agent| {
                    super::sidebar::displayed_workspaces(snapshot, workspace, collapsed_groups)
                        .any(|shown| shown.workspace_id == agent.workspace_id)
                });
                super::sidebar::render_workspace_rows(
                    target,
                    nested,
                    status,
                    icon,
                    entry,
                    tokens,
                    endpoint_active && workspace.focused,
                    selected,
                    state.selected_workspace_id.is_some(),
                    None,
                    false,
                    config,
                );
                let online = endpoint.status == ClientEndpointStatus::Online;
                let tab_hits = super::space_tabs::render_space_tab_lines(
                    target,
                    Rect::new(
                        nested.x,
                        nested.y.saturating_add(own_rows),
                        nested.width,
                        nested.height.saturating_sub(own_rows),
                    ),
                    &tab_lines,
                    endpoint_active && workspace.focused,
                    squares_width.saturating_sub(super::space_tabs::tab_indent(entry.indented)),
                    super::space_tabs::TabLinePointer {
                        square: state.hovered_square.filter(|_| endpoint_active),
                        fold: state.hovered_fold.filter(|_| endpoint_active),
                    },
                    u16::from(show_scrollbar),
                    super::space_tabs::tab_indent(entry.indented),
                    state.tab_line_drag.filter(|_| endpoint_active),
                    None,
                    config,
                );
                // Only the active machine's tab lines and squares take clicks:
                // they act on it; another machine's lines select its space.
                if endpoint_active && online {
                    block_hits.space_tabs.extend(tab_hits.lines);
                    block_hits.space_tab_folds.extend(tab_hits.folds);
                    block_hits.space_tab_squares.extend(tab_hits.squares);
                    block_hits.space_tab_gone.extend(tab_hits.gone);
                    block_hits.tooltips.extend(tab_hits.tooltips);
                    block_hits.space_tab_square_order.extend(tab_hits.order);
                }
                if endpoint.status != ClientEndpointStatus::Online {
                    target.set_style(
                        rect,
                        Style::default()
                            .fg(palette.overlay0)
                            .add_modifier(Modifier::DIM),
                    );
                }
                let group_toggle = if config.spaces.tabs {
                    super::space_tabs::render_space_disclosure(
                        target,
                        nested,
                        snapshot,
                        entry,
                        workspace,
                        collapsed_groups,
                        config,
                    )
                } else {
                    super::sidebar::render_parent_group_toggle(
                        target,
                        rect,
                        snapshot,
                        entry.index,
                        collapsed_groups,
                        palette,
                    )
                };
                block_hits.workspaces.push(WorkspaceHit {
                    rect,
                    endpoint_id: endpoint.endpoint_id.clone(),
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
        }
    }
    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.workspace_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    if config.mouse_capture {
        let label = format!(" new · {}", active_endpoint_label(state));
        hits.new_workspace = Rect::new(
            workspace_area.x,
            footer_y,
            display_width(&label).min(workspace_area.width),
            u16::from(workspace_area.height > 0),
        );
        put_text(
            buffer,
            workspace_area.x,
            footer_y,
            workspace_area.width,
            &label,
            Style::default().fg(palette.overlay0),
        );
        let attention = active_snapshot.is_some_and(super::global_menu::global_menu_attention);
        let width = if attention { 8 } else { 6 }.min(workspace_area.width);
        hits.global_launcher = Rect::new(
            workspace_area.right().saturating_sub(width),
            footer_y,
            width,
            1,
        );
        put_right_text(
            buffer,
            workspace_area,
            footer_y,
            if attention { "● menu" } else { "menu" },
            Style::default().fg(if attention {
                palette.accent
            } else {
                palette.overlay0
            }),
        );
    }
    let (detail_area, usage_area) = super::usage::split_usage_footer(detail_area, state.usage);
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
    super::endpoint_agents::render_expanded(
        buffer,
        detail_area,
        active_snapshot.and_then(|snapshot| snapshot.agent_view_label.as_deref()),
        state.endpoints,
        state.active_endpoint_id,
        config,
        state.agent_scroll,
        hits,
    );
    if let Some(build) = build {
        super::sidebar::render_build_row(buffer, build_area, build, palette);
        hits.tooltips
            .push(super::sidebar::build_row_tooltip(build_area, build));
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

fn active_endpoint_label<'a>(state: &'a ShellRenderState<'_>) -> &'a str {
    state
        .endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == state.active_endpoint_id)
        .map_or("Local", |endpoint| endpoint.label.as_str())
}

fn render_endpoint_row(
    buffer: &mut Buffer,
    rect: Rect,
    marker: &str,
    endpoint: &ClientShellEndpoint,
    highlighted: bool,
    auth: &super::machine_diagnostics::MachineDiagnostics,
    palette: &Palette,
) -> Rect {
    if highlighted {
        buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
    }
    let (glyph, state, color) = endpoint_status_presentation(endpoint.status, palette);
    let state = if endpoint.status == ClientEndpointStatus::Online {
        ""
    } else {
        state
    };
    let signal = if auth.required_for(endpoint) {
        "! auth".to_owned()
    } else if endpoint.status == ClientEndpointStatus::Attention {
        "! error".to_owned()
    } else if endpoint.endpoint_id.is_local() {
        String::new()
    } else if state.is_empty() {
        glyph.to_owned()
    } else {
        format!("{glyph} {state}")
    };
    let signal_width = display_width(&signal).min(rect.width);
    put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width.saturating_sub(signal_width.saturating_add(1)),
        &format!(" {marker} {}", endpoint.label),
        Style::default()
            .fg(
                if matches!(endpoint.status, ClientEndpointStatus::Disabled) {
                    palette.overlay0
                } else {
                    palette.text
                },
            )
            .add_modifier(Modifier::BOLD),
    );
    put_right_text(
        buffer,
        rect,
        rect.y,
        &signal,
        auth.badge_style(endpoint, palette, Style::default().fg(color)),
    );
    Rect::new(
        rect.right().saturating_sub(signal_width),
        rect.y,
        signal_width,
        1,
    )
}
