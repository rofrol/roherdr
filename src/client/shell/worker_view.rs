//! A headless worker's tab: a click on its line under its space shows its
//! transcript in the main pane, like an agent's tab, instead of a modal.
//! The server pushes the transcript (`client_shell.worker_transcript.set`,
//! then `endpoint.worker-transcript.v1`); the lines are the ones `herdr
//! worker log` prints ([`crate::workers::entry_lines`]). It scrolls, selects
//! and copies, follows new output only while at the bottom (else a "new
//! output" mark shows), and takes no input: a header says it is read-only.
//! Focusing any other tab, or the server focusing one (a takeover's tab),
//! leaves it.

use crossterm::event::{MouseButton, MouseEventKind};
use unicode_width::UnicodeWidthChar;

use super::*;
use crate::api::schema::{
    ClientShellWorkerTranscriptSetParams, Method, WorkerState, WorkerTab, WorkerTranscriptEntry,
    WorkerTranscriptEvent, WorkerTranscriptRole,
};

/// Rows a wheel notch scrolls.
const WHEEL_ROWS: usize = 3;

/// What a row of the transcript is, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowKind {
    User,
    Assistant,
    Result,
    ToolCall,
    ToolResult,
    ToolError,
    Status,
}

/// One screen row of the transcript, wrapped to the view's width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ViewRow {
    pub(super) text: String,
    pub(super) kind: RowKind,
}

/// A selection in transcript rows and columns, from where the press was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ViewSelection {
    pub(super) anchor: (usize, u16),
    pub(super) cursor: (usize, u16),
    pub(super) dragging: bool,
}

impl ViewSelection {
    fn ordered(&self) -> ((usize, u16), (usize, u16)) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    fn covers(&self, row: usize, col: u16) -> bool {
        let (start, end) = self.ordered();
        (row, col) >= start && (row, col) <= end
    }
}

pub(crate) struct ClientWorkerView {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) worker_id: String,
    /// The tab focused when the view opened: once another one is, the
    /// user (or the server) went elsewhere and the view closes.
    pub(super) focused_tab_id: Option<String>,
    pub(super) tab: Option<WorkerTab>,
    pub(super) state: Option<WorkerState>,
    pub(super) run_id: Option<String>,
    pub(super) events: Vec<WorkerTranscriptEvent>,
    /// The journal lines received: what the server is asked to go on from.
    pub(super) cursor: u64,
    /// Rows above the bottom the view shows; 0 follows the tail.
    pub(super) offset_from_bottom: usize,
    /// Output arrived while the view was scrolled up.
    pub(super) new_output: bool,
    /// The connection (boot id, generation) the transcript was asked of; a
    /// reconnect asks the new one from `cursor` on.
    pub(super) subscribed: Option<(String, Option<u64>)>,
    pub(super) selection: Option<ViewSelection>,
    /// The rows at the width they were wrapped to, and how many events
    /// they hold.
    rows: Vec<ViewRow>,
    rows_width: u16,
    rows_events: usize,
}

impl ClientWorkerView {
    pub(super) fn new(
        endpoint_id: ClientEndpointId,
        worker_id: String,
        focused_tab_id: Option<String>,
    ) -> Self {
        Self {
            endpoint_id,
            worker_id,
            focused_tab_id,
            tab: None,
            state: None,
            run_id: None,
            events: Vec::new(),
            cursor: 0,
            offset_from_bottom: 0,
            new_output: false,
            subscribed: None,
            selection: None,
            rows: Vec::new(),
            rows_width: 0,
            rows_events: 0,
        }
    }

    /// The rows wrapped to `width`, rebuilt only for new events or a new
    /// width.
    pub(super) fn rows(&mut self, width: u16) -> &[ViewRow] {
        if self.rows_width != width || self.rows_events > self.events.len() {
            self.rows.clear();
            self.rows_events = 0;
            self.rows_width = width;
        }
        for event in &self.events[self.rows_events..] {
            let kind = row_kind(&event.entry);
            for line in crate::workers::entry_lines(&event.entry) {
                for text in wrap(&line, width) {
                    self.rows.push(ViewRow { text, kind });
                }
            }
        }
        self.rows_events = self.events.len();
        &self.rows
    }

