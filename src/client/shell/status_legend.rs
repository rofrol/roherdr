//! The status legend: what each status glyph, mark, count and token means,
//! per domain. Client presentation only. Every glyph is drawn by the same
//! functions the sidebar uses (`agent_icon`, `agent_color`,
//! `tab_groups::status_icon`, the header indicators', the Items button's,
//! the `?` list's and the token renderer), with the user's style, theme and
//! motion phase, so the legend cannot drift from what the sidebar shows; the
//! labels are the ones the glyphs' tooltips show. Each row has a swatch in
//! the colour the theme really renders, never a colour name, and counts are
//! explained by example (`!2` two failed jobs).
//!
//! A click on a glyph that has no action of its own opens the legend at its
//! entry ([`LegendEntry`]); a glyph with its own action (a header button,
//! the Items button, the fold counts, the branch chip) offers the legend in
//! its tooltip instead.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::*;
use crate::api::schema::{
    AgentStatus, HistoryEventKind, TabRole, TabStatus, WorkerRunOutcome, WorkerVerdict,
};
use crate::config::StatusIndicatorStyle;

/// Every agent state the sidebar can show, in the order the legend lists
/// them: the plain statuses, then the marks that override a finished one.
pub(super) const AGENT_STATES: [(AgentStatus, AgentMark); 9] = [
    (AgentStatus::Working, AgentMark::None),
    (AgentStatus::Blocked, AgentMark::None),
    (AgentStatus::Done, AgentMark::None),
    (AgentStatus::Idle, AgentMark::None),
    (AgentStatus::Unknown, AgentMark::None),
    (AgentStatus::Idle, AgentMark::AwaitsReply),
    (AgentStatus::Idle, AgentMark::WaitsOnJob),
    (AgentStatus::Idle, AgentMark::WaitsOnIdleJob),
    // Shown in the finished colour it keeps: the dot it stands apart from.
    (AgentStatus::Done, AgentMark::BackgroundWork),
];

/// Job statuses in the order a group line counts them.
const JOB_STATES: [Option<TabStatus>; 4] = [
    Some(TabStatus::Running),
    Some(TabStatus::Failed),
    Some(TabStatus::Succeeded),
    None,
];

/// The roles whose mark a tab line draws.
const ROLES: [TabRole; 2] = [TabRole::Coordinator, TabRole::Worker];

/// A headless worker's states as `worker.status` names them; the empty name
/// stands for a state this client does not know (a newer server's).
pub(super) const WORKER_STATES: [&str; 9] = [
    "starting",
    "working",
    "waiting_approval",
    "finished",
    "interrupted",
    "failed",
    "exited",
    "lost",
    "",
];

const RUN_OUTCOMES: [WorkerRunOutcome; 7] = [
    WorkerRunOutcome::Running,
    WorkerRunOutcome::Finished,
    WorkerRunOutcome::Failed,
    WorkerRunOutcome::Exited,
    WorkerRunOutcome::Lost,
    WorkerRunOutcome::Degraded,
    WorkerRunOutcome::Unknown,
];

const VERDICTS: [WorkerVerdict; 4] = [
    WorkerVerdict::Verified,
    WorkerVerdict::Failed,
    WorkerVerdict::Unavailable,
    WorkerVerdict::Unknown,
];

/// The records of an item's timeline that carry a glyph.
const GLYPH_RECORDS: [HistoryEventKind; 3] = [
    HistoryEventKind::Closed,
    HistoryEventKind::Blocked,
    HistoryEventKind::Aborted,
];

const HEADER_INDICATORS: [super::sidebar::HeaderIndicator; 4] = [
    super::sidebar::HeaderIndicator::History,
    super::sidebar::HeaderIndicator::Asking,
    super::sidebar::HeaderIndicator::Working,
    super::sidebar::HeaderIndicator::Bookmarks,
];

/// What a row of the `?` list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AskRow {
    /// An agent waiting on the user.
    Agent,
    /// A headless worker's question for the user, or one its coordinator
    /// escalated.
    WorkerQuestion,
    /// A worker's question its coordinator handles.
    Quiet,
}

const ASK_ROWS: [AskRow; 3] = [AskRow::Agent, AskRow::WorkerQuestion, AskRow::Quiet];

