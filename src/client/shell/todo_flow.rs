//! A space's TODO flow overlay: the space's TODO runs on the driver's state
//! machine (preflight to done), opened from the `⋔` button on a hovered
//! space's name line or the space's right-click menu.
//!
//! Driven by events only. The pushed snapshot's `todo_runs` give each run's
//! current (step, status); each snapshot that changes it appends one entry
//! to the run's observed path here (client presentation state, keyed by
//! run id). Steps between two snapshots are never invented: an entry that
//! does not follow its predecessor on a drawn edge is marked unobserved.
//! A user action (opening, selecting a run, Enter) sends one `todo.status`
//! for the selected run, the request a run line's details already use; its
//! reply carries the run's event cursor and, when no newer snapshot came
//! since it was sent, its durable step. The client never fetches on events
//! or timers.
//!
//! A newly appended step pulses for [`PULSE_FRAMES`] frames of the client's
//! existing motion clock (`ui::motion`), counted in `tick_motion`; the
//! overlay asks for frames only while it is open and a pulse is left.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::render::{display_width, put_text};
use super::*;
use crate::api::schema::{Method, TodoRunTarget};
use crate::protocol::ClientShellTodoRun;

/// The glyph of the button that opens the overlay (a pitchfork, so it does
/// not pass for the jobs' fold arrow).
pub(super) const TODO_FLOW_GLYPH: &str = "⋔";

/// The label of the space menu's item that opens the overlay.
pub(super) const TODO_FLOW_MENU_LABEL: &str = "TODO flow";

/// Motion frames a newly observed step pulses for (a count of the motion
/// clock's frames, not a time of its own).
const PULSE_FRAMES: u8 = 6;

/// The most attempts a run has (`TodoRunInfo::attempt`).
const MAX_ATTEMPTS: u32 = 3;

/// A state of the TODO driver (`TodoStep`'s names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum DriverStep {
    Preflight,
    Draft,
    Start,
    Attention,
    Review,
    Stop,
    Verify,
    Restart,
    CherryPick,
    Install,
    Todo,
    Push,
    Cleanup,
    Done,
    Abort,
}

impl DriverStep {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Draft => "draft",
            Self::Start => "start",
            Self::Attention => "attention",
            Self::Review => "review",
            Self::Stop => "stop",
            Self::Verify => "verify",
            Self::Restart => "restart",
            Self::CherryPick => "cherry_pick",
            Self::Install => "install",
            Self::Todo => "todo",
            Self::Push => "push",
            Self::Cleanup => "cleanup",
            Self::Done => "done",
            Self::Abort => "abort",
        }
    }

    /// The step the API names `name`; none for a name this client does not
    /// know.
    pub(super) fn from_api(name: &str) -> Option<Self> {
        Some(match name {
            "preflight" => Self::Preflight,
            "draft" => Self::Draft,
            "start" => Self::Start,
            "attention" => Self::Attention,
            "review" => Self::Review,
            "stop" => Self::Stop,
            "verify" => Self::Verify,
            "restart" => Self::Restart,
            "cherry_pick" | "cherry-pick" => Self::CherryPick,
            "install" => Self::Install,
            "todo" => Self::Todo,
            "push" => Self::Push,
            "cleanup" => Self::Cleanup,
            "done" => Self::Done,
            "abort" => Self::Abort,
            _ => return None,
        })
    }
}

/// One observed (step, status) of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ObservedStep {
    /// The step as the server named it.
    pub(super) step_name: String,
    pub(super) step: Option<DriverStep>,
    pub(super) status: String,
    pub(super) attempt: u32,
    /// Steps may have passed unobserved before this one.
    pub(super) gap_before: bool,
}

/// The one `todo.status` fetch of a run, sent on a user action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FetchState {
    NotFetched,
    Pending { generation: u64 },
    Fetched,
    Failed(String),
    Unsupported,
}

/// A run of the space as the overlay knows it.
#[derive(Clone, Debug)]
pub(super) struct RunFlow {
    pub(super) run_id: String,
    pub(super) item: String,
    pub(super) path: Vec<ObservedStep>,
    /// The run's durable event cursor from the last fetch.
    pub(super) event_cursor: Option<i64>,
    pub(super) fetch: FetchState,
    /// The newest `updated_ms` a fetch applied.
    fetched_updated_ms: Option<u64>,
    /// A snapshot changed the run after the pending fetch was sent, so its
    /// reply is older than what is shown.
    changed_since_fetch: bool,
    /// Motion frames the last appended step still pulses for.
    pub(super) pulse: u8,
    /// The snapshot still lists the run.
    pub(super) listed: bool,
    /// What the snapshot says beyond the step: its command, stop reason.
    pub(super) note: Option<String>,
}

impl RunFlow {
    fn from_snapshot(run: &ClientShellTodoRun) -> Self {
        let step = DriverStep::from_api(&run.step);
        Self {
            run_id: run.run_id.clone(),
            item: run.item.clone(),
            path: vec![ObservedStep {
                step_name: run.step.clone(),
                step,
                status: run.status.clone(),
                attempt: run.attempt,
                // A run first seen past its first step: what came before
                // was not observed.
                gap_before: step != Some(DriverStep::Preflight),
            }],
            event_cursor: None,
            fetch: FetchState::NotFetched,
            fetched_updated_ms: None,
            changed_since_fetch: false,
            pulse: 0,
            listed: true,
            note: run_note(run),
        }
    }

    pub(super) fn current(&self) -> Option<&ObservedStep> {
        self.path.last()
    }

    /// Appends (step, status) when it differs from the last one; whether it
    /// did.
    fn observe(&mut self, step_name: &str, status: &str, attempt: u32) -> bool {
        if self
            .current()
            .is_some_and(|last| last.step_name == step_name && last.status == status)
        {
            if let Some(last) = self.path.last_mut() {
                last.attempt = attempt;
            }
            return false;
        }
        let step = DriverStep::from_api(step_name);
        let gap_before = match (self.current().and_then(|last| last.step), step) {
            (Some(from), Some(to)) => !follows(from, to),
            _ => true,
        };
        self.path.push(ObservedStep {
            step_name: step_name.to_owned(),
            step,
            status: status.to_owned(),
            attempt,
            gap_before,
        });
        true
    }