    /// Takes a pushed transcript: lines it already has are skipped, so a
    /// resend after a reconnect does not repeat them. Returns whether it
    /// changed what shows.
    pub(super) fn receive(&mut self, transcript: crate::api::schema::WorkerTranscript) -> bool {
        if transcript.worker_id != self.worker_id {
            return false;
        }
        let mut changed = self.tab.as_ref() != Some(&transcript.tab)
            || self.state != Some(transcript.state)
            || self.run_id != transcript.run_id;
        self.tab = Some(transcript.tab);
        self.state = Some(transcript.state);
        self.run_id = transcript.run_id;
        let fresh: Vec<_> = transcript
            .events
            .into_iter()
            .filter(|event| event.line > self.cursor)
            .collect();
        self.cursor = self.cursor.max(transcript.cursor);
        if fresh.is_empty() {
            return changed;
        }
        changed = true;
        let rows_before = (self.rows_width > 0).then(|| self.rows(self.rows_width).len());
        self.events.extend(fresh);
        if self.offset_from_bottom > 0 {
            // Scrolled up: what shows stays where it is.
            if let Some(before) = rows_before {
                let after = self.rows(self.rows_width).len();
                self.offset_from_bottom += after.saturating_sub(before);
            }
            self.new_output = true;
        }
        changed
    }

    /// Scrolls by `delta` rows (negative is up), within `max`.
    pub(super) fn scroll(&mut self, delta: isize, max: usize) -> bool {
        let before = self.offset_from_bottom;
        self.offset_from_bottom = if delta < 0 {
            before.saturating_add(delta.unsigned_abs()).min(max)
        } else {
            before.saturating_sub(delta.unsigned_abs())
        };
        if self.offset_from_bottom == 0 {
            self.new_output = false;
        }
        self.offset_from_bottom != before
    }

    /// The first transcript row a body of `height` rows shows, and how far
    /// up it can scroll.
    pub(super) fn window(&self, total: usize, height: usize) -> (usize, usize) {
        let max = total.saturating_sub(height);
        let offset = self.offset_from_bottom.min(max);
        (max - offset, max)
    }

    /// The selected text, rows joined by newlines, trailing blanks cut.
    pub(super) fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let ((start_row, start_col), (end_row, end_col)) = selection.ordered();
        if (start_row, start_col) == (end_row, end_col) {
            return None;
        }
        let mut lines = Vec::new();
        for row in start_row..=end_row.min(self.rows.len().saturating_sub(1)) {
            let text = &self.rows.get(row)?.text;
            let from = if row == start_row { start_col } else { 0 };
            let to = if row == end_row { Some(end_col) } else { None };
            lines.push(columns(text, from, to).trim_end().to_owned());
        }
        Some(lines.join("\n"))
    }
}

fn row_kind(entry: &WorkerTranscriptEntry) -> RowKind {
    match entry {
        WorkerTranscriptEntry::Message { role, .. } => match role {
            WorkerTranscriptRole::User => RowKind::User,
            WorkerTranscriptRole::Result => RowKind::Result,
            WorkerTranscriptRole::Assistant | WorkerTranscriptRole::Unknown => RowKind::Assistant,
        },
        WorkerTranscriptEntry::ToolCall { .. } => RowKind::ToolCall,
        WorkerTranscriptEntry::ToolResult { is_error: true, .. } => RowKind::ToolError,
        WorkerTranscriptEntry::ToolResult { .. } => RowKind::ToolResult,
        WorkerTranscriptEntry::Status { .. } | WorkerTranscriptEntry::Unknown => RowKind::Status,
    }
}

/// `line` cut into rows of at most `width` columns; an empty line is one
/// empty row.
fn wrap(line: &str, width: u16) -> Vec<String> {
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

/// The part of `text` from column `from` through column `to` (all when
/// none).
fn columns(text: &str, from: u16, to: Option<u16>) -> String {
    let mut col = 0u16;
    let mut out = String::new();
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0) as u16;
        if col >= from && to.is_none_or(|to| col <= to) {
            out.push(ch);
        }
        col = col.saturating_add(width.max(1));
    }
    out
}