/// What a count of the Items button counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemsCount {
    InProgress,
    Attention,
}

/// A part of the branch chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitPart {
    Ahead,
    Behind,
    InSync,
}

const GIT_PARTS: [GitPart; 3] = [GitPart::Ahead, GitPart::Behind, GitPart::InSync];

/// An entry of the legend: what a click on a glyph opens it at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LegendEntry {
    Agent(AgentStatus, AgentMark),
    Program,
    Role(TabRole),
    /// A headless worker's line, by its state's name ([`WORKER_STATES`]).
    Worker(&'static str),
    Job(Option<TabStatus>),
    /// The counts at the end of a tab line with jobs under it.
    GroupCounts,
    /// The job counts at the end of a space's name line.
    SpaceJobs,
    /// The `bg` token of an agent's own background work.
    BackgroundToken,
    Header(super::sidebar::HeaderIndicator),
    Ask(AskRow),
    Items(ItemsCount),
    RunOutcome(WorkerRunOutcome),
    Verdict(WorkerVerdict),
    Record(HistoryEventKind),
    Git(GitPart),
}

/// The legend entry of an agent's glyph: a mark is listed once, under the
/// finished status it shows on in [`AGENT_STATES`], whatever status is under
/// it.
pub(super) fn agent_entry(status: AgentStatus, mark: AgentMark) -> LegendEntry {
    let status = match mark {
        AgentMark::None => status,
        AgentMark::BackgroundWork => AgentStatus::Done,
        AgentMark::AwaitsReply | AgentMark::WaitsOnJob | AgentMark::WaitsOnIdleJob => {
            AgentStatus::Idle
        }
    };
    LegendEntry::Agent(status, mark)
}

/// The legend entry of a worker in `state`.
pub(super) fn worker_entry(state: &str) -> LegendEntry {
    LegendEntry::Worker(
        WORKER_STATES
            .into_iter()
            .find(|known| !known.is_empty() && *known == state)
            .unwrap_or(""),
    )
}

/// What an agent's glyph means. A mark wins over the status, as in
/// `agent_icon`.
pub(super) fn agent_state_label(status: AgentStatus, mark: AgentMark) -> &'static str {
    match mark {
        AgentMark::AwaitsReply => "turn ended with a question for you",
        AgentMark::WaitsOnJob => "turn ended; a job it started still runs",
        AgentMark::WaitsOnIdleJob => "turn ended; its running jobs are silent",
        AgentMark::BackgroundWork => "finished, background work running (its bg token names it)",
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

/// What a headless worker's glyph means, as its tooltip says it.
fn worker_state_label(state: &str) -> String {
    format!("worker {}", super::space_tabs::worker_state(state).1)
}

/// What a `?` list row means.
fn ask_label(row: AskRow) -> &'static str {
    match row {
        AskRow::Agent => "an agent waits on you; the line under says for what",
        AskRow::WorkerQuestion => "a worker asks you (or was escalated)",
        AskRow::Quiet => "its coordinator's; uncounted",
    }
}

fn header_label(indicator: super::sidebar::HeaderIndicator) -> &'static str {
    use super::sidebar::HeaderIndicator;
    match indicator {
        HeaderIndicator::History => "3 unread notifications; a click lists them",
        HeaderIndicator::Asking => "2 agents or workers wait on you; a click lists them",
        HeaderIndicator::Working => "1 agent works or waits on a job; a click lists it",
        HeaderIndicator::Bookmarks => "4 bookmarks; a click lists them",
    }
}

/// The count each header example shows.
fn header_example_count(indicator: super::sidebar::HeaderIndicator) -> usize {
    use super::sidebar::HeaderIndicator;
    match indicator {
        HeaderIndicator::History => 3,
        HeaderIndicator::Asking => 2,
        HeaderIndicator::Working => 1,
        HeaderIndicator::Bookmarks => 4,
    }
}

fn items_label(count: ItemsCount) -> &'static str {
    match count {
        ItemsCount::InProgress => "3 TODO items have a worker running",
        ItemsCount::Attention => "1 item's run ended (done, failed, blocked): to review",
    }
}

