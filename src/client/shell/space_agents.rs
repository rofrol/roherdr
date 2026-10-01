//! Agents listed under their space (`ui.sidebar.spaces.agents`): per agent a
//! line with its state and task, then a line with its job counts when tabs
//! with a status are nested under its tab, and last an `other jobs` line with
//! the space's running and failed jobs listed under no agent. The first step
//! of folding the agents panel into spaces; the panel stays until this works
//! in daily use.

use std::collections::HashSet;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::*;
use crate::api::schema::TabStatus;
use crate::protocol::{ClientShellSnapshot, ClientShellWorkspace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SpaceAgentLine {
    Agent {
        pane_id: String,
        status: crate::api::schema::AgentStatus,
        mark: AgentMark,
        text: String,
        focused: bool,
    },
    /// Counts of the job tabs nested under the agent's tab, e.g. `⧖ 1 !2`,
    /// each part with the status it counts, for its colour.
    Jobs {
        pane_id: String,
        tab_ids: Vec<String>,
        segments: Vec<(Option<TabStatus>, String)>,
    },
    /// The space's running and failed job tabs that no agent line lists: the
    /// agent's pane closed, the agent view filters it out, or something other
    /// than an agent set the status. Below the agents: they matter less.
    OtherJobs {
        /// The tab a click focuses: the first failed one, else the first running.
        tab_id: String,
        segments: Vec<(Option<TabStatus>, String)>,
    },
}

/// What clicking a line under a space focuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SpaceLineTarget {
    Pane(String),
    Tab(String),
}

impl SpaceAgentLine {
    fn target(&self) -> SpaceLineTarget {
        match self {
            Self::Agent { pane_id, .. } | Self::Jobs { pane_id, .. } => {
                SpaceLineTarget::Pane(pane_id.clone())
            }
            Self::OtherJobs { tab_id, .. } => SpaceLineTarget::Tab(tab_id.clone()),
        }
    }
}

/// The agent lines shown under `workspace`, or none when the setting is off.
/// A collapsed worktree parent stands for its whole group, so it lists the
/// group's agents.
pub(super) fn space_agent_lines(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
    config: &ClientShellConfig,
) -> Vec<SpaceAgentLine> {
    if !config.spaces.agents {
        return Vec::new();
    }
    let workspace_ids = super::sidebar::displayed_workspaces(snapshot, workspace, collapsed_groups)
        .map(|workspace| workspace.workspace_id.as_str())
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    // A tab with several agents shows its jobs under the first one only.
    let mut tabs_with_jobs_shown = HashSet::new();
    for pane_id in super::agent_sidebar::ordered_agent_pane_ids(snapshot, config.agent_panel_sort) {
        let Some(agent) = snapshot.agents.iter().find(|agent| {
            agent.pane_id == pane_id && workspace_ids.contains(&agent.workspace_id.as_str())
        }) else {
            continue;
        };
        let name = agent
            .display_agent
            .as_deref()
            .or(agent.name.as_deref())
            .or(agent.agent.as_deref())
            .unwrap_or("agent");
        // The reported title wins; otherwise the task title the agent sets as
        // its terminal title (Claude Code summarizes the task there), as on tabs.
        let text = [
            agent.title.as_deref(),
            agent.terminal_title_stripped.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|title| !title.is_empty())
        .map_or_else(|| format!("{name} · no task"), str::to_owned);
        lines.push(SpaceAgentLine::Agent {
            pane_id: agent.pane_id.clone(),
            status: agent.agent_status,
            mark: agent_mark(snapshot, agent),
            text,
            focused: agent.focused,
        });
        if !tabs_with_jobs_shown.insert(agent.tab_id.as_str()) {
            continue;
        }
        // Job tabs, not herdr-job's `$jobs` pane token: the token expires and
        // goes with its pane, and the space row counts the same tabs, leaving
        // out the ones listed here (`unlisted_tab_jobs`).
        let jobs = snapshot
            .tabs
            .iter()
            .filter(|tab| {
                tab.parent_tab_id.as_deref() == Some(agent.tab_id.as_str()) && tab.status.is_some()
            })
            .collect::<Vec<_>>();
        if !jobs.is_empty() {
            lines.push(SpaceAgentLine::Jobs {
                pane_id: agent.pane_id.clone(),
                tab_ids: jobs.iter().map(|tab| tab.tab_id.clone()).collect(),
                segments: super::tab_groups::children_summary_segments(&jobs),
            });
        }
    }
    if lines.is_empty() {
        return lines;
    }
    let listed = lines
        .iter()
        .filter_map(|line| match line {
            SpaceAgentLine::Jobs { tab_ids, .. } => Some(tab_ids),
            _ => None,
        })
        .flatten()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let others = snapshot
        .tabs
        .iter()
        .filter(|tab| workspace_ids.contains(&tab.workspace_id.as_str()))
        .filter(|tab| matches!(tab.status, Some(TabStatus::Running | TabStatus::Failed)))
        .filter(|tab| !listed.contains(tab.tab_id.as_str()))
        .collect::<Vec<_>>();
    let target = others
        .iter()
        .find(|tab| tab.status == Some(TabStatus::Failed))
        .or_else(|| others.first());
    if let Some(target) = target {
        lines.push(SpaceAgentLine::OtherJobs {
            tab_id: target.tab_id.clone(),
            segments: super::tab_groups::children_summary_segments(&others),
        });
    }
    lines
}

