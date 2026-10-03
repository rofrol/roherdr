//! The status legend: what each status glyph and colour means. Client
//! presentation only. Every glyph is drawn by the same functions the sidebar
//! uses (`agent_icon`, `agent_color`, `tab_groups::status_icon`), with the
//! user's style, theme and motion phase, so the legend cannot drift from what
//! the sidebar shows; the labels are the ones the glyphs' tooltips show.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::*;
use crate::api::schema::{AgentStatus, TabStatus};
use crate::config::StatusIndicatorStyle;

/// Every agent state the sidebar can show, in the order the legend lists
/// them: the plain statuses, then the marks that override a finished one.
pub(super) const AGENT_STATES: [(AgentStatus, AgentMark); 8] = [
    (AgentStatus::Working, AgentMark::None),
    (AgentStatus::Blocked, AgentMark::None),
    (AgentStatus::Done, AgentMark::None),
    (AgentStatus::Idle, AgentMark::None),
    (AgentStatus::Unknown, AgentMark::None),
    (AgentStatus::Idle, AgentMark::AwaitsReply),
    (AgentStatus::Idle, AgentMark::WaitsOnJob),
    (AgentStatus::Idle, AgentMark::WaitsOnIdleJob),
];

/// Job statuses in the order a group line counts them.
const JOB_STATES: [Option<TabStatus>; 4] = [
    Some(TabStatus::Running),
    Some(TabStatus::Failed),
    Some(TabStatus::Succeeded),
    None,
];

/// What an agent's glyph means. A mark wins over the status, as in
/// `agent_icon`.
pub(super) fn agent_state_label(status: AgentStatus, mark: AgentMark) -> &'static str {
    match mark {
        AgentMark::AwaitsReply => "turn ended with a question for you",
        AgentMark::WaitsOnJob => "turn ended; a job it started still runs",
        AgentMark::WaitsOnIdleJob => "turn ended; its running jobs are silent",
        AgentMark::None => match status {
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked: needs input, approval or a decision",
            AgentStatus::Done => "finished, not looked at yet",
            AgentStatus::Idle => "finished or waiting, already seen",
            AgentStatus::Unknown => "state not detected",
        },
    }
}

/// What the glyph of a tab without an agent means.
pub(super) const PROGRAM_LABEL: &str = "tab running a program, no agent";

/// What a job tab's glyph means.
pub(super) fn job_state_label(status: Option<TabStatus>) -> &'static str {
    match status {
        Some(TabStatus::Running) => "job running",
        Some(TabStatus::Failed) => "job failed",
        Some(TabStatus::Succeeded) => "job succeeded",
        Some(TabStatus::Unknown) | None => "tab without a job status",
    }
}

/// The glyph a job count uses for `status`, as `children_summary` draws it.
fn job_icon(status: Option<TabStatus>) -> &'static str {
    super::tab_groups::status_icon(status).unwrap_or(super::tab_groups::NO_STATUS_ICON)
}

/// The legend's lines, each with its width in cells, for the help popup.
pub(super) fn legend_lines(
    style: StatusIndicatorStyle,
    animations: bool,
    palette: &Palette,
) -> Vec<(usize, Line<'static>)> {
    let base = Style::default()
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::BOLD | Modifier::DIM);
    let heading = |text: &str| {
        (
            text.chars().count() + 1,
            Line::from(Span::styled(
                format!(" {text}"),
                base.fg(palette.accent).add_modifier(Modifier::BOLD),
            )),
        )
    };
    let row = |icon: &str, color: Color, label: &str| {
        // In the dots style several states share a glyph; name the colour.
        let label = if style == StatusIndicatorStyle::Dots {
            format!("{label} ({})", color_name(color, palette))
        } else {
            label.to_owned()
        };
        (
            4 + label.chars().count(),
            Line::from(vec![
                Span::styled(format!("  {icon} "), base.fg(color)),
                Span::styled(label, base.fg(palette.text)),
            ]),
        )
    };
    let mut lines = vec![(
        0,
        Line::from(Span::styled(
            format!(
                " style: {} · animation {} (settings)",
                style.as_str(),
                if animations { "on" } else { "off" }
            ),
            base.fg(palette.overlay0),
        )),
    )];
    lines.push((0, Line::raw("")));
    lines.push(heading("agents"));
    for (status, mark) in AGENT_STATES {
        lines.push(row(
            agent_icon(status, mark, style),
            agent_color(status, mark, palette),
            agent_state_label(status, mark),
        ));
    }
    lines.push(row(
        super::space_tabs::PROGRAM_ICON,
        palette.overlay0,
        PROGRAM_LABEL,
    ));
    lines.push((0, Line::raw("")));
    lines.push(heading("jobs"));
    for status in JOB_STATES {
        lines.push(row(
            job_icon(status),
            super::render::tabs::tab_status_color(status, palette).unwrap_or(palette.overlay0),
            job_state_label(status),
        ));
    }
    lines.push((0, Line::raw("")));
    lines.push(heading("group counts"));
    let example = JOB_STATES
        .iter()
        .zip([1, 2, 3, 1])
        .map(|(status, count)| format!("{}{count}", job_icon(*status)))
        .collect::<Vec<_>>()
        .join(" ");
    let text =
        format!("  {example}  a tab's jobs by status: running, failed, succeeded, without status");
    lines.push((
        text.chars().count(),
        Line::from(Span::styled(text, base.fg(palette.text))),
    ));
    lines
}