fn items_example(count: ItemsCount) -> crate::protocol::ClientShellWorkerItems {
    let (in_progress, attention) = match count {
        ItemsCount::InProgress => (3, 0),
        ItemsCount::Attention => (0, 1),
    };
    crate::protocol::ClientShellWorkerItems {
        tab_id: String::new(),
        repo: String::new(),
        in_progress,
        attention,
    }
}

fn run_outcome_label(outcome: WorkerRunOutcome) -> String {
    format!("run {}", super::worker_items::outcome(outcome).0)
}

fn verdict_label(verdict: WorkerVerdict) -> String {
    format!("attempt: {}", super::worker_items::verdict(verdict))
}

fn record_label(kind: HistoryEventKind) -> String {
    let said = super::worker_items::record(kind).0;
    match kind {
        HistoryEventKind::Blocked => format!("item {said}: its run stopped, a decision is due"),
        _ => format!("item {said}"),
    }
}

fn git_label(part: GitPart) -> &'static str {
    match part {
        GitPart::Ahead => "4 commits ahead of its upstream: not pushed yet",
        GitPart::Behind => "2 commits behind its upstream: not pulled yet",
        GitPart::InSync => "branch in sync with its upstream",
    }
}

const GROUP_COUNTS_LABEL: &str = "a tab's jobs: 1 running, 2 failed, 3 done, 1 no status";
const SPACE_JOBS_LABEL: &str = "a space's jobs: 2 running, 1 failed";
const BACKGROUND_TOKEN_EXAMPLE: &str = "1 bg ⧖ Build";
const BACKGROUND_TOKEN_LABEL: &str = "1 background task of the agent; ⧖ the job it waits on";

/// The glyph a job count uses for `status`, as `children_summary` draws it.
fn job_icon(status: Option<TabStatus>) -> &'static str {
    super::tab_groups::status_icon(status).unwrap_or(super::tab_groups::NO_STATUS_ICON)
}

/// A line of the legend: its width in cells, and the entry it explains.
pub(super) struct LegendLine {
    pub(super) width: usize,
    pub(super) line: Line<'static>,
    pub(super) entry: Option<LegendEntry>,
}