/// The running and failed tab counts for the space row's `tab_jobs`: none
/// while `lines` lists agents, which show every job themselves or in the
/// `other jobs` line below them.
pub(super) fn space_row_tab_jobs(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
    lines: &[SpaceAgentLine],
) -> (usize, usize) {
    if lines.is_empty() {
        super::sidebar::displayed_workspace_tab_jobs(snapshot, workspace, collapsed_groups)
    } else {
        (0, 0)
    }
}

/// Draws `lines` from the top of `area`, below the space's own rows, and
/// returns each drawn line's rect with what clicking it focuses.
pub(super) fn render_space_agent_lines(
    buffer: &mut Buffer,
    area: Rect,
    lines: &[SpaceAgentLine],
    config: &ClientShellConfig,
) -> Vec<(Rect, SpaceLineTarget)> {
    let palette = &config.palette;
    let right = area.right().saturating_sub(1);
    let mut hits = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let y = area.y.saturating_add(index as u16);
        if y >= area.bottom() {
            break;
        }
        hits.push((Rect::new(area.x, y, area.width, 1), line.target()));
        match line {
            SpaceAgentLine::Agent {
                status,
                mark,
                text,
                focused,
                ..
            } => {
                let x = area.x.saturating_add(3);
                super::render::put_text(
                    buffer,
                    x,
                    y,
                    1,
                    agent_icon(*status, *mark, config.status_indicators),
                    Style::default().fg(agent_color(*status, *mark, palette)),
                );
                let text_x = x.saturating_add(2);
                let width = area.right().saturating_sub(text_x).saturating_sub(1);
                super::render::put_text(
                    buffer,
                    text_x,
                    y,
                    width,
                    &truncate(text, width as usize),
                    Style::default().fg(if *focused {
                        palette.text
                    } else {
                        palette.subtext0
                    }),
                );
            }
            SpaceAgentLine::Jobs { segments, .. } => {
                render_segments(
                    buffer,
                    area.x.saturating_add(5),
                    y,
                    right,
                    segments,
                    palette,
                );
            }
            SpaceAgentLine::OtherJobs { segments, .. } => {
                // Aligned with the agents' icons, not their job lines, so it
                // does not read as the last agent's.
                const LABEL: &str = "other jobs";
                let x = area.x.saturating_add(3);
                let width = right.saturating_sub(x);
                let label = truncate(LABEL, width as usize);
                super::render::put_text(
                    buffer,
                    x,
                    y,
                    width,
                    &label,
                    Style::default().fg(palette.overlay0),
                );
                if label == LABEL {
                    let x = x.saturating_add(LABEL.len() as u16 + 1);
                    render_segments(buffer, x, y, right, segments, palette);
                }
            }
        }
    }
    hits
}