/// The palette role `color` comes from, for the dots style's labels.
fn color_name(color: Color, palette: &Palette) -> &'static str {
    [
        (palette.yellow, "yellow"),
        (palette.red, "red"),
        (palette.teal, "teal"),
        (palette.green, "green"),
        (palette.mauve, "mauve"),
        (palette.overlay1, "grey"),
        (palette.overlay0, "dim grey"),
    ]
    .into_iter()
    .find(|(role, _)| *role == color)
    .map_or("", |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[(usize, Line<'static>)]) -> String {
        lines
            .iter()
            .map(|(_, line)| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_agent_and_job_state_appears_with_its_real_glyph_in_every_style() {
        let palette = Palette::catppuccin();
        for style in [
            StatusIndicatorStyle::Dots,
            StatusIndicatorStyle::Symbols,
            StatusIndicatorStyle::Shapes,
        ] {
            let legend = text(&legend_lines(style, false, &palette));
            for status in [
                AgentStatus::Working,
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Idle,
                AgentStatus::Unknown,
            ] {
                for mark in [
                    AgentMark::None,
                    AgentMark::AwaitsReply,
                    AgentMark::WaitsOnJob,
                    AgentMark::WaitsOnIdleJob,
                ] {
                    let shown = format!(
                        "{} {}",
                        agent_icon(status, mark, style),
                        agent_state_label(status, mark)
                    );
                    // A mark only shows on a finished agent, so the
                    // legend lists it once, under any finished status.
                    if mark == AgentMark::None || status == AgentStatus::Idle {
                        assert!(legend.contains(&shown), "{style:?}: {shown}\n{legend}");
                    }
                }
            }
            for status in JOB_STATES {
                let shown = format!("{} {}", job_icon(status), job_state_label(status));
                assert!(legend.contains(&shown), "{style:?}: {shown}\n{legend}");
            }
        }
    }

    #[test]
    fn a_mark_means_the_same_whatever_the_finished_status_under_it() {
        for mark in [
            AgentMark::AwaitsReply,
            AgentMark::WaitsOnJob,
            AgentMark::WaitsOnIdleJob,
        ] {
            assert_eq!(
                agent_state_label(AgentStatus::Idle, mark),
                agent_state_label(AgentStatus::Done, mark)
            );
            assert_eq!(
                agent_icon(AgentStatus::Idle, mark, StatusIndicatorStyle::Shapes),
                agent_icon(AgentStatus::Done, mark, StatusIndicatorStyle::Shapes)
            );
        }
    }

    #[test]
    fn the_dots_style_names_the_colours_its_shared_glyphs_differ_by() {
        let legend = text(&legend_lines(
            StatusIndicatorStyle::Dots,
            true,
            &Palette::catppuccin(),
        ));
        assert!(legend.contains("● working (yellow)"), "{legend}");
        assert!(
            legend.contains("● finished, not looked at yet (teal)"),
            "{legend}"
        );
        assert!(legend.contains("animation on"), "{legend}");
    }
}