/// Draws a worker's tab over `area`, the main pane's place: a header with
/// the worker, its state and a read-only label, then its transcript.
/// Returns the tab's area and its body's, for the pointer.
pub(super) fn render_worker_view(
    view: &mut ClientWorkerView,
    worker: Option<&crate::protocol::ClientShellWorker>,
    palette: &Palette,
    buffer: &mut Buffer,
    area: Rect,
) -> (Rect, Rect) {
    if area.is_empty() {
        return (Rect::default(), Rect::default());
    }
    let base = Style::default().bg(palette.panel_bg).fg(palette.text);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            buffer[(x, y)].reset();
            buffer[(x, y)].set_symbol(" ").set_style(base);
        }
    }
    // The header: whose tab, its state, and that it takes no input.
    let state = worker
        .map(|worker| worker.state.clone())
        .or_else(|| {
            view.state.and_then(|state| {
                serde_json::to_value(state)
                    .ok()
                    .and_then(|state| state.as_str().map(str::to_owned))
            })
        })
        .unwrap_or_default();
    let (_, said, _) = super::space_tabs::worker_state(&state);
    let name = worker
        .map(|worker| worker.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("worker {}", view.worker_id));
    let label = " read-only ";
    let header = format!(
        " ⚒ {name} · {} · {said}{}",
        view.worker_id,
        view.run_id
            .as_deref()
            .map(|run| format!(" · {run}"))
            .unwrap_or_default()
    );
    let header_style = Style::default()
        .bg(palette.surface0)
        .fg(palette.text)
        .add_modifier(Modifier::BOLD);
    buffer.set_style(Rect::new(area.x, area.y, area.width, 1), header_style);
    let label_width = label.chars().count() as u16;
    let header_room = area.width.saturating_sub(label_width);
    buffer.set_stringn(
        area.x,
        area.y,
        &header,
        usize::from(header_room),
        header_style,
    );
    let read_only = view.tab.as_ref().is_none_or(|tab| tab.read_only);
    if read_only && area.width > label_width {
        buffer.set_string(
            area.right() - label_width,
            area.y,
            label,
            Style::default().bg(palette.overlay0).fg(palette.panel_bg),
        );
    }
    let body = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    if body.is_empty() {
        return (area, body);
    }
    let selection = view.selection;
    let total = view.rows(body.width).len();
    let (top, max) = view.window(total, usize::from(body.height));
    view.offset_from_bottom = view.offset_from_bottom.min(max);
    let new_output = view.new_output && view.offset_from_bottom > 0;
    let rows = view.rows(body.width);
    for (index, row) in rows
        .iter()
        .skip(top)
        .take(usize::from(body.height))
        .enumerate()
    {
        let y = body.y + index as u16;
        let style = base.fg(match row.kind {
            RowKind::User | RowKind::Assistant => palette.text,
            RowKind::Result => palette.green,
            RowKind::ToolCall => palette.accent,
            RowKind::ToolResult => palette.overlay1,
            RowKind::ToolError => palette.red,
            RowKind::Status => palette.yellow,
        });
        let style = if row.kind == RowKind::User {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        };
        buffer.set_stringn(body.x, y, &row.text, usize::from(body.width), style);
        if let Some(selection) = selection {
            for x in body.x..body.right() {
                if selection.covers(top + index, x - body.x) {
                    buffer[(x, y)].set_bg(palette.selection_bg);
                }
            }
        }
    }
    if total == 0 {
        buffer.set_stringn(
            body.x + 1,
            body.y,
            "no transcript yet",
            usize::from(body.width.saturating_sub(1)),
            base.fg(palette.overlay0),
        );
    }
    if new_output {
        let mark = " ↓ new output ";
        let width = (mark.chars().count() as u16).min(body.width);
        let x = body.x + (body.width - width) / 2;
        buffer.set_stringn(
            x,
            body.bottom() - 1,
            mark,
            usize::from(width),
            Style::default()
                .bg(palette.accent)
                .fg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
    }
    (area, body)
}