/// Job counts from `x` up to `right`, coloured like the space row's counts
/// and the child-tab row.
fn render_segments(
    buffer: &mut Buffer,
    mut x: u16,
    y: u16,
    right: u16,
    segments: &[(Option<TabStatus>, String)],
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
            Style::default()
                .fg(super::render::tabs::tab_status_color(*status, palette)
                    .unwrap_or(palette.overlay0)),
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
    use crate::protocol::ClientShellAgent;

    fn agent(pane_id: &str, workspace_id: &str, title: Option<&str>) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: workspace_id.into(),
            tab_id: "tab_1".into(),
            name: None,
            display_agent: Some("claude".into()),
            agent: Some("claude".into()),
            title: title.map(str::to_owned),
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Working,
            state_change_seq: 0,
            awaiting_reply: false,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    fn config(agents: bool) -> ClientShellConfig {
        let mut config = ClientShellConfig::from_config(&crate::config::Config::default());
        config.spaces.agents = agents;
        config
    }

    fn tab(
        tab_id: &str,
        parent_tab_id: Option<&str>,
        status: Option<TabStatus>,
    ) -> crate::protocol::ClientShellTab {
        crate::protocol::ClientShellTab {
            tab_id: tab_id.into(),
            parent_tab_id: parent_tab_id.map(str::to_owned),
            status,
            focused: false,
            ..super::super::tests::snapshot().tabs[0].clone()
        }
    }

    #[test]
    fn lists_the_spaces_agents_with_their_jobs_only_when_enabled() {
        let mut snapshot = super::super::tests::snapshot();
        let mut split = agent("pane_2", "ws_1", Some("Same tab"));
        split.tab_id = "tab_1".into();
        let mut idle = agent("pane_3", "ws_1", None);
        idle.tab_id = "tab_2".into();
        snapshot.agents = vec![
            agent("pane_1", "ws_1", Some("Fix the drop marker")),
            split,
            idle,
            agent("pane_4", "elsewhere", Some("not here")),
        ];
        snapshot.tabs.extend([
            tab("tab_2", None, None),
            tab("job_1", Some("tab_1"), Some(TabStatus::Running)),
            tab("job_2", Some("tab_1"), Some(TabStatus::Succeeded)),
            tab("job_3", Some("tab_1"), Some(TabStatus::Succeeded)),
            // A nested tab without a status is not a job.
            tab("notes", Some("tab_1"), None),
        ]);
        let workspace = snapshot.workspaces[0].clone();

        assert!(
            space_agent_lines(&snapshot, &workspace, &HashSet::new(), &config(false)).is_empty()
        );
        assert_eq!(
            space_agent_lines(&snapshot, &workspace, &HashSet::new(), &config(true)),
            vec![
                SpaceAgentLine::Agent {
                    pane_id: "pane_1".into(),
                    status: AgentStatus::Working,
                    mark: AgentMark::None,
                    text: "Fix the drop marker".into(),
                    focused: false,
                },
                SpaceAgentLine::Jobs {
                    pane_id: "pane_1".into(),
                    tab_ids: vec!["job_1".into(), "job_2".into(), "job_3".into()],
                    segments: vec![
                        (Some(TabStatus::Running), "⧖ 1".into()),
                        (Some(TabStatus::Succeeded), "✓2".into()),
                    ],
                },
                // The second agent of the tab does not repeat its jobs.
                SpaceAgentLine::Agent {
                    pane_id: "pane_2".into(),
                    status: AgentStatus::Working,
                    mark: AgentMark::None,
                    text: "Same tab".into(),
                    focused: false,
                },
                SpaceAgentLine::Agent {
                    pane_id: "pane_3".into(),
                    status: AgentStatus::Working,
                    mark: AgentMark::None,
                    text: "claude · no task".into(),
                    focused: false,
                },
            ]
        );
    }

    #[test]
    fn jobs_listed_under_no_agent_go_below_the_agents_not_on_the_space_row() {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.agents = vec![agent("pane_1", "ws_1", None)];
        snapshot.tabs.extend([
            tab("job_1", Some("tab_1"), Some(TabStatus::Failed)),
            tab("job_2", Some("tab_1"), Some(TabStatus::Running)),
            // A status set on a top-level tab, not by a job of an agent.
            tab("build", None, Some(TabStatus::Running)),
            // Its agent's tab is gone, e.g. the agent's pane was closed.
            tab("orphan", Some("tab_9"), Some(TabStatus::Failed)),
            // A finished job no agent lists is not worth a line.
            tab("done", None, Some(TabStatus::Succeeded)),
        ]);
        let workspace = snapshot.workspaces[0].clone();
        let space_row = |snapshot: &ClientShellSnapshot, lines: &[SpaceAgentLine]| {
            space_row_tab_jobs(snapshot, &workspace, &HashSet::new(), lines)
        };

        let lines = space_agent_lines(&snapshot, &workspace, &HashSet::new(), &config(false));
        assert_eq!(space_row(&snapshot, &lines), (2, 2));

        let lines = space_agent_lines(&snapshot, &workspace, &HashSet::new(), &config(true));
        assert_eq!(space_row(&snapshot, &lines), (0, 0));
        assert_eq!(
            lines.last(),
            Some(&SpaceAgentLine::OtherJobs {
                // The failed one, though the running one comes first.
                tab_id: "orphan".into(),
                segments: vec![
                    (Some(TabStatus::Running), "⧖ 1".into()),
                    (Some(TabStatus::Failed), "!1".into()),
                ],
            })
        );

        // A space without agents keeps the counts on its own row.
        snapshot.agents.clear();
        let lines = space_agent_lines(&snapshot, &workspace, &HashSet::new(), &config(true));
        assert!(lines.is_empty());
        assert_eq!(space_row(&snapshot, &lines), (2, 2));
    }

    #[test]
    fn falls_back_to_the_terminal_title_for_the_task() {
        let mut snapshot = super::super::tests::snapshot();
        let mut titled = agent("pane_1", "ws_1", None);
        titled.terminal_title_stripped = Some("Alternatives to herdr".into());
        let mut reported = agent("pane_2", "ws_1", Some("Reported task"));
        reported.terminal_title_stripped = Some("Terminal title".into());
        let mut blank = agent("pane_3", "ws_1", Some(" "));
        blank.terminal_title_stripped = Some("  ".into());
        snapshot.agents = vec![titled, reported, blank];
        let workspace = snapshot.workspaces[0].clone();

        let texts = space_agent_lines(&snapshot, &workspace, &HashSet::new(), &config(true))
            .into_iter()
            .filter_map(|line| match line {
                SpaceAgentLine::Agent { text, .. } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            ["Alternatives to herdr", "Reported task", "claude · no task"]
        );
    }

    #[test]
    fn listing_agents_drops_the_spaces_own_state_icon() {
        let workspace = super::super::tests::snapshot().workspaces[0].clone();
        let has_icon = |agents: bool| {
            super::super::sidebar::workspace_rows(
                &workspace,
                AgentStatus::Working,
                (0, 0),
                false,
                &config(agents).spaces,
            )
            .iter()
            .flatten()
            .any(|token| matches!(token.kind, crate::ui::ResolvedTokenKind::StateIcon))
        };

        assert!(has_icon(false));
        assert!(!has_icon(true));
    }

    #[test]
    fn truncation_keeps_the_width_and_marks_the_cut() {
        assert_eq!(truncate("Name unnamed tabs after", 10), "Name unna…");
        assert_eq!(truncate("short", 10), "short");
    }
}