/// The legend's lines, for the help popup.
pub(super) fn legend_lines(
    style: StatusIndicatorStyle,
    animations: bool,
    palette: &Palette,
) -> Vec<LegendLine> {
    let base = Style::default()
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::BOLD | Modifier::DIM);
    let plain = |text: String, style: Style| LegendLine {
        width: text.chars().count(),
        line: Line::from(Span::styled(text, style)),
        entry: None,
    };
    let heading = |text: &str| {
        plain(
            format!(" {text}"),
            base.fg(palette.accent).add_modifier(Modifier::BOLD),
        )
    };
    let blank = || LegendLine {
        width: 0,
        line: Line::raw(""),
        entry: None,
    };
    // A glyph drawn as the sidebar draws it, a swatch of the colour it is
    // drawn in, and what it means.
    let row = |icon: &str, color: Color, label: &str, entry: LegendEntry| {
        let icon = format!("  {icon} ");
        LegendLine {
            width: icon.chars().count() + 3 + label.chars().count(),
            line: Line::from(vec![
                Span::styled(icon, base.fg(color)),
                Span::styled("  ", base.bg(color)),
                Span::styled(" ", base),
                Span::styled(label.to_owned(), base.fg(palette.text)),
            ]),
            entry: Some(entry),
        }
    };
    // An example as the sidebar draws it (`spans`, in their own colours),
    // then what it means.
    let example = |spans: Vec<Span<'static>>, label: &str, entry: LegendEntry| {
        let mut line = vec![Span::styled("  ", base)];
        let mut width = 2;
        for span in spans {
            width += super::render::display_width(&span.content) as usize;
            line.push(Span::styled(span.content, base.patch(span.style)));
        }
        line.push(Span::styled("  ", base));
        line.push(Span::styled(label.to_owned(), base.fg(palette.text)));
        LegendLine {
            width: width + 2 + label.chars().count(),
            line: Line::from(line),
            entry: Some(entry),
        }
    };
    let agent_glyph = |status: AgentStatus| {
        (
            agent_icon(status, AgentMark::None, style),
            agent_color(status, AgentMark::None, palette),
        )
    };
    let token = |kind: crate::ui::ResolvedTokenKind| {
        crate::ui::resolved_token_spans(
            &[crate::ui::ResolvedToken {
                kind,
                style: Default::default(),
            }],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default().fg(palette.overlay1),
            palette,
            usize::MAX,
        )
    };

    let mut lines = vec![plain(
        format!(
            " style: {} · animation {} (settings)",
            style.as_str(),
            if animations { "on" } else { "off" }
        ),
        base.fg(palette.overlay0),
    )];
    lines.push(blank());
    lines.push(heading("agents"));
    for (status, mark) in AGENT_STATES {
        lines.push(row(
            agent_icon(status, mark, style),
            agent_color(status, mark, palette),
            agent_state_label(status, mark),
            LegendEntry::Agent(status, mark),
        ));
    }
    lines.push(row(
        super::space_tabs::PROGRAM_ICON,
        palette.overlay0,
        PROGRAM_LABEL,
        LegendEntry::Program,
    ));
    for role in ROLES {
        if let Some((glyph, meaning)) = super::space_tabs::role_marker(role) {
            lines.push(row(
                glyph,
                super::space_tabs::role_mark_color(palette),
                meaning,
                LegendEntry::Role(role),
            ));
        }
    }
    lines.push(example(
        vec![Span::styled(
            BACKGROUND_TOKEN_EXAMPLE,
            Style::default().fg(palette.overlay1),
        )],
        BACKGROUND_TOKEN_LABEL,
        LegendEntry::BackgroundToken,
    ));
    lines.push(blank());
    lines.push(heading("headless workers"));
    for state in WORKER_STATES {
        let (icon, color) = agent_glyph(super::space_tabs::worker_state(state).0);
        lines.push(row(
            icon,
            color,
            &worker_state_label(state),
            LegendEntry::Worker(state),
        ));
    }
    lines.push(blank());
    lines.push(heading("jobs"));
    for status in JOB_STATES {
        lines.push(row(
            job_icon(status),
            super::render::tabs::tab_status_color(status, palette).unwrap_or(palette.overlay0),
            job_state_label(status),
            LegendEntry::Job(status),
        ));
    }
    let group = JOB_STATES
        .iter()
        .zip([1, 2, 3, 1])
        .enumerate()
        .flat_map(|(index, (status, count))| {
            let color =
                super::render::tabs::tab_status_color(*status, palette).unwrap_or(palette.overlay0);
            let separator = (index > 0).then(|| Span::raw(" "));
            separator.into_iter().chain([Span::styled(
                super::tab_groups::count_segment(*status, count),
                Style::default().fg(color),
            )])
        })
        .collect();
    lines.push(example(group, GROUP_COUNTS_LABEL, LegendEntry::GroupCounts));
    lines.push(example(
        token(crate::ui::ResolvedTokenKind::TabJobs {
            running: 2,
            failed: 1,
        }),
        SPACE_JOBS_LABEL,
        LegendEntry::SpaceJobs,
    ));
    lines.push(blank());
    lines.push(heading(
        "git (a space's branch chip; a click opens its menu)",
    ));
    for part in GIT_PARTS {
        let (branch, ahead_behind) = match part {
            GitPart::Ahead => (None, (4, 0)),
            GitPart::Behind => (None, (0, 2)),
            GitPart::InSync => (Some("main".to_owned()), (0, 0)),
        };
        lines.push(example(
            token(crate::ui::ResolvedTokenKind::PushStatus {
                branch,
                ahead_behind: Some(ahead_behind),
            }),
            git_label(part),
            LegendEntry::Git(part),
        ));
    }
    lines.push(blank());
    lines.push(heading("header"));
    for indicator in HEADER_INDICATORS {
        let count = header_example_count(indicator);
        lines.push(example(
            vec![Span::styled(
                indicator.label(count),
                indicator.style(count, palette),
            )],
            header_label(indicator),
            LegendEntry::Header(indicator),
        ));
    }
    lines.push(blank());
    lines.push(heading("the ? list"));
    for ask in ASK_ROWS {
        let question = crate::protocol::ClientShellWorkerQuestion {
            worker_id: "w12".to_owned(),
            request_id: String::new(),
            cwd: "herdr".to_owned(),
            tool_name: String::new(),
            text: String::new(),
            choice: false,
            since_ms: 0,
            quiet: ask == AskRow::Quiet,
            owner_seen_ms: None,
        };
        let spans = match ask {
            AskRow::Agent => {
                let (icon, color) = agent_glyph(AgentStatus::Blocked);
                vec![
                    Span::styled(icon, Style::default().fg(color)),
                    Span::styled(" fix login", Style::default().fg(palette.text)),
                ]
            }
            AskRow::WorkerQuestion => vec![Span::styled(
                format!(
                    "? {}",
                    ClientShellState::worker_question_row_at(&question, 0).title
                ),
                Style::default().fg(palette.text),
            )],
            AskRow::Quiet => vec![Span::styled(
                ClientShellState::worker_question_row_at(&question, 0).title,
                Style::default().fg(palette.overlay0),
            )],
        };
        lines.push(example(spans, ask_label(ask), LegendEntry::Ask(ask)));
    }
    lines.push(blank());
    lines.push(heading(
        "TODO items (a coordinator's Items button and list)",
    ));
    for count in [ItemsCount::InProgress, ItemsCount::Attention] {
        let counts = items_example(count);
        lines.push(example(
            vec![Span::styled(
                super::worker_items::items_button_text(&super::worker_items::items_badge(&counts)),
                Style::default().fg(palette.accent),
            )],
            items_label(count),
            LegendEntry::Items(count),
        ));
    }
    for outcome in RUN_OUTCOMES {
        let (icon, color) = agent_glyph(super::worker_items::outcome(outcome).1);
        lines.push(row(
            icon,
            color,
            &run_outcome_label(outcome),
            LegendEntry::RunOutcome(outcome),
        ));
    }
    for verdict in VERDICTS {
        let (icon, color) = agent_glyph(super::worker_items::verdict_status(Some(verdict)));
        lines.push(row(
            icon,
            color,
            &verdict_label(verdict),
            LegendEntry::Verdict(verdict),
        ));
    }
    for kind in GLYPH_RECORDS {
        if let Some(status) = super::worker_items::record(kind).1 {
            let (icon, color) = agent_glyph(status);
            lines.push(row(
                icon,
                color,
                &record_label(kind),
                LegendEntry::Record(kind),
            ));
        }
    }
    lines
}