impl ClientShellState {
    /// The worker view shows on the active endpoint.
    pub(super) fn worker_view_shown(&self) -> bool {
        self.worker_view
            .as_ref()
            .is_some_and(|view| view.endpoint_id == self.active_endpoint_id)
    }

    /// The worker whose tab shows, for its line's highlight.
    pub(super) fn shown_worker_id(&self) -> Option<&str> {
        self.worker_view
            .as_ref()
            .filter(|view| view.endpoint_id == self.active_endpoint_id)
            .map(|view| view.worker_id.as_str())
    }

    fn worker_transcript_set(worker_id: Option<String>, after: u64) -> Method {
        Method::ClientShellWorkerTranscriptSet(ClientShellWorkerTranscriptSetParams {
            worker_id,
            after,
        })
    }

    /// Opens the worker's tab: its transcript in the main pane. A server
    /// without worker tabs opens its log popup instead.
    pub(super) fn open_worker_view(&mut self, worker_id: String, outcome: &mut ClientShellInput) {
        if !self.supports_endpoint_method(&Self::worker_transcript_set(None, 0)) {
            self.open_worker_log_popup(worker_id, outcome);
            return;
        }
        outcome.repaint = true;
        if self
            .worker_view
            .as_ref()
            .is_some_and(|view| view.worker_id == worker_id && self.worker_view_shown())
        {
            return;
        }
        let focused_tab_id = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.clone());
        self.selection = None;
        self.worker_view = Some(ClientWorkerView::new(
            self.active_endpoint_id.clone(),
            worker_id,
            focused_tab_id,
        ));
        self.subscribe_worker_view(outcome);
    }

    /// Asks the active connection for the shown worker's transcript from
    /// what the view has, unless it was asked already.
    fn subscribe_worker_view(&mut self, outcome: &mut ClientShellInput) {
        let Some(key) = self
            .snapshot
            .as_deref()
            .map(|snapshot| (snapshot.boot_id.clone(), self.active_snapshot_generation))
        else {
            return;
        };
        let Some(view) = self
            .worker_view
            .as_mut()
            .filter(|view| view.endpoint_id == self.active_endpoint_id)
        else {
            return;
        };
        if view.subscribed.as_ref() == Some(&key) {
            return;
        }
        let method = Self::worker_transcript_set(Some(view.worker_id.clone()), view.cursor);
        if self.push_endpoint_method_with_kind(method, PendingEndpointKind::Generic, outcome) {
            if let Some(view) = self.worker_view.as_mut() {
                view.subscribed = Some(key);
            }
        }
    }

    /// After a snapshot: a reconnect asks the new connection for the
    /// transcript from where the view is. Returns the request to send.
    pub(crate) fn worker_view_resubscription(&mut self) -> Vec<ClientShellAction> {
        let mut outcome = ClientShellInput::default();
        self.subscribe_worker_view(&mut outcome);
        outcome.actions
    }

    /// Leaves the worker's tab and stops its transcript.
    pub(super) fn close_worker_view(&mut self, outcome: &mut ClientShellInput) {
        let Some(view) = self.worker_view.take() else {
            return;
        };
        outcome.repaint = true;
        if view.endpoint_id == self.active_endpoint_id && view.subscribed.is_some() {
            self.push_endpoint_method(Self::worker_transcript_set(None, 0), outcome);
        }
    }

    /// A snapshot whose focused tab is not the one the view opened over:
    /// the focus went elsewhere, so the view gives way to that tab.
    pub(super) fn close_worker_view_on_focus_change(&mut self, focused_tab_id: Option<&str>) {
        if self.worker_view.as_ref().is_some_and(|view| {
            view.endpoint_id == self.active_endpoint_id
                && view.focused_tab_id.as_deref() != focused_tab_id
        }) {
            self.worker_view = None;
        }
    }

    /// A transcript the server pushed. Returns whether to repaint.
    pub(crate) fn receive_worker_transcript(
        &mut self,
        endpoint_id: &ClientEndpointId,
        projection: crate::protocol::endpoint::EndpointWorkerTranscript,
    ) -> bool {
        let Some(view) = self
            .worker_view
            .as_mut()
            .filter(|view| &view.endpoint_id == endpoint_id)
        else {
            return false;
        };
        if view
            .subscribed
            .as_ref()
            .is_some_and(|(boot_id, _)| *boot_id != projection.boot_id)
        {
            return false;
        }
        view.receive(projection.transcript) && endpoint_id == &self.active_endpoint_id
    }

    /// Scrolls the shown view by `delta` rows (negative is up).
    fn scroll_worker_view(&mut self, delta: isize) -> bool {
        let body = self.hits.worker_view_body;
        let Some(view) = self.worker_view.as_mut() else {
            return false;
        };
        let total = view.rows(body.width.max(1)).len();
        let (_, max) = view.window(total, usize::from(body.height));
        view.scroll(delta, max)
    }

    /// The keys the view takes (arrows, pages, Home and End scroll); others
    /// are not sent anywhere, since the tab takes no input.
    pub(super) fn worker_view_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        let page = (self.hits.worker_view_body.height.max(2) - 1) as isize;
        let changed = match key.code {
            KeyCode::Up => self.scroll_worker_view(-1),
            KeyCode::Down => self.scroll_worker_view(1),
            KeyCode::PageUp => self.scroll_worker_view(-page),
            KeyCode::PageDown => self.scroll_worker_view(page),
            KeyCode::Home => self.scroll_worker_view(isize::MIN / 2),
            KeyCode::End => self.scroll_worker_view(isize::MAX / 2),
            _ => false,
        };
        outcome.repaint |= changed;
    }

    /// The pointer over the shown view: the wheel scrolls, a drag selects
    /// and its release copies. Returns whether the view took the event.
    pub(super) fn worker_view_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.worker_view_shown() {
            return false;
        }
        let dragging = self
            .worker_view
            .as_ref()
            .and_then(|view| view.selection)
            .is_some_and(|selection| selection.dragging);
        if !super::contains(self.hits.worker_view, point) && !dragging {
            return false;
        }
        let body = self.hits.worker_view_body;
        let cell = |view: &mut ClientWorkerView| {
            let total = view.rows(body.width.max(1)).len();
            let (top, _) = view.window(total, usize::from(body.height));
            let row =
                top + usize::from(point.1.clamp(body.y, body.bottom().saturating_sub(1)) - body.y);
            let col = point.0.clamp(body.x, body.right().saturating_sub(1)) - body.x;
            (row.min(total.saturating_sub(1)), col)
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                outcome.repaint |= self.scroll_worker_view(-(WHEEL_ROWS as isize));
            }
            MouseEventKind::ScrollDown => {
                outcome.repaint |= self.scroll_worker_view(WHEEL_ROWS as isize);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(view) = self.worker_view.as_mut() {
                    let had = view.selection.take().is_some();
                    if super::contains(body, point) {
                        let at = cell(view);
                        view.selection = Some(ViewSelection {
                            anchor: at,
                            cursor: at,
                            dragging: true,
                        });
                    }
                    outcome.repaint |= had;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(view) = self.worker_view.as_mut() {
                    let at = cell(view);
                    if let Some(selection) = view.selection.as_mut().filter(|s| s.dragging) {
                        if selection.cursor != at {
                            selection.cursor = at;
                            outcome.repaint = true;
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let text = self.worker_view.as_mut().and_then(|view| {
                    let selection = view.selection.as_mut()?;
                    selection.dragging = false;
                    let text = view.selected_text();
                    if text.is_none() {
                        view.selection = None;
                    }
                    text
                });
                if let Some(text) = text {
                    outcome
                        .actions
                        .push(ClientShellAction::ClipboardWrite(text.into_bytes()));
                    self.show_copy_feedback(std::time::Instant::now());
                }
                outcome.repaint = true;
            }
            _ => {}
        }
        true
    }
}