    fn running(&self) -> bool {
        self.listed
            && self
                .current()
                .is_some_and(|step| matches!(step.status.as_str(), "running" | "waiting"))
    }
}

fn run_note(run: &ClientShellTodoRun) -> Option<String> {
    if let Some(reason) = run.stop_reason.as_deref() {
        return Some(format!(
            "stopped: {}",
            reason.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    if run.between_runs {
        return Some("between runs: its coordinator starts the next one".to_owned());
    }
    run.step_command
        .as_deref()
        .filter(|_| run.status == "running")
        .map(|command| format!("runs {command}"))
}

/// Whether `to` directly follows `from` in the driver: a drawn edge, the
/// same step again (an answer, a retried draft, install, TODO edit or
/// push), an abort, or a transition the graph does not draw: a skipped
/// draft, a worker started straight from the preflight, a retry from
/// review, a branch that moved since the approval (reviewed again), and a
/// verify again of the integrated tree after the target branch's head moved
/// or the owner approved the landing.
fn follows(from: DriverStep, to: DriverStep) -> bool {
    use DriverStep::*;
    from == to
        || to == Abort
        || matches!(
            (from, to),
            (Preflight, Start)
                | (Preflight, Attention)
                | (Review, Restart)
                | (Verify | CherryPick, Review)
                | (CherryPick, Verify)
        )
        || EDGES.iter().any(|(a, b, _)| (*a, *b) == (from, to))
}

/// The overlay of one space.
#[derive(Debug)]
pub(super) struct TodoFlowOverlay {
    pub(super) workspace_id: String,
    pub(super) space_name: String,
    /// The space's runs, oldest first.
    pub(super) runs: Vec<RunFlow>,
    pub(super) selected: usize,
}

impl TodoFlowOverlay {
    /// The overlay of `workspace_id` with the runs `snapshot` lists for it,
    /// the newest running one selected, else the newest.
    pub(super) fn new(snapshot: Option<&ClientShellSnapshot>, workspace_id: String) -> Self {
        let space_name = snapshot
            .and_then(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == workspace_id)
                    .map(|workspace| workspace.label.clone())
            })
            .unwrap_or_else(|| workspace_id.clone());
        let mut overlay = Self {
            workspace_id,
            space_name,
            runs: Vec::new(),
            selected: 0,
        };
        if let Some(snapshot) = snapshot {
            overlay.observe_snapshot(snapshot, false);
        }
        overlay.selected = overlay
            .runs
            .iter()
            .rposition(RunFlow::running)
            .unwrap_or(overlay.runs.len().saturating_sub(1));
        overlay
    }

    pub(super) fn selected_run(&self) -> Option<&RunFlow> {
        self.runs.get(self.selected)
    }

    /// Folds a new snapshot in: new runs, changed (step, status) appended,
    /// runs no longer listed kept and marked. Whether anything changed.
    pub(super) fn observe_snapshot(
        &mut self,
        snapshot: &ClientShellSnapshot,
        animate: bool,
    ) -> bool {
        let mut changed = false;
        for run in &snapshot.todo_runs {
            if super::todo_runs::run_space(snapshot, run) != Some(self.workspace_id.as_str()) {
                continue;
            }
            match self
                .runs
                .iter()
                .position(|known| known.run_id == run.run_id)
            {
                Some(index) => {
                    let selected = index == self.selected;
                    let flow = &mut self.runs[index];
                    let note = run_note(run);
                    if flow.note != note || !flow.listed {
                        flow.note = note;
                        flow.listed = true;
                        changed = true;
                    }
                    if flow.observe(&run.step, &run.status, run.attempt) {
                        changed = true;
                        if matches!(flow.fetch, FetchState::Pending { .. }) {
                            flow.changed_since_fetch = true;
                        }
                        if selected && animate {
                            flow.pulse = PULSE_FRAMES;
                        }
                    }
                }
                None => {
                    self.runs.push(RunFlow::from_snapshot(run));
                    changed = true;
                }
            }
        }
        for flow in &mut self.runs {
            let listed = snapshot
                .todo_runs
                .iter()
                .any(|run| run.run_id == flow.run_id);
            if flow.listed && !listed {
                flow.listed = false;
                changed = true;
            }
        }
        changed
    }

    /// Selects the run `delta` away, wrapping; whether the selection moved.
    pub(super) fn select(&mut self, delta: isize) -> bool {
        let count = self.runs.len();
        if count < 2 {
            return false;
        }
        for flow in &mut self.runs {
            flow.pulse = 0;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        true
    }

    /// Applies the reply to the fetch `generation` of `run_id`; whether it
    /// changed what is shown. Another run's, an older fetch's, or a reply
    /// older than the shown state never overwrites it.
    pub(super) fn apply_status(
        &mut self,
        run_id: &str,
        generation: u64,
        result: Result<&crate::api::schema::TodoRunInfo, String>,
    ) -> bool {
        let Some(flow) = self.runs.iter_mut().find(|flow| flow.run_id == run_id) else {
            return false;
        };
        if flow.fetch != (FetchState::Pending { generation }) {
            return false;
        }
        let run = match result {
            Ok(run) if run.run_id == run_id => run,
            Ok(_) => return false,
            Err(message) => {
                flow.fetch = FetchState::Failed(message);
                flow.changed_since_fetch = false;
                return true;
            }
        };
        flow.fetch = FetchState::Fetched;
        let newer = flow
            .fetched_updated_ms
            .is_none_or(|seen| run.updated_ms >= seen);
        if newer {
            flow.fetched_updated_ms = Some(run.updated_ms);
            flow.event_cursor = run.pending_event.or(run.last_acked_seq);
            // A step or status this client's schema does not know (a newer
            // server's) parsed as `unknown`: the snapshot names it as the
            // server sent it, so it stays the shown one.
            let known = run.step != crate::api::schema::TodoStep::Unknown
                && run.status != crate::api::schema::TodoRunStatus::Unknown;
            if !flow.changed_since_fetch && known {
                let name = |value: serde_json::Result<serde_json::Value>| {
                    value
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "unknown".to_owned())
                };
                let step = name(serde_json::to_value(run.step));
                let status = name(serde_json::to_value(run.status));
                flow.observe(&step, &status, run.attempt);
            }
        }
        flow.changed_since_fetch = false;
        true
    }

    /// Whether the overlay needs motion frames: the shown run pulses.
    pub(super) fn animates(&self) -> bool {
        self.selected_run().is_some_and(|flow| flow.pulse > 0)
    }

    /// One frame of the motion clock passed; whether the picture changed.
    pub(super) fn motion_frame(&mut self) -> bool {
        let Some(flow) = self.runs.get_mut(self.selected) else {
            return false;
        };
        if flow.pulse == 0 {
            return false;
        }
        flow.pulse -= 1;
        true
    }

    fn mark(&self, step: DriverStep) -> NodeMark {
        let Some(flow) = self.selected_run() else {
            return NodeMark::Pending;
        };
        let Some((current, before)) = flow.path.split_last() else {
            return NodeMark::Pending;
        };
        if current.step == Some(step) {
            return NodeMark::of_status(&current.status);
        }
        if let Some(last) = before.iter().rev().find(|seen| seen.step == Some(step)) {
            return if matches!(last.status.as_str(), "blocked" | "aborted") {
                NodeMark::Failed
            } else {
                NodeMark::Done
            };
        }
        NodeMark::Pending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeMark {
    Done,
    Current,
    Waiting,
    Failed,
    Pending,
    /// A status this client does not know (a newer server's).
    Unknown,
}

impl NodeMark {
    /// The mark of the current step in `status` (`TodoRunStatus`'s names).
    fn of_status(status: &str) -> Self {
        match status {
            "running" => Self::Current,
            "waiting" => Self::Waiting,
            "blocked" | "aborted" => Self::Failed,
            "done" => Self::Done,
            _ => Self::Unknown,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Done => "✓",
            // The turning half circle while the motion clock runs.
            Self::Current => crate::ui::motion::working_glyph(),
            Self::Waiting => "◆",
            Self::Failed => "✖",
            Self::Pending => "○",
            Self::Unknown => "?",
        }
    }

    fn color(self, p: &Palette) -> ratatui::style::Color {
        match self {
            Self::Done => p.green,
            Self::Current => p.accent,
            Self::Waiting => p.yellow,
            Self::Failed => p.red,
            Self::Pending => p.overlay0,
            Self::Unknown => p.subtext0,
        }
    }
}

/// Between two path entries whose steps were not seen in between.
const UNOBSERVED: &str = "┄ unobserved ┄";

// --- layout -----------------------------------------------------------------

/// Columns and rows the graph takes.
const GRAPH_WIDTH: u16 = 70;
const GRAPH_HEIGHT: u16 = 9;
/// Title, gap, graph, gap, five inspector lines, footer.
const FULL_HEIGHT: u16 = 2 + GRAPH_HEIGHT + 1 + 5 + 1;
/// Columns of an inspector line's key.
const KEY_WIDTH: u16 = 8;

/// Where each node's glyph starts, relative to the graph's corner. The
/// snake: the worker loop on the first rows (its worker runs and asks at
/// attention), the verify of the integrated tree and its restart edge
/// below, then the landing from cherry_pick (the verified tree onto the
/// target branch) to done.
const NODES: &[(DriverStep, u16, u16)] = {
    use DriverStep::*;
    &[
        (Attention, 26, 0),
        (Preflight, 0, 2),
        (Draft, 15, 2),
        (Start, 26, 2),
        (Review, 37, 2),
        (Stop, 49, 2),
        (Restart, 32, 4),
        (Verify, 49, 4),
        (CherryPick, 0, 6),
        (Install, 17, 6),
        (Todo, 30, 6),
        (Push, 40, 6),
        (Cleanup, 50, 6),
        (Done, 63, 6),
        (Abort, 63, 8),
    ]
};

/// The edges, as points from a cell next to the source to one next to the
/// target; the cells strictly between them are drawn.
/// An edge: its source, its target and its points.
type Edge = (DriverStep, DriverStep, &'static [(u16, u16)]);

const EDGES: &[Edge] = {
    use DriverStep::*;
    &[
        (Preflight, Draft, &[(11, 2), (14, 2)]),
        (Draft, Start, &[(22, 2), (25, 2)]),
        (Start, Attention, &[(26, 2), (26, 0)]),
        (Attention, Review, &[(36, 0), (37, 0), (37, 2)]),
        (Review, Stop, &[(45, 2), (48, 2)]),
        (Stop, Verify, &[(49, 2), (49, 4)]),
        (Verify, Restart, &[(48, 4), (45, 4)]),
        (Restart, Start, &[(32, 4), (26, 4), (26, 2)]),
        (Verify, CherryPick, &[(49, 4), (49, 5), (0, 5), (0, 6)]),
        (CherryPick, Install, &[(13, 6), (16, 6)]),
        (Install, Install, &[(18, 6), (18, 7), (22, 7), (22, 6)]),
        (Install, Todo, &[(26, 6), (29, 6)]),
        (Todo, Push, &[(36, 6), (39, 6)]),
        (Push, Cleanup, &[(46, 6), (49, 6)]),
        (Cleanup, Done, &[(59, 6), (62, 6)]),
    ]
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

fn dir(from: (u16, u16), to: (u16, u16)) -> Dir {
    if to.0 > from.0 {
        Dir::Right
    } else if to.0 < from.0 {
        Dir::Left
    } else if to.1 < from.1 {
        Dir::Up
    } else {
        Dir::Down
    }
}

/// The cells of an edge with their glyphs: lines, corners, and an arrow
/// head on the last cell.
fn edge_cells(points: &[(u16, u16)]) -> Vec<((u16, u16), &'static str)> {
    let mut path = Vec::new();
    for pair in points.windows(2) {
        let (mut x, mut y) = pair[0];
        if path.is_empty() {
            path.push((x, y));
        }
        while (x, y) != pair[1] {
            match dir((x, y), pair[1]) {
                Dir::Right => x += 1,
                Dir::Left => x -= 1,
                Dir::Up => y -= 1,
                Dir::Down => y += 1,
            }
            path.push((x, y));
        }
    }
    let mut cells = Vec::new();
    for i in 1..path.len().saturating_sub(1) {
        let incoming = dir(path[i - 1], path[i]);
        let outgoing = dir(path[i], path[i + 1]);
        let glyph = if i + 2 == path.len() {
            match outgoing {
                Dir::Right => "▸",
                Dir::Left => "◂",
                Dir::Up => "▴",
                Dir::Down => "▾",
            }
        } else {
            match (incoming, outgoing) {
                (Dir::Left | Dir::Right, Dir::Left | Dir::Right) => "─",
                (Dir::Up | Dir::Down, Dir::Up | Dir::Down) => "│",
                (Dir::Right, Dir::Down) | (Dir::Up, Dir::Left) => "┐",
                (Dir::Right, Dir::Up) | (Dir::Down, Dir::Left) => "┘",
                (Dir::Left, Dir::Down) | (Dir::Up, Dir::Right) => "┌",
                (Dir::Left, Dir::Up) | (Dir::Down, Dir::Right) => "└",
            }
        };
        cells.push((path[i], glyph));
    }
    cells
}

// --- rendering ----------------------------------------------------------------

/// Draws the overlay: the graph with an inspector when it fits, else the
/// selected run as a card. Never outside `b.area`.
pub(super) fn render_todo_flow(
    b: &mut Buffer,
    o: &TodoFlowOverlay,
    p: &Palette,
) -> Option<super::render::OverlayRender> {
    let outer = super::render::popup_area(b.area, GRAPH_WIDTH + 26, FULL_HEIGHT + 3)?;
    let inner = super::render::panel_area(b, outer, p.accent, p.panel_bg)?;
    let base = Style::default()
        .bg(p.panel_bg)
        .fg(p.text)
        .remove_modifier(Modifier::DIM | Modifier::BOLD);
    let dim = base.fg(p.overlay0);
    put_text(
        b,
        inner.x,
        inner.y,
        inner.width,
        &format!("TODO flow · {}", o.space_name),
        base.add_modifier(Modifier::BOLD),
    );
    let footer_y = inner.bottom().saturating_sub(1);
    let full = inner.width >= GRAPH_WIDTH + 2 && inner.height >= FULL_HEIGHT;
    let lines = inspector_lines(o);
    if o.runs.is_empty() {
        let text = [
            "No TODO runs in this space.",
            "Start one from its coordinator: herdr todo run <item-id> --task <file> …",
            "or with the space's crown button when ui.sidebar.spaces.todo_command is set.",
        ];
        for (i, line) in text.iter().enumerate() {
            let y = inner.y + 2 + i as u16;
            if y >= footer_y {
                break;
            }
            put_text(
                b,
                inner.x,
                y,
                inner.width,
                line,
                if i == 0 { base } else { dim },
            );
        }
    } else if full {
        let gx = inner.x + (inner.width - GRAPH_WIDTH) / 2;
        let gy = inner.y + 2;
        render_graph(b, o, gx, gy, base, p);
        let top = gy + GRAPH_HEIGHT + 1;
        draw_lines(b, &lines, gx, top, inner.right(), footer_y, base, dim);
    } else if inner.height > 2 {
        let mut y = inner.y + 2;
        if let Some(current) = o.selected_run().and_then(RunFlow::current) {
            if y < footer_y {
                let mark = current
                    .step
                    .map(|step| o.mark(step))
                    .unwrap_or_else(|| NodeMark::of_status(&current.status));
                put_text(
                    b,
                    inner.x,
                    y,
                    1,
                    mark.glyph(),
                    base.fg(mark.color(p)).add_modifier(Modifier::BOLD),
                );
                put_text(
                    b,
                    inner.x + 2,
                    y,
                    inner.width.saturating_sub(2),
                    &current.step_name,
                    base.add_modifier(Modifier::BOLD),
                );
                y += 1;
            }
        }
        draw_lines(b, &lines, inner.x, y, inner.right(), footer_y, base, dim);
    }
    let (primary, next, close) = render_footer(b, o, inner, footer_y, base, p);
    Some(super::render::OverlayRender {
        area: outer,
        primary,
        clear: next,
        cancel: close,
        ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)] // one block of lines and its two styles
fn draw_lines(
    b: &mut Buffer,
    lines: &[(&'static str, String)],
    x: u16,
    top: u16,
    right: u16,
    bottom: u16,
    base: Style,
    dim: Style,
) {
    for (i, (key, value)) in lines.iter().enumerate() {
        let y = top + i as u16;
        if y >= bottom {
            break;
        }
        let key_width = KEY_WIDTH.min(right.saturating_sub(x));
        put_text(b, x, y, key_width, key, dim);
        let value_x = x + key_width;
        put_text(b, value_x, y, right.saturating_sub(value_x), value, base);
    }
}

fn inspector_lines(o: &TodoFlowOverlay) -> Vec<(&'static str, String)> {
    let Some(flow) = o.selected_run() else {
        return Vec::new();
    };
    let current = flow.current();
    let mut run = format!("{} · item {}", flow.run_id, flow.item);
    if o.runs.len() > 1 {
        run.push_str(&format!(" · {}/{} runs", o.selected + 1, o.runs.len()));
    }
    if !flow.listed {
        run.push_str(" · no longer listed");
    }
    let step = current
        .map(|step| {
            format!(
                "{} · {} · attempt {}/{MAX_ATTEMPTS}",
                step.step_name, step.status, step.attempt
            )
        })
        .unwrap_or_default();
    // The newest entries, the oldest left out first.
    let mut path = String::new();
    for (i, seen) in flow.path.iter().enumerate() {
        if seen.gap_before {
            path.push_str(if i == 0 { "" } else { " " });
            path.push_str(UNOBSERVED);
            path.push(' ');
        } else if i > 0 {
            path.push_str(" → ");
        }
        path.push_str(&seen.step_name);
        if seen.status != "running" {
            path.push_str(&format!(" ({})", seen.status));
        }
    }
    let events = match &flow.fetch {
        FetchState::NotFetched => "not fetched · Enter fetches its todo.status".to_owned(),
        FetchState::Pending { .. } => "fetching todo.status…".to_owned(),
        FetchState::Fetched => match flow.event_cursor {
            Some(cursor) => format!("fetched · event cursor {cursor} · snapshots on top"),
            None => "fetched · no event yet · snapshots on top".to_owned(),
        },
        FetchState::Failed(message) => format!("fetch failed: {message}"),
        FetchState::Unsupported => "this server does not advertise todo.status".to_owned(),
    };
    let mut lines = vec![
        ("run", run),
        ("step", step),
        ("path", path),
        ("events", events),
    ];
    if let Some(note) = &flow.note {
        lines.push(("note", note.clone()));
    }
    lines
}

fn render_graph(b: &mut Buffer, o: &TodoFlowOverlay, gx: u16, gy: u16, base: Style, p: &Palette) {
    let flow = o.selected_run();
    let pulse_on = flow.is_some_and(|flow| flow.pulse % 2 == 1);
    // The edge into the current step, when it was observed.
    let incoming = flow.and_then(|flow| match flow.path.as_slice() {
        [.., from, to] if !to.gap_before => from.step.zip(to.step),
        _ => None,
    });
    for (from, to, points) in EDGES {
        let is_incoming = incoming == Some((*from, *to));
        let style = if is_incoming && pulse_on {
            base.fg(p.panel_bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else if is_incoming {
            base.fg(p.accent).add_modifier(Modifier::BOLD)
        } else {
            base.fg(p.surface1)
        };
        for ((x, y), glyph) in edge_cells(points) {
            put_text(b, gx + x, gy + y, 1, glyph, style);
        }
    }
    let current = flow.and_then(RunFlow::current);
    let attempt = current.map(|step| step.attempt).unwrap_or(1);
    for (step, x, y) in NODES {
        let mark = o.mark(*step);
        let name = if *step == DriverStep::Restart {
            format!("restart {attempt}/{MAX_ATTEMPTS}")
        } else {
            step.label().to_owned()
        };
        let is_current = current.is_some_and(|seen| seen.step == Some(*step));
        put_text(
            b,
            gx + x,
            gy + y,
            1,
            mark.glyph(),
            base.fg(mark.color(p)).add_modifier(Modifier::BOLD),
        );
        let style = match mark {
            _ if is_current && pulse_on => base
                .fg(p.panel_bg)
                .bg(mark.color(p))
                .add_modifier(Modifier::BOLD),
            NodeMark::Pending => base.fg(p.overlay0),
            NodeMark::Done if !is_current => base.fg(p.subtext0),
            _ => base.add_modifier(Modifier::BOLD),
        };
        put_text(b, gx + x + 2, gy + y, display_width(&name), &name, style);
        // A current step reached without its way there observed.
        if is_current && current.is_some_and(|seen| seen.gap_before) && *x >= 2 {
            put_text(b, gx + x - 2, gy + y, 1, "┄", base.fg(p.peach));
        }
    }
    put_text(b, gx + 49, gy + 8, 14, "╌ any step ╌▸", base.fg(p.surface1));
    // A current step this client does not know (a newer server's): said
    // under the graph instead of marking none of its nodes.
    if let Some(seen) = current.filter(|seen| seen.step.is_none()) {
        let mark = NodeMark::of_status(&seen.status);
        put_text(
            b,
            gx,
            gy + 8,
            1,
            mark.glyph(),
            base.fg(mark.color(p)).add_modifier(Modifier::BOLD),
        );
        put_text(
            b,
            gx + 2,
            gy + 8,
            46,
            &format!("{} (a step this client does not draw)", seen.step_name),
            base.add_modifier(Modifier::BOLD),
        );
    }
}

/// The footer's buttons (fetch, next run, close) and the key hints.
fn render_footer(
    b: &mut Buffer,
    o: &TodoFlowOverlay,
    inner: Rect,
    y: u16,
    base: Style,
    p: &Palette,
) -> (Rect, Rect, Rect) {
    if y <= inner.y {
        return Default::default();
    }
    let button_style = Style::default()
        .fg(p.panel_bg)
        .bg(p.accent)
        .add_modifier(Modifier::BOLD);
    let quiet_button = base.bg(p.surface1);
    let mut x = inner.x;
    let mut place = |b: &mut Buffer, label: &str, style: Style| {
        let width = display_width(label);
        if x + width > inner.right() {
            return Rect::default();
        }
        let rect = Rect::new(x, y, width, 1);
        put_text(b, x, y, width, label, style);
        x += width + 1;
        rect
    };
    let primary = if o.runs.is_empty() {
        Rect::default()
    } else {
        place(b, " ↵ fetch ", button_style)
    };
    let next = if o.runs.len() > 1 {
        place(b, " next run ", quiet_button)
    } else {
        Rect::default()
    };
    let close = place(b, " close ", quiet_button);
    put_text(
        b,
        x,
        y,
        inner.right().saturating_sub(x),
        "←/→ tab run · enter fetch · esc close · ┄ unobserved",
        base.fg(p.overlay0),
    );
    (primary, next, close)
}

// --- state ----------------------------------------------------------------------

impl ClientShellState {
    /// Opens the flow of the space `workspace_id`, fetching its selected
    /// run once (a user action).
    pub(super) fn open_todo_flow(&mut self, workspace_id: String, outcome: &mut ClientShellInput) {
        let overlay = TodoFlowOverlay::new(self.snapshot.as_deref(), workspace_id);
        self.overlay = Some(ClientShellOverlay::TodoFlow(overlay));
        self.fetch_todo_flow_run(outcome);
        outcome.repaint = true;
    }

    /// Sends one `todo.status` for the selected run, as a run line's
    /// details do; nothing while another is in flight.
    pub(super) fn fetch_todo_flow_run(&mut self, outcome: &mut ClientShellInput) {
        let Some(run_id) = (match self.overlay.as_ref() {
            Some(ClientShellOverlay::TodoFlow(o)) => o.selected_run().map(|r| r.run_id.clone()),
            _ => None,
        }) else {
            return;
        };
        let method = Method::TodoStatus(TodoRunTarget {
            run_id: run_id.clone(),
        });
        let supported = self.supports_endpoint_method(&method);
        let busy = self
            .pending_requests
            .values()
            .any(|pending| matches!(pending.kind, PendingEndpointKind::TodoStatus { .. }));
        let generation = self.next_request_id;
        let state = if !supported {
            FetchState::Unsupported
        } else if busy {
            return;
        } else if self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::TodoStatus {
                run_id: run_id.clone(),
                generation,
            },
            outcome,
        ) {
            FetchState::Pending { generation }
        } else {
            FetchState::Failed("the request could not be sent".to_owned())
        };
        if let Some(ClientShellOverlay::TodoFlow(o)) = self.overlay.as_mut() {
            if let Some(flow) = o.runs.iter_mut().find(|flow| flow.run_id == run_id) {
                flow.fetch = state;
                flow.changed_since_fetch = false;
            }
        }
        outcome.repaint = true;
    }

    /// The reply to a `todo.status` the open flow sent.
    pub(super) fn complete_todo_flow_status(
        &mut self,
        run_id: &str,
        generation: u64,
        result: &Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(ClientShellOverlay::TodoFlow(o)) = self.overlay.as_mut() else {
            return false;
        };
        let result = match result {
            Ok(crate::api::schema::ResponseResult::TodoRun { run }) => Ok(run),
            Ok(_) => Err("the server returned unexpected run details".to_owned()),
            Err(error) => Err(error.message.clone()),
        };
        o.apply_status(run_id, generation, result)
    }

    /// Folds the new snapshot into the open flow.
    pub(super) fn observe_todo_flow_snapshot(&mut self) {
        let animate = self.config.animations;
        if let (Some(ClientShellOverlay::TodoFlow(o)), Some(snapshot)) =
            (self.overlay.as_mut(), self.snapshot.as_deref())
        {
            o.observe_snapshot(snapshot, animate);
        }
    }

    /// Whether the open flow needs motion frames.
    pub(super) fn todo_flow_animates(&self) -> bool {
        matches!(&self.overlay, Some(ClientShellOverlay::TodoFlow(o)) if o.animates())
    }

    /// A motion frame passed: the open flow's pulse counts down.
    pub(super) fn todo_flow_motion_frame(&mut self) -> bool {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::TodoFlow(o)) => o.motion_frame(),
            _ => false,
        }
    }

    pub(super) fn todo_flow_key(&mut self, code: KeyCode, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::TodoFlow(o)) = self.overlay.as_mut() else {
            return;
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => {
                if o.select(-1) {
                    self.fetch_todo_flow_run(outcome);
                }
            }
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                if o.select(1) {
                    self.fetch_todo_flow_run(outcome);
                }
            }
            KeyCode::Enter => self.fetch_todo_flow_run(outcome),
            _ => return,
        }
        outcome.repaint = true;
    }

    pub(super) fn todo_flow_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) {
        if !matches!(
            mouse.kind,
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
        ) {
            return;
        }
        let point = (mouse.column, mouse.row);
        if !super::contains(self.hits.overlay_area, point)
            || super::contains(self.hits.overlay_cancel, point)
        {
            self.overlay = None;
        } else if super::contains(self.hits.overlay_primary, point) {
            self.fetch_todo_flow_run(outcome);
        } else if super::contains(self.hits.overlay_clear, point) {
            if let Some(ClientShellOverlay::TodoFlow(o)) = self.overlay.as_mut() {
                if o.select(1) {
                    self.fetch_todo_flow_run(outcome);
                }
            }
        } else {
            return;
        }
        outcome.repaint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(run_id: &str, step: &str, status: &str) -> ClientShellTodoRun {
        ClientShellTodoRun {
            run_id: run_id.into(),
            repo: "/repo".into(),
            item: "t-aaaaaaaa".into(),
            step: step.into(),
            status: status.into(),
            attempt: 1,
            workspace_id: Some("w1".into()),
            owner_pane_id: None,
            step_started_ms: None,
            step_command: None,
            has_log: false,
            stop_reason: None,
            between_runs: false,
        }
    }

    fn snapshot_with(runs: Vec<ClientShellTodoRun>) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        let workspace = |id: &str, label: &str| ClientShellWorkspace {
            workspace_id: id.into(),
            label: label.into(),
            new_workspace_cwd: format!("/elsewhere/{id}"),
            ..snapshot.workspaces[0].clone()
        };
        snapshot.workspaces = vec![workspace("w1", "herdr"), workspace("w2", "other")];
        snapshot.todo_runs = runs;
        snapshot
    }

    fn info(
        run_id: &str,
        step: &str,
        status: &str,
        updated_ms: u64,
    ) -> crate::api::schema::TodoRunInfo {
        serde_json::from_value(serde_json::json!({
            "run_id": run_id, "repo": "/repo", "item": "t-aaaaaaaa",
            "step": step, "status": status, "attempt": 2,
            "task": "task", "message": "message", "paths": [], "checks": [],
            "created_ms": 1000, "updated_ms": updated_ms, "last_acked_seq": 12,
        }))
        .unwrap()
    }

    fn names(flow: &RunFlow) -> Vec<(String, bool)> {
        flow.path
            .iter()
            .map(|seen| {
                (
                    format!("{}:{}", seen.step_name, seen.status),
                    seen.gap_before,
                )
            })
            .collect()
    }

    #[test]
    fn only_the_spaces_runs_are_shown_and_the_newest_running_one_is_selected() {
        let mut elsewhere = run("r-other", "start", "running");
        elsewhere.workspace_id = Some("w2".into());
        let snapshot = snapshot_with(vec![
            run("r-old", "done", "done"),
            run("r-live", "review", "waiting"),
            elsewhere,
            run("r-new", "done", "done"),
        ]);
        let o = TodoFlowOverlay::new(Some(&snapshot), "w1".into());
        assert_eq!(o.space_name, "herdr");
        let ids: Vec<_> = o.runs.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["r-old", "r-live", "r-new"]);
        assert_eq!(o.selected_run().unwrap().run_id, "r-live");
        // None running: the newest.
        let snapshot = snapshot_with(vec![
            run("r-old", "done", "done"),
            run("r-new", "done", "done"),
        ]);
        let o = TodoFlowOverlay::new(Some(&snapshot), "w1".into());
        assert_eq!(o.selected_run().unwrap().run_id, "r-new");
    }

    #[test]
    fn snapshot_changes_append_and_skipped_steps_are_marked_unobserved() {
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-1", "preflight", "running")])),
            "w1".into(),
        );
        let step = |o: &mut TodoFlowOverlay, step: &str, status: &str| {
            o.observe_snapshot(&snapshot_with(vec![run("r-1", step, status)]), true)
        };
        assert!(step(&mut o, "start", "running"));
        assert!(!step(&mut o, "start", "running"), "no change, no entry");
        assert!(step(&mut o, "start", "waiting"));
        // Review and stop were never seen.
        assert!(step(&mut o, "verify", "running"));
        assert_eq!(
            names(&o.runs[0]),
            [
                ("preflight:running".to_owned(), false),
                ("start:running".to_owned(), false),
                ("start:waiting".to_owned(), false),
                ("verify:running".to_owned(), true),
            ]
        );
        assert_eq!(o.runs[0].pulse, PULSE_FRAMES);
        // First seen mid-run: what came before was not observed.
        let o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-2", "install", "running")])),
            "w1".into(),
        );
        assert!(o.runs[0].path[0].gap_before);
        // A run that disappears stays, marked.
        let mut o = o;
        assert!(o.observe_snapshot(&snapshot_with(Vec::new()), true));
        assert!(!o.runs[0].listed);
    }

    #[test]
    fn a_fetch_reply_sets_the_cursor_and_state_but_a_stale_one_is_ignored() {
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-1", "start", "running")])),
            "w1".into(),
        );
        o.runs[0].fetch = FetchState::Pending { generation: 5 };
        // Another fetch's reply, another run's: ignored.
        assert!(!o.apply_status("r-1", 4, Ok(&info("r-1", "review", "waiting", 2000))));
        assert!(!o.apply_status("r-1", 5, Ok(&info("r-x", "review", "waiting", 2000))));
        assert!(o.apply_status("r-1", 5, Ok(&info("r-1", "review", "waiting", 2000))));
        assert_eq!(o.runs[0].fetch, FetchState::Fetched);
        assert_eq!(o.runs[0].event_cursor, Some(12));
        assert_eq!(o.runs[0].current().unwrap().step_name, "review");
        assert_eq!(o.runs[0].current().unwrap().attempt, 2);

        // A snapshot newer than the fetch arrives first: the reply keeps
        // only its cursor.
        o.runs[0].fetch = FetchState::Pending { generation: 6 };
        o.observe_snapshot(&snapshot_with(vec![run("r-1", "verify", "running")]), false);
        assert!(o.apply_status("r-1", 6, Ok(&info("r-1", "stop", "running", 3000))));
        assert_eq!(o.runs[0].current().unwrap().step_name, "verify");
        // An older reply than the last applied one changes nothing shown.
        o.runs[0].fetch = FetchState::Pending { generation: 7 };
        assert!(o.apply_status("r-1", 7, Ok(&info("r-1", "start", "running", 1000))));
        assert_eq!(o.runs[0].current().unwrap().step_name, "verify");
        // A failure is said.
        o.runs[0].fetch = FetchState::Pending { generation: 8 };
        assert!(o.apply_status("r-1", 8, Err("gone".into())));
        assert_eq!(o.runs[0].fetch, FetchState::Failed("gone".into()));
    }

    #[test]
    fn frames_are_wanted_only_while_the_shown_run_pulses() {
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-1", "start", "running")])),
            "w1".into(),
        );
        assert!(!o.animates(), "nothing appended yet");
        o.observe_snapshot(&snapshot_with(vec![run("r-1", "review", "waiting")]), true);
        assert!(o.animates());
        for _ in 0..PULSE_FRAMES {
            assert!(o.motion_frame());
        }
        assert!(!o.animates());
        assert!(!o.motion_frame());
        // Without animations nothing pulses.
        o.observe_snapshot(&snapshot_with(vec![run("r-1", "stop", "running")]), false);
        assert!(!o.animates());
    }

    #[test]
    fn a_closed_overlay_wants_no_frames_and_ignores_snapshots() {
        let mut state = ClientShellState::new(super::super::tests::config_with_sidebar_width(30));
        state.set_snapshot(Box::new(snapshot_with(vec![run(
            "r-1", "start", "running",
        )])));
        let mut outcome = ClientShellInput::default();
        state.open_todo_flow("w1".into(), &mut outcome);
        state.set_snapshot(Box::new(snapshot_with(vec![run(
            "r-1", "review", "running",
        )])));
        assert!(state.todo_flow_animates() == state.config.animations);
        state.todo_flow_key(KeyCode::Esc, &mut outcome);
        assert!(state.overlay.is_none());
        assert!(!state.todo_flow_animates());
        assert!(!state.todo_flow_motion_frame());
    }

    #[test]
    fn the_landing_order_is_followed_without_gaps() {
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-1", "preflight", "running")])),
            "w1".into(),
        );
        // The worker runs and asks at attention, then the approval stops it,
        // the integrated tree is verified, landed, installed, recorded,
        // pushed and cleaned up; a moved head is verified again.
        for (step, status) in [
            ("start", "running"),
            ("attention", "running"),
            ("attention", "waiting"),
            ("review", "waiting"),
            ("stop", "running"),
            ("verify", "running"),
            ("cherry_pick", "running"),
            ("verify", "running"),
            ("cherry_pick", "running"),
            ("install", "running"),
            ("install", "waiting"),
            ("todo", "running"),
            ("push", "running"),
            ("cleanup", "running"),
            ("done", "done"),
        ] {
            o.observe_snapshot(&snapshot_with(vec![run("r-1", step, status)]), false);
        }
        let gaps: Vec<_> = names(&o.runs[0])
            .into_iter()
            .filter(|(_, gap)| *gap)
            .collect();
        assert!(gaps.is_empty(), "{gaps:?}");
        // An abort restoring the branch follows any step.
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-2", "preflight", "running")])),
            "w1".into(),
        );
        o.observe_snapshot(&snapshot_with(vec![run("r-2", "push", "running")]), false);
        o.observe_snapshot(&snapshot_with(vec![run("r-2", "abort", "running")]), false);
        o.observe_snapshot(&snapshot_with(vec![run("r-2", "abort", "aborted")]), false);
        assert_eq!(
            names(&o.runs[0]),
            [
                ("preflight:running".to_owned(), false),
                ("push:running".to_owned(), true),
                ("abort:running".to_owned(), false),
                ("abort:aborted".to_owned(), false),
            ]
        );
    }

    #[test]
    fn an_unknown_step_or_status_from_a_newer_server_is_shown_neutrally() {
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-1", "stop", "running")])),
            "w1".into(),
        );
        // A step this client does not know: kept by its name, after a gap.
        o.observe_snapshot(
            &snapshot_with(vec![run("r-1", "land_later", "paused")]),
            false,
        );
        let current = o.runs[0].current().unwrap();
        assert_eq!(current.step, None);
        assert_eq!(current.step_name, "land_later");
        assert!(current.gap_before);
        assert_eq!(NodeMark::of_status("paused"), NodeMark::Unknown);
        let text = render(&o, 100, 30);
        assert!(
            text.contains("? land_later (a step this client does not draw)"),
            "{text}"
        );
        assert!(text.contains("land_later · paused"), "{text}");
        assert!(!text.contains("◆"), "no node is marked current: {text}");
        let text = render(&o, 60, 18);
        assert!(text.contains("? land_later"), "{text}");
        for (w, h) in [(0, 0), (8, 5), (75, 21)] {
            render(&o, w, h);
        }

        // A fetch reply whose step or status parsed as `unknown` keeps the
        // snapshot's names; its cursor still applies.
        o.runs[0].fetch = FetchState::Pending { generation: 1 };
        assert!(o.apply_status("r-1", 1, Ok(&info("r-1", "land_later", "paused", 2000))));
        assert_eq!(o.runs[0].current().unwrap().step_name, "land_later");
        assert_eq!(o.runs[0].event_cursor, Some(12));
        o.observe_snapshot(&snapshot_with(vec![run("r-1", "verify", "running")]), false);
        o.runs[0].fetch = FetchState::Pending { generation: 2 };
        assert!(o.apply_status("r-1", 2, Ok(&info("r-1", "push", "paused", 3000))));
        assert_eq!(o.runs[0].current().unwrap().step_name, "verify");
    }

    #[test]
    fn edges_have_corners_and_arrow_heads() {
        let cells = edge_cells(&[(32, 4), (26, 4), (26, 2)]);
        let glyphs: String = cells.iter().map(|(_, g)| *g).collect();
        assert_eq!(glyphs, "─────└▴");
        assert_eq!(cells.last().map(|(at, _)| *at), Some((26, 3)));
    }

    fn render(o: &TodoFlowOverlay, width: u16, height: u16) -> String {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        render_todo_flow(&mut buffer, o, &Palette::catppuccin());
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn verify_run() -> TodoFlowOverlay {
        let mut o = TodoFlowOverlay::new(
            Some(&snapshot_with(vec![run("r-1", "stop", "running")])),
            "w1".into(),
        );
        let mut verifying = run("r-1", "verify", "waiting");
        verifying.attempt = 2;
        o.observe_snapshot(&snapshot_with(vec![verifying]), false);
        o
    }

    #[test]
    fn renders_the_graph_and_inspector_at_100_by_30() {
        let text = render(&verify_run(), 100, 30);
        assert!(text.contains("TODO flow · herdr"), "{text}");
        assert!(text.contains("◆ verify"), "{text}");
        assert!(text.contains("✓ stop"), "{text}");
        assert!(text.contains("○ cherry_pick"), "{text}");
        assert!(text.contains("restart 2/3"), "{text}");
        assert!(text.contains("unobserved"), "{text}");
        assert!(text.contains("esc close"), "{text}");
    }

    #[test]
    fn renders_a_card_when_small_an_empty_state_and_never_panics() {
        let o = verify_run();
        let text = render(&o, 60, 18);
        assert!(text.contains("TODO flow · herdr"), "{text}");
        assert!(text.contains("◆ verify"), "{text}");
        assert!(text.contains("attempt 2/3"), "{text}");
        assert!(text.contains("close"), "{text}");
        assert!(!text.contains("cherry_pick"), "no graph: {text}");
        let empty = TodoFlowOverlay::new(Some(&snapshot_with(Vec::new())), "w1".into());
        for (w, h) in [(100, 30), (60, 18)] {
            let text = render(&empty, w, h);
            assert!(text.contains("No TODO runs in this space"), "{text}");
        }
        for (w, h) in [(0, 0), (1, 1), (4, 4), (8, 5), (20, 6), (75, 21), (200, 60)] {
            render(&o, w, h);
            render(&empty, w, h);
        }
    }
}