/// The width the legend's text wraps at in a popup at full size: the
/// popup's 76 columns less its border and scrollbar.
const LEGEND_TEXT_WIDTH: usize = 73;

/// The row `entry` starts at, wrapped as the popup wraps it at full size.
fn entry_row(lines: &[LegendLine], entry: LegendEntry) -> Option<usize> {
    let mut row = 0;
    for line in lines {
        if line.entry == Some(entry) {
            return Some(row);
        }
        row += line.width.max(1).div_ceil(LEGEND_TEXT_WIDTH);
    }
    None
}

impl ClientShellState {
    /// Opens the status legend, scrolled to `entry` and marking it, or at
    /// its top.
    pub(super) fn open_status_legend(&mut self, entry: Option<LegendEntry>) {
        let lines = legend_lines(
            self.config.status_indicators,
            self.config.animations,
            &self.config.palette,
        );
        // A row above it stays in sight, so the entry is not the very top.
        let scroll = entry
            .and_then(|entry| entry_row(&lines, entry))
            .map_or(0, |row| row.saturating_sub(1));
        self.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
            legend: true,
            focus: entry,
            query: TextEditor::default(),
            search_focused: false,
            scroll,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[LegendLine]) -> String {
        lines
            .iter()
            .map(|line| line.line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    const STYLES: [StatusIndicatorStyle; 3] = [
        StatusIndicatorStyle::Dots,
        StatusIndicatorStyle::Symbols,
        StatusIndicatorStyle::Shapes,
    ];

    // Each list below names every variant through an exhaustive match, so a
    // new variant fails to compile here until the legend lists it.

    fn every_agent_status() -> Vec<AgentStatus> {
        [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Idle,
            AgentStatus::Unknown,
        ]
        .into_iter()
        .inspect(|status| match status {
            AgentStatus::Working
            | AgentStatus::Blocked
            | AgentStatus::Done
            | AgentStatus::Idle
            | AgentStatus::Unknown => {}
        })
        .collect()
    }

    fn every_mark() -> Vec<AgentMark> {
        [
            AgentMark::None,
            AgentMark::AwaitsReply,
            AgentMark::WaitsOnJob,
            AgentMark::WaitsOnIdleJob,
            AgentMark::BackgroundWork,
        ]
        .into_iter()
        .inspect(|mark| match mark {
            AgentMark::None
            | AgentMark::AwaitsReply
            | AgentMark::WaitsOnJob
            | AgentMark::WaitsOnIdleJob
            | AgentMark::BackgroundWork => {}
        })
        .collect()
    }

    fn every_job_status() -> Vec<Option<TabStatus>> {
        [
            None,
            Some(TabStatus::Running),
            Some(TabStatus::Failed),
            Some(TabStatus::Succeeded),
            Some(TabStatus::Unknown),
        ]
        .into_iter()
        .inspect(|status| match status {
            None
            | Some(TabStatus::Running)
            | Some(TabStatus::Failed)
            | Some(TabStatus::Succeeded)
            | Some(TabStatus::Unknown) => {}
        })
        .collect()
    }

    fn every_role() -> Vec<TabRole> {
        [TabRole::Coordinator, TabRole::Worker, TabRole::Unknown]
            .into_iter()
            .inspect(|role| match role {
                TabRole::Coordinator | TabRole::Worker | TabRole::Unknown => {}
            })
            .collect()
    }

    fn every_outcome() -> Vec<WorkerRunOutcome> {
        RUN_OUTCOMES
            .into_iter()
            .inspect(|outcome| match outcome {
                WorkerRunOutcome::Running
                | WorkerRunOutcome::Finished
                | WorkerRunOutcome::Failed
                | WorkerRunOutcome::Exited
                | WorkerRunOutcome::Lost
                | WorkerRunOutcome::Degraded
                | WorkerRunOutcome::Unknown => {}
            })
            .collect()
    }

    fn every_verdict() -> Vec<WorkerVerdict> {
        VERDICTS
            .into_iter()
            .inspect(|verdict| match verdict {
                WorkerVerdict::Verified
                | WorkerVerdict::Failed
                | WorkerVerdict::Unavailable
                | WorkerVerdict::Unknown => {}
            })
            .collect()
    }

    fn every_record() -> Vec<HistoryEventKind> {
        [
            HistoryEventKind::Claimed,
            HistoryEventKind::Noted,
            HistoryEventKind::Closed,
            HistoryEventKind::Aborted,
            HistoryEventKind::Blocked,
            HistoryEventKind::CoordinatorStarted,
            HistoryEventKind::CoordinatorEnded,
            HistoryEventKind::Stopped,
            HistoryEventKind::Unknown,
        ]
        .into_iter()
        .inspect(|kind| match kind {
            HistoryEventKind::Claimed
            | HistoryEventKind::Noted
            | HistoryEventKind::Closed
            | HistoryEventKind::Aborted
            | HistoryEventKind::Blocked
            | HistoryEventKind::CoordinatorStarted
            | HistoryEventKind::CoordinatorEnded
            | HistoryEventKind::Stopped
            | HistoryEventKind::Unknown => {}
        })
        .collect()
    }

    fn every_header_indicator() -> Vec<super::super::sidebar::HeaderIndicator> {
        use super::super::sidebar::HeaderIndicator;
        HEADER_INDICATORS
            .into_iter()
            .inspect(|indicator| match indicator {
                HeaderIndicator::History
                | HeaderIndicator::Asking
                | HeaderIndicator::Working
                | HeaderIndicator::Bookmarks => {}
            })
            .collect()
    }

    /// Every entry of every domain with the text its line must show: the
    /// glyph or example as the sidebar draws it, then its label.
    fn expected(style: StatusIndicatorStyle) -> Vec<(LegendEntry, String)> {
        let mut expected = Vec::new();
        for status in every_agent_status() {
            for mark in every_mark() {
                // A mark only shows on a finished agent, so the legend lists
                // it once, under one finished status.
                let listed_under = match mark {
                    AgentMark::BackgroundWork => AgentStatus::Done,
                    _ => AgentStatus::Idle,
                };
                if mark == AgentMark::None || status == listed_under {
                    expected.push((
                        LegendEntry::Agent(status, mark),
                        format!(
                            "{}    {}",
                            agent_icon(status, mark, style),
                            agent_state_label(status, mark)
                        ),
                    ));
                }
            }
        }
        expected.push((
            LegendEntry::Program,
            format!(
                "{}    {PROGRAM_LABEL}",
                super::super::space_tabs::PROGRAM_ICON
            ),
        ));
        for role in every_role() {
            // A role this build does not know draws no mark.
            if let Some((glyph, meaning)) = super::super::space_tabs::role_marker(role) {
                expected.push((LegendEntry::Role(role), format!("{glyph}    {meaning}")));
            }
        }
        expected.push((
            LegendEntry::BackgroundToken,
            format!("{BACKGROUND_TOKEN_EXAMPLE}  {BACKGROUND_TOKEN_LABEL}"),
        ));
        for state in WORKER_STATES {
            let status = super::super::space_tabs::worker_state(state).0;
            expected.push((
                LegendEntry::Worker(state),
                format!(
                    "{}    {}",
                    agent_icon(status, AgentMark::None, style),
                    worker_state_label(state)
                ),
            ));
        }
        for status in every_job_status() {
            // A newer server's job status reads as none.
            let shown = status.filter(|status| *status != TabStatus::Unknown);
            expected.push((
                LegendEntry::Job(shown),
                format!("{}    {}", job_icon(status), job_state_label(status)),
            ));
        }
        expected.push((LegendEntry::GroupCounts, GROUP_COUNTS_LABEL.to_owned()));
        expected.push((
            LegendEntry::SpaceJobs,
            format!(
                "{} 2 !1  {SPACE_JOBS_LABEL}",
                crate::ui::motion::job_glyph()
            ),
        ));
        expected.push((
            LegendEntry::Git(GitPart::Ahead),
            format!("↑4  {}", git_label(GitPart::Ahead)),
        ));
        expected.push((
            LegendEntry::Git(GitPart::Behind),
            format!("↓2  {}", git_label(GitPart::Behind)),
        ));
        expected.push((
            LegendEntry::Git(GitPart::InSync),
            format!("main  {}", git_label(GitPart::InSync)),
        ));
        for indicator in every_header_indicator() {
            expected.push((
                LegendEntry::Header(indicator),
                format!(
                    "{}  {}",
                    indicator.label(header_example_count(indicator)),
                    header_label(indicator)
                ),
            ));
        }
        for ask in ASK_ROWS {
            expected.push((LegendEntry::Ask(ask), ask_label(ask).to_owned()));
        }
        expected.push((
            LegendEntry::Items(ItemsCount::InProgress),
            format!("Items 3  {}", items_label(ItemsCount::InProgress)),
        ));
        expected.push((
            LegendEntry::Items(ItemsCount::Attention),
            format!("Items 1!  {}", items_label(ItemsCount::Attention)),
        ));
        for outcome in every_outcome() {
            let status = super::super::worker_items::outcome(outcome).1;
            expected.push((
                LegendEntry::RunOutcome(outcome),
                format!(
                    "{}    {}",
                    agent_icon(status, AgentMark::None, style),
                    run_outcome_label(outcome)
                ),
            ));
        }
        for verdict in every_verdict() {
            let status = super::super::worker_items::verdict_status(Some(verdict));
            expected.push((
                LegendEntry::Verdict(verdict),
                format!(
                    "{}    {}",
                    agent_icon(status, AgentMark::None, style),
                    verdict_label(verdict)
                ),
            ));
        }
        for kind in every_record() {
            // Records drawn without a glyph have no entry.
            if let Some(status) = super::super::worker_items::record(kind).1 {
                expected.push((
                    LegendEntry::Record(kind),
                    format!(
                        "{}    {}",
                        agent_icon(status, AgentMark::None, style),
                        record_label(kind)
                    ),
                ));
            }
        }
        expected
    }

    #[test]
    fn every_state_of_every_domain_has_an_entry_with_its_real_glyph_and_label() {
        let palette = Palette::catppuccin();
        for style in STYLES {
            let lines = legend_lines(style, false, &palette);
            for (entry, shown) in expected(style) {
                let line = lines
                    .iter()
                    .find(|line| line.entry == Some(entry))
                    .unwrap_or_else(|| panic!("{style:?}: no entry {entry:?}"));
                let line = line.line.to_string();
                assert!(line.contains(&shown), "{style:?} {entry:?}: {line:?}");
            }
        }
    }

    #[test]
    fn every_line_with_an_entry_is_expected_and_listed_once() {
        let palette = Palette::catppuccin();
        let lines = legend_lines(StatusIndicatorStyle::Shapes, true, &palette);
        let expected = expected(StatusIndicatorStyle::Shapes);
        let mut seen = Vec::new();
        for entry in lines.iter().filter_map(|line| line.entry) {
            assert!(
                expected.iter().any(|(known, _)| *known == entry),
                "unexpected {entry:?}"
            );
            assert!(!seen.contains(&entry), "{entry:?} listed twice");
            seen.push(entry);
        }
    }

    #[test]
    fn every_agent_glyph_reaches_an_entry_the_legend_lists() {
        let lines = legend_lines(StatusIndicatorStyle::Shapes, false, &Palette::catppuccin());
        for status in every_agent_status() {
            for mark in every_mark() {
                let entry = agent_entry(status, mark);
                assert!(
                    lines.iter().any(|line| line.entry == Some(entry)),
                    "{status:?} {mark:?}"
                );
            }
        }
    }

    #[test]
    fn every_worker_state_reaches_its_entry_and_an_unknown_one_the_unknown_entry() {
        for state in WORKER_STATES.into_iter().filter(|state| !state.is_empty()) {
            assert_eq!(worker_entry(state), LegendEntry::Worker(state));
        }
        assert_eq!(worker_entry("hibernating"), LegendEntry::Worker(""));
        assert_eq!(worker_entry(""), LegendEntry::Worker(""));
        assert!(worker_state_label("hibernating").contains("unknown state"));
    }

    #[test]
    fn a_mark_means_the_same_whatever_the_finished_status_under_it() {
        for mark in [
            AgentMark::AwaitsReply,
            AgentMark::WaitsOnJob,
            AgentMark::WaitsOnIdleJob,
            AgentMark::BackgroundWork,
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
    fn a_swatch_shows_the_colour_the_glyph_is_drawn_in_and_no_colour_is_named() {
        let palette = Palette::catppuccin();
        let lines = legend_lines(StatusIndicatorStyle::Dots, true, &palette);
        let legend = text(&lines);
        assert!(legend.contains("animation on"), "{legend}");
        for name in ["(yellow)", "(teal)", "(red)", "(mauve)", "(grey)"] {
            assert!(!legend.contains(name), "{name}\n{legend}");
        }
        for (status, mark) in AGENT_STATES {
            let line = lines
                .iter()
                .find(|line| line.entry == Some(LegendEntry::Agent(status, mark)))
                .expect("agent entry");
            let color = agent_color(status, mark, &palette);
            assert_eq!(line.line.spans[0].style.fg, Some(color));
            assert_eq!(line.line.spans[1].content, "  ");
            assert_eq!(line.line.spans[1].style.bg, Some(color));
        }
    }

    #[test]
    fn counts_are_explained_by_example_and_no_planned_glyph_is_listed() {
        let legend = text(&legend_lines(
            StatusIndicatorStyle::Shapes,
            false,
            &Palette::catppuccin(),
        ));
        assert!(legend.contains("!2"), "{legend}");
        assert!(legend.contains("2 failed"), "{legend}");
        assert!(legend.contains("↑4  4 commits ahead"), "{legend}");
        // `±` is not drawn anywhere yet.
        assert!(!legend.contains('±'), "{legend}");
    }

    #[test]
    fn every_line_fits_the_popup_unwrapped() {
        let palette = Palette::catppuccin();
        for style in STYLES {
            let lines = legend_lines(style, true, &palette);
            let too_wide = lines
                .iter()
                .filter(|line| line.width > LEGEND_TEXT_WIDTH)
                .map(|line| format!("{}: {}", line.width, line.line))
                .collect::<Vec<_>>();
            assert!(too_wide.is_empty(), "{}", too_wide.join("\n"));
            for line in lines {
                assert_eq!(
                    line.width,
                    super::super::render::display_width(&line.line.to_string()) as usize,
                    "{}",
                    line.line
                );
            }
        }
    }
}
