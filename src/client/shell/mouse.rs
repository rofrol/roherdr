use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

const SELECTION_AUTOSCROLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);
/// How often a space dragged to the list's top or bottom row scrolls the
/// list by a row.
const SPACE_DRAG_AUTOSCROLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(60);
const SELECTION_REPAINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

impl ClientShellState {
    /// Middle click on a sidebar workspace or a tab closes it through the same
    /// confirmation path as the context menu's Close.
    fn close_chrome_target_at(&mut self, point: (u16, u16), outcome: &mut ClientShellInput) {
        if !self.config.mouse_capture {
            return;
        }
        if self.on_gone_square(point) {
            return;
        }
        // A tab line lies inside its space's block but closes only its tab.
        if let Some(tab_id) = self.space_tab_at(point) {
            self.request_tab_close(tab_id, outcome);
            outcome.repaint = true;
            return;
        }
        let workspace_id = (!self.sidebar_collapsed)
            .then(|| self.active_endpoint_workspace_at(point))
            .flatten();
        if let Some(workspace_id) = workspace_id {
            self.request_workspace_close(workspace_id, None, outcome);
            outcome.repaint = true;
            return;
        }
        let tab_id = self
            .hits
            .tabs
            .iter()
            .chain(&self.hits.child_tabs)
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, tab_id)| tab_id.clone());
        if let Some(tab_id) = tab_id {
            self.request_tab_close(tab_id, outcome);
            outcome.repaint = true;
        }
    }

    /// The tab whose line or square under a space is at `point`: a line
    /// stands for its top-level tab, a square for its nested tab.
    fn space_tab_at(&self, point: (u16, u16)) -> Option<String> {
        if self.sidebar_collapsed {
            return None;
        }
        self.hits
            .space_tab_squares
            .iter()
            .chain(&self.hits.space_tabs)
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, tab_id)| tab_id.clone())
    }

    /// Client chrome (or the legacy PTY footer with vertical tabs):
    /// ` ← ` in its first three columns goes back to the parent
    /// tab and ` × ` in its last three closes the job tab (a running job asks
    /// first). Returns whether the click was one of them.
    fn job_footer_click(&mut self, point: (u16, u16), outcome: &mut ClientShellInput) -> bool {
        const BUTTON_WIDTH: u16 = 3;
        let client_footer = self.active_job_metadata().is_some();
        if (!client_footer && !self.config.spaces.tabs)
            || !self.config.mouse_capture
            || self.overlay.is_some()
            || self.mode != ClientShellMode::Terminal
        {
            return false;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        let Some(job) = snapshot
            .focused_tab_id
            .as_deref()
            .and_then(|id| snapshot.tabs.iter().find(|tab| tab.tab_id == id))
            .filter(|tab| client_footer || tab.status.is_some())
        else {
            return false;
        };
        let parent = job.parent_tab_id.clone();
        let job_id = job.tab_id.clone();
        let inner = if client_footer {
            if !super::contains(self.hits.job_footer, point) {
                return false;
            }
            self.hits.job_footer
        } else {
            // The footer is drawn into the job's last row, which a full-screen
            // program (vim, less) uses itself: its click is the program's.
            let alternate_screen = |pane_id: &str| {
                self.pane_surface.as_ref().is_some_and(|surface| {
                    surface
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == pane_id && pane.alternate_screen_active)
                })
            };
            let Some(hit) = self.hits.panes.iter().find(|hit| {
                !hit.popup
                    && point.1 == hit.inner_rect.bottom().saturating_sub(1)
                    && super::contains(hit.inner_rect, point)
                    && !alternate_screen(&hit.pane_id)
                    && snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == hit.pane_id && pane.tab_id == job_id)
            }) else {
                return false;
            };
            hit.inner_rect
        };
        if inner.width < BUTTON_WIDTH * 2 {
            return false;
        }
        if point.0 < inner.x.saturating_add(BUTTON_WIDTH) {
            let Some(parent) = parent else {
                return client_footer;
            };
            self.push_endpoint_method(
                crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                    tab_id: parent,
                }),
                outcome,
            );
            return true;
        }
        if point.0 >= inner.right().saturating_sub(BUTTON_WIDTH) {
            self.request_tab_close(job_id, outcome);
            outcome.repaint = true;
            return true;
        }
        false
    }

    /// How far one wheel event moves the spaces list, which scrolls by rows:
    /// one row, like the agents panel. The terminal already multiplies the
    /// events of a fast wheel or a trackpad flick.
    fn workspace_wheel_step(&self) -> usize {
        1
    }

    /// A closed job's blank slot, held while the pointer is over the list:
    /// clicks there do nothing, not even act on the space around it.
    fn on_gone_square(&self, point: (u16, u16)) -> bool {
        self.hits
            .space_tab_gone
            .iter()
            .any(|rect| super::contains(*rect, point))
    }

    /// The tab a left click on a tab line or square focuses, or none when
    /// the click folds or unfolds the line's squares (on its triangle and
    /// counts). A square opens its tab, and the open one goes back to its
    /// parent. The rest of a line always opens the tab itself.
    fn space_tab_click(&mut self, point: (u16, u16)) -> Option<Option<String>> {
        let hit = |hits: &[(Rect, String)]| {
            hits.iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .map(|(_, tab_id)| tab_id.clone())
        };
        if let Some(tab_id) = hit(&self.hits.space_tab_folds) {
            // Tab ids can be reused: forget tabs that are gone.
            let live = self
                .snapshot
                .as_deref()
                .map(|snapshot| {
                    snapshot
                        .tabs
                        .iter()
                        .map(|tab| tab.tab_id.clone())
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_default();
            let unfolded = self
                .unfolded_squares
                .entry(self.active_endpoint_id.clone())
                .or_default();
            unfolded.retain(|key| live.contains(key));
            if !unfolded.remove(&tab_id) {
                // Unfolding scrolls the list to show the squares.
                self.reveal_unfolded_tab = Some(tab_id.clone());
                unfolded.insert(tab_id.clone());
                // Unfolding unpins the job kept under the folded line; a
                // later fold shows none until the focus moves to a job.
                if let Some(kept) = self.kept_jobs.get_mut(&self.active_endpoint_id) {
                    kept.remove(&tab_id);
                }
            }
            self.persist_chrome_preferences(&mut ClientShellInput::default());
            return Some(None);
        }
        if self.on_gone_square(point) {
            return Some(None);
        }
        if let Some(square) = hit(&self.hits.space_tab_squares) {
            let snapshot = self.snapshot.as_deref()?;
            let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == square)?;
            let open = snapshot.focused_tab_id.as_deref() == Some(square.as_str());
            return Some(Some(match tab.parent_tab_id.as_ref() {
                Some(parent) if open => parent.clone(),
                _ => square,
            }));
        }
        hit(&self.hits.space_tabs).map(Some)
    }

    fn set_sidebar_width_from_column(&mut self, column: u16, outcome: &mut ClientShellInput) {
        let (min, max) = crate::config::validated_sidebar_bounds(
            self.config.sidebar_min_width,
            self.config.sidebar_max_width,
        )
        .unwrap_or((18, 36));
        let width = column.saturating_add(1).clamp(min, max);
        if self.sidebar_width != width {
            self.sidebar_width = width;
            self.sidebar_width_manual = true;
            self.invalidate_pane_surface();
            outcome.repaint = true;
            outcome.resize = true;
        }
    }

    fn set_sidebar_section_from_row(&mut self, row: u16, outcome: &mut ClientShellInput) {
        let sections = self.hits.sidebar_sections;
        if sections.height == 0 {
            return;
        }
        let ratio = row.saturating_sub(sections.y) as f32 / sections.height as f32;
        let ratio = ratio.clamp(0.1, 0.9);
        if (self.sidebar_section_split - ratio).abs() > f32::EPSILON {
            self.sidebar_section_split = ratio;
            self.sidebar_section_split_manual = true;
            outcome.repaint = true;
        }
    }

    fn pane_scrollbar_offset(
        hit: &PaneHit,
        row: u16,
        grab_row_offset: Option<u16>,
    ) -> Option<usize> {
        let track = hit.scrollbar_rect?;
        let metrics = hit.scroll?;
        (metrics.max_offset_from_bottom > 0).then(|| match grab_row_offset {
            Some(grab_row_offset) => {
                crate::ui::scrollbar_offset_from_drag_row(metrics, track, row, grab_row_offset)
            }
            None => crate::ui::scrollbar_offset_from_row(metrics, track, row),
        })
    }

    pub(super) fn push_pane_scroll_offset(
        &mut self,
        pane_id: String,
        offset_from_bottom: usize,
        outcome: &mut ClientShellInput,
    ) {
        self.pane_scroll_targets
            .insert(pane_id.clone(), offset_from_bottom);
        if self.pane_scroll_in_flight.contains_key(&pane_id) {
            self.pane_scroll_queued.insert(pane_id, offset_from_bottom);
            return;
        }
        self.dispatch_pane_scroll_offset(pane_id, offset_from_bottom, outcome);
    }

    fn dispatch_pane_scroll_offset(
        &mut self,
        pane_id: String,
        offset_from_bottom: usize,
        outcome: &mut ClientShellInput,
    ) {
        if self.snapshot.is_none() {
            return;
        }
        self.next_scroll_serial = self.next_scroll_serial.saturating_add(1);
        let serial = self.next_scroll_serial;
        self.pane_scroll_targets
            .insert(pane_id.clone(), offset_from_bottom);
        self.pane_scroll_in_flight.insert(pane_id.clone(), serial);
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::PaneScroll(crate::api::schema::PaneScrollParams {
                pane_id: pane_id.clone(),
                offset_from_bottom: offset_from_bottom as u64,
            }),
            PendingEndpointKind::PaneScroll {
                pane_id: pane_id.clone(),
                serial,
            },
            outcome,
        ) {
            self.pane_scroll_targets.remove(&pane_id);
            self.pane_scroll_in_flight.remove(&pane_id);
        }
    }

    pub(super) fn complete_pane_scroll(
        &mut self,
        pane_id: String,
        serial: u64,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if self.pane_scroll_in_flight.get(&pane_id).copied() != Some(serial) {
            return false;
        }
        self.pane_scroll_in_flight.remove(&pane_id);
        let repaint = match result {
            Ok(crate::api::schema::ResponseResult::PaneInfo { pane })
                if pane.pane_id == pane_id =>
            {
                if let Some(scroll) = pane.scroll {
                    if self.pane_scroll_targets.contains_key(&pane_id) {
                        self.pane_scroll_targets.insert(
                            pane_id.clone(),
                            usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX),
                        );
                    }
                }
                false
            }
            Ok(_) => {
                self.pane_scroll_queued.remove(&pane_id);
                self.pane_scroll_targets.remove(&pane_id);
                self.set_endpoint_error("endpoint returned an unexpected pane-scroll result");
                true
            }
            Err(_) => {
                self.pane_scroll_queued.remove(&pane_id);
                self.pane_scroll_targets.remove(&pane_id);
                true
            }
        };
        if let Some(offset) = self.pane_scroll_queued.remove(&pane_id) {
            self.dispatch_pane_scroll_offset(pane_id, offset, outcome);
        }
        repaint
    }

    pub(super) fn stop_selection_autoscroll(&mut self) {
        self.selection_autoscroll = None;
        self.selection_autoscroll_deadline = None;
    }

    fn selection_edge_scroll_lines(distance: u16) -> usize {
        usize::from(distance).saturating_mul(3).clamp(3, 15)
    }

    fn selection_scroll_metrics(&self, hit: &PaneHit) -> Option<crate::pane::ScrollMetrics> {
        let metrics = hit.scroll?;
        Some(
            self.selection_autoscroll
                .as_ref()
                .filter(|autoscroll| autoscroll.pane_id == hit.pane_id)
                .map_or(metrics, |autoscroll| crate::pane::ScrollMetrics {
                    offset_from_bottom: autoscroll.offset_from_bottom,
                    max_offset_from_bottom: autoscroll.max_offset_from_bottom,
                    viewport_rows: metrics.viewport_rows,
                }),
        )
    }

    fn active_selection_pane(&self) -> Option<PaneHit> {
        let pane_id = if let Some(gesture) = self.word_selection_gesture.as_ref() {
            if gesture.released {
                return None;
            }
            &gesture.pane_id
        } else {
            &self
                .selection
                .as_ref()
                .filter(|selection| selection.is_in_progress())?
                .pane_id
        };
        self.hits
            .panes
            .iter()
            .find(|hit| &hit.pane_id == pane_id)
            .cloned()
    }

    fn update_selection_cursor_with_metrics(
        &mut self,
        hit: &PaneHit,
        column: u16,
        row: u16,
        metrics: Option<crate::pane::ScrollMetrics>,
        outcome: &mut ClientShellInput,
    ) {
        if self.word_selection_gesture.is_some() {
            let viewport_row = row
                .saturating_sub(hit.inner_rect.y)
                .min(hit.inner_rect.height.saturating_sub(1));
            let col = column
                .saturating_sub(hit.inner_rect.x)
                .min(hit.inner_rect.width.saturating_sub(1));
            let absolute_row = crate::selection::absolute_row_for_viewport(viewport_row, metrics);
            self.drag_word_selection((absolute_row, col), outcome);
        } else if let Some(selection) = self.selection.as_mut() {
            selection.drag(column, row, hit.inner_rect, metrics);
        }
    }

    fn update_selection_drag(
        &mut self,
        hit: &PaneHit,
        column: u16,
        row: u16,
        outcome: &mut ClientShellInput,
    ) {
        let metrics = self.selection_scroll_metrics(hit);
        let was_dragging = self
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_dragging);
        let moved_from_anchor = self.selection.as_ref().is_some_and(|selection| {
            let (anchor_row, anchor_col) = selection.anchor_screen_pos(hit.inner_rect, metrics);
            anchor_row != row || anchor_col != column
        });
        self.update_selection_cursor_with_metrics(hit, column, row, metrics, outcome);
        let is_dragging = self
            .word_selection_gesture
            .as_ref()
            .map_or(was_dragging || moved_from_anchor, |gesture| gesture.dragged);
        if is_dragging {
            if let Some(selection) = self.selection.as_mut() {
                if selection.is_just_click() {
                    selection.force_dragging();
                }
            }
            self.last_pane_click = None;
        }
        if !is_dragging {
            self.stop_selection_autoscroll();
            return;
        }

        let Some(metrics) = metrics else {
            self.stop_selection_autoscroll();
            return;
        };
        let top = hit.inner_rect.y;
        let bottom = hit.inner_rect.y + hit.inner_rect.height.saturating_sub(1);
        let (direction, immediate_lines) = if row < top {
            (
                ClientSelectionAutoscrollDirection::Up,
                Self::selection_edge_scroll_lines(top - row),
            )
        } else if row > bottom {
            (
                ClientSelectionAutoscrollDirection::Down,
                Self::selection_edge_scroll_lines(row - bottom),
            )
        } else if row == top {
            (ClientSelectionAutoscrollDirection::Up, 0)
        } else if row == bottom {
            (ClientSelectionAutoscrollDirection::Down, 0)
        } else {
            self.stop_selection_autoscroll();
            return;
        };

        let offset_from_bottom = match direction {
            ClientSelectionAutoscrollDirection::Up => metrics
                .offset_from_bottom
                .saturating_add(immediate_lines)
                .min(metrics.max_offset_from_bottom),
            ClientSelectionAutoscrollDirection::Down => {
                metrics.offset_from_bottom.saturating_sub(immediate_lines)
            }
        };
        if offset_from_bottom != metrics.offset_from_bottom {
            let projected = crate::pane::ScrollMetrics {
                offset_from_bottom,
                ..metrics
            };
            self.update_selection_cursor_with_metrics(hit, column, row, Some(projected), outcome);
            self.push_pane_scroll_offset(hit.pane_id.clone(), offset_from_bottom, outcome);
        }
        self.selection_autoscroll = Some(ClientSelectionAutoscroll {
            pane_id: hit.pane_id.clone(),
            direction,
            last_mouse_column: column,
            last_mouse_row: row,
            inner_rect: hit.inner_rect,
            offset_from_bottom,
            max_offset_from_bottom: metrics.max_offset_from_bottom,
        });
        self.selection_autoscroll_deadline =
            Some(std::time::Instant::now() + SELECTION_AUTOSCROLL_INTERVAL);
    }

    fn scroll_in_progress_selection(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return false;
        }
        let Some(hit) = self.active_selection_pane() else {
            return false;
        };
        let Some(metrics) = self.selection_scroll_metrics(&hit) else {
            return false;
        };
        let offset_from_bottom = match mouse.kind {
            MouseEventKind::ScrollUp => metrics
                .offset_from_bottom
                .saturating_add(self.config.mouse_scroll_lines)
                .min(metrics.max_offset_from_bottom),
            MouseEventKind::ScrollDown => metrics
                .offset_from_bottom
                .saturating_sub(self.config.mouse_scroll_lines),
            _ => unreachable!(),
        };
        if offset_from_bottom != metrics.offset_from_bottom {
            let projected = crate::pane::ScrollMetrics {
                offset_from_bottom,
                ..metrics
            };
            self.update_selection_cursor_with_metrics(
                &hit,
                mouse.column,
                mouse.row,
                Some(projected),
                outcome,
            );
            self.push_pane_scroll_offset(hit.pane_id, offset_from_bottom, outcome);
            outcome.repaint = true;
        }
        true
    }

    pub(super) fn request_selection_drag_repaint(&mut self, now: std::time::Instant) -> bool {
        let deadline = self
            .last_composed_at
            .map(|last| last + SELECTION_REPAINT_INTERVAL);
        self.selection_repaint_deadline = deadline.filter(|deadline| now < *deadline);
        self.selection_repaint_deadline.is_none()
    }

    /// While a space is dragged on the list's top or bottom row (or past
    /// it), the list scrolls a row at a time, so the space can be dropped
    /// anywhere; elsewhere it stops.
    fn update_space_drag_autoscroll(&mut self, point: (u16, u16)) {
        let body = self.hits.workspace_body;
        let direction = if body.height < 2 || self.hits.workspace_layout.is_empty() {
            0
        } else if point.1 <= body.y {
            -1
        } else if point.1 >= body.bottom().saturating_sub(1) && point.1 <= body.bottom() {
            1
        } else {
            0
        };
        if direction == 0 {
            self.space_drag_autoscroll = None;
            return;
        }
        if self
            .space_drag_autoscroll
            .is_none_or(|(current, _, _)| current != direction)
        {
            self.space_drag_autoscroll = Some((
                direction,
                point,
                std::time::Instant::now() + SPACE_DRAG_AUTOSCROLL_INTERVAL,
            ));
        } else if let Some((_, last, _)) = self.space_drag_autoscroll.as_mut() {
            *last = point;
        }
    }

    /// One step of [`Self::update_space_drag_autoscroll`]: scrolls a row and
    /// retargets the drop with the pointer where it was.
    fn tick_space_drag_autoscroll(
        &mut self,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) {
        let Some((direction, point, deadline)) = self.space_drag_autoscroll else {
            return;
        };
        if now < deadline {
            return;
        }
        let Some(ClientChromeDrag::Workspace {
            source_workspace_id,
            grab_offset,
            ..
        }) = self.chrome_drag.as_ref()
        else {
            self.space_drag_autoscroll = None;
            return;
        };
        let (source, grab_offset) = (source_workspace_id.clone(), *grab_offset);
        let next = if direction < 0 {
            self.workspace_scroll.saturating_sub(1)
        } else {
            self.workspace_scroll
                .saturating_add(1)
                .min(self.hits.workspace_max_scroll)
        };
        if next == self.workspace_scroll {
            self.space_drag_autoscroll = None;
            return;
        }
        self.workspace_scroll = next;
        // The drawn layout moves with the list until the next frame.
        for layout in &mut self.hits.workspace_layout {
            layout.top -= i32::from(direction);
            layout.bottom -= i32::from(direction);
        }
        let target = self.workspace_drop_target_at(point, &source, grab_offset);
        if let Some(ClientChromeDrag::Workspace {
            target: current, ..
        }) = self.chrome_drag.as_mut()
        {
            *current = target;
        }
        self.space_drag_autoscroll = Some((direction, point, now + SPACE_DRAG_AUTOSCROLL_INTERVAL));
        outcome.repaint = true;
    }

    pub(crate) fn tick_selection_autoscroll(
        &mut self,
        now: std::time::Instant,
    ) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        self.tick_space_drag_autoscroll(now, &mut outcome);
        self.tick_tooltip(now, &mut outcome);
        if self
            .selection_repaint_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.selection_repaint_deadline = None;
            outcome.repaint = true;
        }
        if self
            .selection_autoscroll_deadline
            .is_none_or(|deadline| now < deadline)
        {
            return outcome;
        }
        let Some(mut autoscroll) = self.selection_autoscroll.clone() else {
            self.selection_autoscroll_deadline = None;
            return outcome;
        };
        let dragging = self.word_selection_gesture.as_ref().map_or_else(
            || {
                self.selection.as_ref().is_some_and(|selection| {
                    selection.pane_id == autoscroll.pane_id && selection.is_dragging()
                })
            },
            |gesture| gesture.pane_id == autoscroll.pane_id && gesture.dragged && !gesture.released,
        );
        if !dragging {
            self.stop_selection_autoscroll();
            return outcome;
        }
        let Some(hit) = self
            .hits
            .panes
            .iter()
            .find(|hit| hit.pane_id == autoscroll.pane_id)
            .cloned()
        else {
            self.stop_selection_autoscroll();
            return outcome;
        };
        if hit.inner_rect != autoscroll.inner_rect {
            self.stop_selection_autoscroll();
            return outcome;
        }
        let next_offset = match autoscroll.direction {
            ClientSelectionAutoscrollDirection::Up => autoscroll
                .offset_from_bottom
                .saturating_add(1)
                .min(autoscroll.max_offset_from_bottom),
            ClientSelectionAutoscrollDirection::Down => {
                autoscroll.offset_from_bottom.saturating_sub(1)
            }
        };
        if next_offset == autoscroll.offset_from_bottom {
            self.stop_selection_autoscroll();
            return outcome;
        }
        autoscroll.offset_from_bottom = next_offset;
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: next_offset,
            max_offset_from_bottom: autoscroll.max_offset_from_bottom,
            viewport_rows: hit.scroll.map_or(0, |metrics| metrics.viewport_rows),
        };
        self.update_selection_cursor_with_metrics(
            &hit,
            autoscroll.last_mouse_column,
            autoscroll.last_mouse_row,
            Some(metrics),
            &mut outcome,
        );
        self.push_pane_scroll_offset(autoscroll.pane_id.clone(), next_offset, &mut outcome);
        self.selection_autoscroll = Some(autoscroll);
        self.selection_autoscroll_deadline = Some(now + SELECTION_AUTOSCROLL_INTERVAL);
        outcome.repaint = true;
        outcome
    }

    fn pane_split_target_is_current(&self, hit: &PaneSplitHit, tab_id: &str) -> Option<bool> {
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        if snapshot.revision != surface.projection_revision {
            return None;
        }
        Some(
            snapshot.focused_tab_id.as_deref() == Some(tab_id)
                && pane_surface_topology_signature(surface) == hit.topology_signature,
        )
    }

    fn pane_split_ratio(hit: &PaneSplitHit, grab_offset: i32, point: (u16, u16)) -> f32 {
        let (pointer, origin, length) = match hit.direction {
            crate::protocol::PaneSurfaceSplitDirection::Horizontal => {
                (i32::from(point.0), i32::from(hit.area.x), hit.area.width)
            }
            crate::protocol::PaneSurfaceSplitDirection::Vertical => {
                (i32::from(point.1), i32::from(hit.area.y), hit.area.height)
            }
        };
        ((pointer + grab_offset - origin) as f32 / f32::from(length.max(1))).clamp(0.1, 0.9)
    }

    /// Wheel scrolling over the tab bar stops at the first and last tab
    /// instead of wrapping like the previous/next tab keybindings.
    /// The main-row tab `delta` steps from the active one; `None` past either
    /// end, so the wheel stops there. Children are skipped: they have their
    /// own row.
    fn main_row_step(&self, delta: isize) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        let active = super::tab_groups::active_main_tab_id(snapshot)?;
        let tabs = super::tab_groups::main_row_tabs(snapshot);
        let current = tabs.iter().position(|tab| tab.tab_id == active)?;
        let next = current.checked_add_signed(delta)?;
        tabs.get(next).map(|tab| self.group_entry_tab(&tab.tab_id))
    }

    /// The second-row entry `delta` steps from the focused one; `None` past
    /// either end, so the wheel stops there instead of leaving the group.
    fn child_row_step(&self, delta: isize) -> Option<String> {
        let entries = super::tab_groups::active_row_entries(self.snapshot.as_deref()?);
        let current = entries.iter().position(|tab| tab.focused)?;
        let next = current.checked_add_signed(delta)?;
        entries.get(next).map(|tab| tab.tab_id.clone())
    }

    /// A press on a tab line in the spaces list (not on its triangle, counts
    /// or squares). The tab opens on release, so a drag can start from it.
    fn space_tab_line_press(&self, mouse: &MouseEvent) -> Option<ClientTabPress> {
        // A filtered list hides tabs: no drag slots, so a press just opens.
        if self.space_filter.active() {
            return None;
        }
        let point = (mouse.column, mouse.row);
        let on =
            |hits: &[(Rect, String)]| hits.iter().any(|(rect, _)| super::contains(*rect, point));
        if on(&self.hits.space_tab_folds)
            || on(&self.hits.space_tab_squares)
            || self.on_gone_square(point)
        {
            return None;
        }
        let (_, tab_id) = self
            .hits
            .space_tabs
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))?;
        let tab = self
            .snapshot
            .as_deref()?
            .tabs
            .iter()
            .find(|tab| tab.tab_id == *tab_id)?;
        Some(ClientTabPress {
            tab_id: tab.tab_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            main_row: true,
            sidebar_line: true,
            start_column: mouse.column,
            start_row: mouse.row,
        })
    }

    /// The drawn tab lines of a space and the row below its block.
    fn tab_line_geometry(&self, workspace_id: &str) -> Option<TabLineGeometry> {
        let snapshot = self.snapshot.as_deref()?;
        let top_level = snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id && tab.parent_tab_id.is_none())
            .map(|tab| tab.tab_id.as_str())
            .collect::<Vec<_>>();
        let lines = self
            .hits
            .space_tabs
            .iter()
            .filter_map(|(rect, id)| {
                let index = top_level.iter().position(|tab| *tab == id.as_str())?;
                Some((index, i32::from(rect.y)))
            })
            .collect::<Vec<_>>();
        let last_row = lines.last()?.1;
        let bottom = self
            .hits
            .workspace_layout
            .iter()
            .find(|layout| layout.workspace_id == workspace_id)
            .map_or(last_row + 1, |layout| layout.bottom.max(last_row + 1));
        Some(TabLineGeometry {
            lines,
            bottom,
            top_level: top_level.into_iter().map(str::to_owned).collect(),
        })
    }

    /// Where a dragged tab line would land among its space's top-level tabs
    /// (counting the dragged one): the slot nearest to the dragged line's top,
    /// among the slots the others leave once it is lifted, as for a dragged
    /// space. `geometry` is the lines' rows when the drag started. None while
    /// the pointer is above the first drawn line or below the space: the drag
    /// then cancels on release and never clamps to the first or last slot.
    fn tab_line_drop_index_at(
        &self,
        point: (u16, u16),
        workspace_id: &str,
        tab_id: &str,
        geometry: &TabLineGeometry,
    ) -> Option<usize> {
        let snapshot = self.snapshot.as_deref()?;
        let top_level = snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id && tab.parent_tab_id.is_none())
            .map(|tab| tab.tab_id.as_str())
            .collect::<Vec<_>>();
        let TabLineGeometry {
            lines,
            bottom,
            top_level: started_with,
        } = geometry;
        if top_level
            .iter()
            .map(|id| id.to_string())
            .ne(started_with.iter().cloned())
        {
            return None;
        }
        let bottom = *bottom;
        let (first_index, first_row) = *lines.first()?;
        let (last_index, _) = *lines.last()?;
        let row = i32::from(point.1);
        let at_the_end = last_index + 1 == top_level.len();
        if row < first_row || (at_the_end && row >= bottom) {
            return None;
        }
        // The rows the dragged line's block (the line and its squares) frees.
        let lifted = lines
            .iter()
            .position(|(index, _)| top_level.get(*index) == Some(&tab_id))
            .map_or(0, |at| {
                lines.get(at + 1).map_or(bottom, |(_, next)| *next) - lines[at].1
            });
        let source = top_level.iter().position(|id| *id == tab_id);
        // Before each drawn line, and after the last one when that is the end.
        let slots = lines
            .iter()
            .map(|(index, line_row)| (*index, *line_row))
            .chain(at_the_end.then_some((last_index + 1, bottom)));
        slots
            .map(|(index, slot_row)| {
                let after_source = source.is_some_and(|source| index > source);
                let slot_row = slot_row - if after_source { lifted } else { 0 };
                (slot_row.abs_diff(row), index)
            })
            .min()
            .map(|(_, index)| index)
            .filter(|index| *index >= first_index)
    }

    /// Insert position among the main-row tabs (see `tab_groups::flat_insert_index`).
    fn tab_drop_index_at(&self, point: (u16, u16)) -> Option<usize> {
        let snapshot = self.snapshot.as_deref()?;
        let tabs = super::tab_groups::main_row_tabs(snapshot);
        let visible = self
            .hits
            .tabs
            .iter()
            .filter_map(|(rect, tab_id)| {
                tabs.iter()
                    .position(|tab| tab.tab_id == *tab_id)
                    .map(|index| (index, *rect))
            })
            .collect::<Vec<_>>();
        let (first_index, first_rect) = *visible.first()?;
        let (last_index, last_rect) = *visible.last()?;
        let on_tab_row = point.1 == first_rect.y;
        if !on_tab_row {
            return None;
        }
        if super::contains(self.hits.tab_scroll_left, point) {
            return Some(0);
        }
        if super::contains(self.hits.tab_scroll_right, point) {
            return Some(tabs.len());
        }
        let left_edge = if first_index == 0 {
            first_rect.x
        } else {
            self.hits.tab_scroll_left.right()
        };
        let right_edge = if last_index + 1 >= tabs.len() {
            last_rect.right()
        } else {
            self.hits.tab_scroll_right.x.saturating_sub(1)
        };
        if point.0 <= left_edge {
            return Some(first_index);
        }
        if point.0 >= right_edge {
            return Some(last_index + 1);
        }
        for (index, rect) in visible {
            let midpoint = rect.x + rect.width / 2;
            if point.0 < midpoint {
                return Some(index);
            }
            if point.0 < rect.right() {
                return Some(index + 1);
            }
        }
        Some(last_index + 1)
    }

    fn group_toggle_hit_is_main_space(&self, hit: &super::state::WorkspaceHit) -> bool {
        let snapshot = self.snapshot.as_deref();
        hit.group_toggle.as_ref().is_none_or(|(_, key)| {
            snapshot.is_none_or(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| {
                        workspace.worktree.as_ref().is_some_and(|worktree| {
                            worktree.key == *key && !worktree.is_linked_worktree
                        })
                    })
                    .is_some_and(|workspace| workspace.workspace_id == hit.workspace_id)
            })
        })
    }

    /// Top-level blocks of this endpoint's spaces: (id, top, bottom) in
    /// screen rows per space with its indented worktrees. The local sidebar
    /// scrolls by rows and gives every space, also those scrolled out of
    /// view (rows above the screen are negative); the multi-machine sidebar
    /// gives the drawn ones.
    fn workspace_blocks(&self) -> Vec<(String, i32, i32)> {
        let mut blocks = Vec::<(String, i32, i32)>::new();
        let mut push = |id: &str, indented: bool, top: i32, bottom: i32| match blocks.last_mut() {
            // A worktree space nested under one of the parent's tab lines
            // ends above the parent's last row.
            Some(block) if indented => block.2 = block.2.max(bottom),
            _ => blocks.push((id.to_owned(), top, bottom)),
        };
        if self.hits.workspace_layout.is_empty() {
            for hit in self
                .hits
                .workspaces
                .iter()
                .filter(|hit| hit.endpoint_id == self.active_endpoint_id)
                // A group's toggle row belongs to the group's main space only.
                .filter(|hit| self.group_toggle_hit_is_main_space(hit))
            {
                push(
                    &hit.workspace_id,
                    hit.indented,
                    i32::from(hit.rect.y),
                    i32::from(hit.rect.bottom()),
                );
            }
        } else {
            for layout in &self.hits.workspace_layout {
                push(
                    &layout.workspace_id,
                    layout.indented,
                    layout.top,
                    layout.bottom,
                );
            }
        }
        blocks
    }

    /// Where the dragged space would land: `Some(before)` (`None` for the
    /// end), or `None` when the pointer is outside this endpoint's spaces.
    ///
    /// Spaces move as whole blocks (a space with its indented worktrees). The
    /// dragged block's top follows the pointer at the row it was grabbed by,
    /// and the target is the landing slot nearest to that top in the list
    /// without the dragged block. That list does not depend on where the live
    /// preview drew the block, so the target cannot flip back and forth: the
    /// block passes a neighbour after moving about that neighbour's height.
    fn workspace_drop_target_at(
        &self,
        point: (u16, u16),
        source_workspace_id: &str,
        grab_offset: u16,
    ) -> Option<Option<String>> {
        if self.hits.workspace_body.height == 0
            || point.1 < self.hits.workspace_body.y.saturating_sub(1)
            || point.1 >= self.hits.new_workspace.y
            || self.hits.workspaces.iter().any(|hit| {
                hit.endpoint_id != self.active_endpoint_id && super::contains(hit.rect, point)
            })
        {
            return None;
        }
        let blocks = self.workspace_blocks();
        let source = blocks
            .iter()
            .position(|(id, ..)| id == source_workspace_id)?;
        // Blocks after the dragged one move up into its place, gap included.
        let shift = blocks
            .get(source + 1)
            .map_or(0, |next| next.1 - blocks[source].1);
        let compact_top = |index: usize, top: i32| {
            if index > source {
                top - shift
            } else {
                top
            }
        };
        let mut slots = blocks
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != source)
            .map(|(index, (id, top, _))| (Some(id.clone()), compact_top(index, *top)))
            .collect::<Vec<_>>();
        let end = blocks
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != source)
            .map(|(index, (_, top, bottom))| compact_top(index, *top) + (bottom - top))
            .max()
            .unwrap_or(blocks[source].1);
        slots.push((None, end));
        let top = i32::from(point.1) - i32::from(grab_offset);
        slots
            .into_iter()
            .enumerate()
            .min_by_key(|(index, (_, row))| (top.abs_diff(*row), *index))
            .map(|(_, (before, _))| before)
    }

    pub(super) fn workspace_move_method(
        &self,
        source_workspace_id: &str,
        before_workspace_id: Option<&str>,
    ) -> Option<crate::api::schema::Method> {
        let snapshot = self.snapshot.as_deref()?;
        let source = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == source_workspace_id)?;
        if source
            .worktree
            .as_ref()
            .is_some_and(|worktree| worktree.is_linked_worktree)
        {
            return None;
        }
        if before_workspace_id == Some(source_workspace_id) {
            return None;
        }
        let roots = snapshot
            .workspaces
            .iter()
            .filter(|workspace| {
                !workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        let source_position = roots
            .iter()
            .position(|workspace| workspace.workspace_id == source_workspace_id)?;
        let remaining = roots
            .iter()
            .copied()
            .filter(|workspace| workspace.workspace_id != source_workspace_id)
            .collect::<Vec<_>>();
        let insert_position = match before_workspace_id {
            Some(target) => remaining
                .iter()
                .position(|workspace| workspace.workspace_id == target)?,
            None => remaining.len(),
        };
        if source.worktree.is_none() && insert_position == source_position {
            return None;
        }

        if let Some(worktree) = source.worktree.as_ref() {
            let workspace_ids = std::iter::once(source.workspace_id.clone())
                .chain(
                    snapshot
                        .workspaces
                        .iter()
                        .filter(|workspace| workspace.workspace_id != source.workspace_id)
                        .filter(|workspace| {
                            workspace
                                .worktree
                                .as_ref()
                                .is_some_and(|candidate| candidate.key == worktree.key)
                        })
                        .map(|workspace| workspace.workspace_id.clone()),
                )
                .collect::<Vec<_>>();
            if before_workspace_id.is_some_and(|target| workspace_ids.iter().any(|id| id == target))
            {
                return None;
            }
            Some(crate::api::schema::Method::WorkspaceMoveBlock(
                crate::api::schema::WorkspaceMoveBlockParams {
                    workspace_ids,
                    before_workspace_id: before_workspace_id.map(str::to_owned),
                },
            ))
        } else {
            let insert_index = before_workspace_id
                .and_then(|target| {
                    snapshot
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.workspace_id == target)
                })
                .unwrap_or(snapshot.workspaces.len());
            Some(crate::api::schema::Method::WorkspaceMove(
                crate::api::schema::WorkspaceMoveParams {
                    workspace_id: source.workspace_id.clone(),
                    insert_index,
                },
            ))
        }
    }

    /// Routes mouse input through overlays, shell controls, and pane interactions.
    ///
    /// Hit-test order determines which overlapping control receives the event;
    /// the sidebar toggle takes precedence over the agent scrollbar beneath it.
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent, outcome: &mut ClientShellInput) {
        let over_spaces = !self.sidebar_collapsed
            && super::contains(self.hits.workspace_body, (mouse.column, mouse.row));
        if self.pointer_over_spaces != over_spaces {
            self.pointer_over_spaces = over_spaces;
            // Leaving lets closed jobs' blank slots go.
            outcome.repaint |= !self.hits.space_tab_gone.is_empty();
        }
        self.update_link_hover(mouse, outcome);
        self.update_workspace_hover(mouse, outcome);
        self.update_tooltip(mouse, outcome);
        let point = (mouse.column, mouse.row);
        // A press anywhere gives the keys back to the pane; the filter bar
        // and its button take them again below.
        if self.space_filter.focused && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.space_filter.focused = false;
            outcome.repaint = true;
        }
        if self.mode == ClientShellMode::Navigate
            && self.workspace_preview_action_blocked()
            && self.overlay.is_none()
            && !self.mobile_layout_active()
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        {
            self.mode = self.copy_or_terminal_mode();
            self.navigate_workspace_id = None;
            outcome.repaint = true;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Onboarding)) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && super::contains(self.hits.overlay_primary, point)
            {
                self.complete_onboarding(outcome);
            }
            return;
        }
        if matches!(
            self.overlay,
            Some(ClientShellOverlay::ProductAnnouncement(_))
        ) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if super::contains(self.hits.overlay_primary, point) =>
                {
                    self.dismiss_product_announcement(outcome);
                }
                MouseEventKind::Down(MouseButton::Left)
                    if super::contains(self.hits.product_announcement_scrollbar, point) =>
                {
                    if let Some(metrics) = self.hits.product_announcement_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.product_announcement_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::ProductAnnouncementScrollbar {
                                    grab_row_offset,
                                });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.product_announcement_scrollbar,
                                mouse.row,
                            );
                            self.set_product_announcement_offset_from_bottom(offset);
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let (
                        Some(ClientChromeDrag::ProductAnnouncementScrollbar { grab_row_offset }),
                        Some(metrics),
                    ) = (
                        self.chrome_drag.as_ref(),
                        self.hits.product_announcement_scroll_metrics,
                    ) {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.product_announcement_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        self.set_product_announcement_offset_from_bottom(offset);
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.chrome_drag = None;
                }
                MouseEventKind::ScrollUp => {
                    self.scroll_product_announcement(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_product_announcement(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::ReleaseNotes(_))) {
            let (close, track, metrics) = self
                .current_release_notes_input_geometry()
                .map(|(close, track, metrics)| (close, track, Some(metrics)))
                .unwrap_or((
                    self.hits.overlay_primary,
                    (!self.hits.release_notes_scrollbar.is_empty())
                        .then_some(self.hits.release_notes_scrollbar),
                    self.hits.release_notes_scroll_metrics,
                ));
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if super::contains(close, point) => {
                    self.dismiss_release_notes(outcome);
                }
                MouseEventKind::Down(MouseButton::Left)
                    if track.is_some_and(|track| super::contains(track, point)) =>
                {
                    if let (Some(track), Some(metrics)) = (track, metrics) {
                        if let Some(grab_row_offset) =
                            crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row)
                        {
                            self.chrome_drag =
                                Some(ClientChromeDrag::ReleaseNotesScrollbar { grab_row_offset });
                        } else {
                            let offset =
                                crate::ui::scrollbar_offset_from_row(metrics, track, mouse.row);
                            self.set_release_notes_offset_from_bottom(offset);
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let (
                        Some(ClientChromeDrag::ReleaseNotesScrollbar { grab_row_offset }),
                        Some(track),
                        Some(metrics),
                    ) = (self.chrome_drag.as_ref(), track, metrics)
                    {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            track,
                            mouse.row,
                            *grab_row_offset,
                        );
                        self.set_release_notes_offset_from_bottom(offset);
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.chrome_drag = None;
                }
                MouseEventKind::ScrollUp => {
                    self.scroll_release_notes(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_release_notes(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && self.job_footer_click(point, outcome)
        {
            return;
        }
        if self.url_click_consumes_until_up {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => return,
                MouseEventKind::Up(MouseButton::Left) => {
                    self.url_click_consumes_until_up = false;
                    return;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.url_click_consumes_until_up = false;
                }
                _ => {}
            }
        }
        if !self.replaying_url_click
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            if let Some(fallback_events) =
                self.pending_requests
                    .values_mut()
                    .find_map(|pending| match &mut pending.kind {
                        PendingEndpointKind::PaneLinkActivate {
                            fallback_events, ..
                        } if !fallback_events
                            .iter()
                            .any(|event| event.kind == MouseEventKind::Up(MouseButton::Left)) =>
                        {
                            Some(fallback_events)
                        }
                        _ => None,
                    })
            {
                fallback_events.push(mouse);
                return;
            }
        }
        if let Some(gesture) = self.pane_mouse_gesture.as_ref() {
            let gesture_event = matches!(
                mouse.kind,
                MouseEventKind::Drag(button) | MouseEventKind::Up(button)
                    if button == gesture.button
            );
            if gesture_event {
                let button = gesture.button;
                let modifiers = mouse.modifiers.difference(gesture.stripped_modifiers);
                let hit = if gesture.hit.popup {
                    self.hits
                        .popup
                        .as_ref()
                        .filter(|hit| hit.pane_id == gesture.hit.pane_id)
                        .cloned()
                } else {
                    self.hits
                        .panes
                        .iter()
                        .find(|hit| hit.pane_id == gesture.hit.pane_id)
                        .cloned()
                }
                .unwrap_or_else(|| gesture.hit.clone());
                let position = self.pane_mouse_position(&hit, mouse);
                if let Some(gesture) = self.pane_mouse_gesture.as_mut() {
                    gesture.last_event = mouse;
                    gesture.last_position = position;
                }
                self.push_pane_mouse_event(&hit, mouse, modifiers, outcome);
                if mouse.kind == MouseEventKind::Up(button) {
                    self.pane_mouse_gesture = None;
                }
                return;
            }
            if matches!(
                mouse.kind,
                MouseEventKind::Down(_) | MouseEventKind::Drag(_) | MouseEventKind::Up(_)
            ) {
                return;
            }
        }
        if self.popup_pending {
            return;
        }
        if let Some(hit) = self.hits.popup.clone() {
            if super::contains(hit.inner_rect, point) {
                match mouse.kind {
                    MouseEventKind::Down(button) => {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                        if hit.mouse_reporting {
                            self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                                last_position: self.pane_mouse_position(&hit, mouse),
                                hit,
                                button,
                                stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                                last_event: mouse,
                            });
                        }
                    }
                    MouseEventKind::Moved if hit.mouse_reporting => {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    }
                    MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight => {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    }
                    MouseEventKind::Up(_) | MouseEventKind::Drag(_) | MouseEventKind::Moved => {}
                }
            } else if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && !super::contains(hit.rect, point)
                && self.dismissable_popup_id.as_deref() == Some(hit.pane_id.as_str())
            {
                self.push_endpoint_method(
                    crate::api::schema::Method::PopupClose(Default::default()),
                    outcome,
                );
            }
            return;
        }
        if self.popup_terminal_id.is_some() {
            return;
        }
        if !self.replaying_url_click
            && self.overlay.is_none()
            && self.mode == ClientShellMode::Terminal
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && mouse
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            if let Some(hit) = self
                .hits
                .panes
                .iter()
                .find(|hit| super::contains(hit.inner_rect, point))
                .cloned()
            {
                let viewport_row = mouse.row.saturating_sub(hit.inner_rect.y);
                let col = mouse.column.saturating_sub(hit.inner_rect.x);
                let content_revision = self
                    .pane_surface
                    .as_ref()
                    .and_then(|surface| {
                        surface
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id == hit.pane_id)
                    })
                    .map(|pane| pane.content_revision);
                self.last_pane_click = None;
                let pane_id = hit.pane_id.clone();
                self.push_endpoint_method_with_kind(
                    crate::api::schema::Method::PaneLinkActivate(
                        crate::api::schema::PaneLinkActivateParams {
                            pane_id: pane_id.clone(),
                            viewport_row,
                            col,
                            content_revision,
                            offset_from_bottom: hit
                                .scroll
                                .map(|metrics| metrics.offset_from_bottom as u64),
                        },
                    ),
                    PendingEndpointKind::PaneLinkActivate {
                        pane_id,
                        inner_rect: hit.inner_rect,
                        fallback_events: vec![mouse],
                    },
                    outcome,
                );
                return;
            }
        }
        if self.visible_endpoint_notice.is_some()
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && super::contains(self.hits.notification_toast, point)
        {
            self.visible_endpoint_notice = None;
            outcome.repaint = true;
            return;
        }
        if self.overlay.is_none()
            && self.mode == ClientShellMode::Terminal
            && self
                .visible_notification
                .as_ref()
                .is_some_and(|notification| notification.event.pane_id.is_some())
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && super::contains(self.hits.notification_toast, point)
        {
            self.focus_visible_notification(outcome);
            return;
        }
        if self.handle_mobile_mouse(mouse, outcome) {
            return;
        }
        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            match self.chrome_drag.as_ref() {
                Some(ClientChromeDrag::SidebarWidth) => {
                    self.set_sidebar_width_from_column(mouse.column, outcome);
                    return;
                }
                Some(ClientChromeDrag::SidebarSection) => {
                    self.set_sidebar_section_from_row(mouse.row, outcome);
                    return;
                }
                Some(ClientChromeDrag::WorkspaceScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.workspace_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.workspace_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != self.workspace_scroll {
                            self.workspace_scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(ClientChromeDrag::AgentScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.agent_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.agent_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != self.agent_scroll {
                            self.agent_scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(ClientChromeDrag::NavigatorScrollbar { grab_row_offset }) => {
                    if let Some(metrics) = self.hits.navigator_scroll_metrics {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.navigator_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        self.scroll_navigator_to(
                            metrics.max_offset_from_bottom.saturating_sub(offset),
                            metrics.viewport_rows,
                        );
                        outcome.repaint = true;
                    }
                    return;
                }
                Some(ClientChromeDrag::HelpScrollbar { grab_row_offset }) => {
                    if let (Some(metrics), Some(ClientShellOverlay::Help(help))) =
                        (self.hits.help_scroll_metrics, self.overlay.as_mut())
                    {
                        let offset = crate::ui::scrollbar_offset_from_drag_row(
                            metrics,
                            self.hits.help_scrollbar,
                            mouse.row,
                            *grab_row_offset,
                        );
                        let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                        if next != help.scroll {
                            help.scroll = next;
                            outcome.repaint = true;
                        }
                    }
                    return;
                }
                Some(
                    ClientChromeDrag::ProductAnnouncementScrollbar { .. }
                    | ClientChromeDrag::ReleaseNotesScrollbar { .. },
                ) => {
                    self.chrome_drag = None;
                    return;
                }
                Some(ClientChromeDrag::PaneScrollbar {
                    hit,
                    grab_row_offset,
                    last_sent_offset,
                    last_sent_at,
                }) => {
                    let current_hit = self
                        .hits
                        .panes
                        .iter()
                        .find(|current| current.pane_id == hit.pane_id)
                        .cloned()
                        .unwrap_or_else(|| hit.clone());
                    let Some(offset) = Self::pane_scrollbar_offset(
                        &current_hit,
                        mouse.row,
                        Some(*grab_row_offset),
                    ) else {
                        self.chrome_drag = None;
                        return;
                    };
                    let now = std::time::Instant::now();
                    let should_send = *last_sent_offset != Some(offset)
                        && last_sent_at.is_none_or(|last| {
                            now.duration_since(last) >= std::time::Duration::from_millis(33)
                        });
                    if should_send {
                        if let Some(ClientChromeDrag::PaneScrollbar {
                            last_sent_offset,
                            last_sent_at,
                            ..
                        }) = self.chrome_drag.as_mut()
                        {
                            *last_sent_offset = Some(offset);
                            *last_sent_at = Some(now);
                        }
                        self.push_pane_scroll_offset(current_hit.pane_id, offset, outcome);
                    }
                    return;
                }
                Some(ClientChromeDrag::PaneSplit {
                    hit,
                    tab_id,
                    grab_offset,
                    last_sent_at,
                    ..
                }) => {
                    let hit = hit.clone();
                    let tab_id = tab_id.clone();
                    let grab_offset = *grab_offset;
                    match self.pane_split_target_is_current(&hit, &tab_id) {
                        Some(true) => {}
                        Some(false) => {
                            self.chrome_drag = None;
                            return;
                        }
                        None => return,
                    }
                    let ratio = Self::pane_split_ratio(&hit, grab_offset, point);
                    let now = std::time::Instant::now();
                    let should_send = last_sent_at.is_none_or(|last| {
                        now.duration_since(last) >= std::time::Duration::from_millis(33)
                    });
                    if let Some(ClientChromeDrag::PaneSplit {
                        last_sent_ratio,
                        last_sent_at,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        if should_send {
                            *last_sent_ratio = Some(ratio);
                            *last_sent_at = Some(now);
                        }
                    }
                    if should_send {
                        self.push_endpoint_method(
                            crate::api::schema::Method::LayoutSetSplitRatio(
                                crate::api::schema::LayoutSetSplitRatioParams {
                                    tab_id: Some(tab_id),
                                    pane_id: None,
                                    path: hit.path,
                                    ratio,
                                },
                            ),
                            outcome,
                        );
                    }
                    return;
                }
                Some(ClientChromeDrag::TabLine {
                    workspace_id,
                    tab_id,
                    geometry,
                    ..
                }) => {
                    let (workspace_id, tab_id, geometry) =
                        (workspace_id.clone(), tab_id.clone(), geometry.clone());
                    let insert_index =
                        self.tab_line_drop_index_at(point, &workspace_id, &tab_id, &geometry);
                    if let Some(ClientChromeDrag::TabLine {
                        insert_index: current,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = insert_index;
                    }
                    outcome.repaint = true;
                    return;
                }
                Some(ClientChromeDrag::Tab { .. }) => {
                    let insert_index = self.tab_drop_index_at(point);
                    if let Some(ClientChromeDrag::Tab {
                        insert_index: current,
                        ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = insert_index;
                    }
                    outcome.repaint = true;
                    return;
                }
                Some(ClientChromeDrag::Workspace {
                    source_workspace_id,
                    grab_offset,
                    ..
                }) => {
                    let (source, grab_offset) = (source_workspace_id.clone(), *grab_offset);
                    let target = self.workspace_drop_target_at(point, &source, grab_offset);
                    if let Some(ClientChromeDrag::Workspace {
                        target: current, ..
                    }) = self.chrome_drag.as_mut()
                    {
                        *current = target;
                    }
                    self.update_space_drag_autoscroll(point);
                    outcome.repaint = true;
                    return;
                }
                None => {}
            }
            if let Some(press) = self.workspace_press.as_ref() {
                let delta = mouse
                    .column
                    .abs_diff(press.start_column)
                    .max(mouse.row.abs_diff(press.start_row));
                if delta >= 1 {
                    let source_workspace_id = press.workspace_id.clone();
                    let start_row = press.start_row;
                    let check =
                        self.endpoint_workspace_drag_check(&press.endpoint_id, &press.workspace_id);
                    if let (Err(Some(reason)), Some(press)) = (check, self.workspace_press.as_mut())
                    {
                        // Say why instead of ignoring the drag.
                        if press.refused != Some(reason) {
                            press.refused = Some(reason);
                            outcome.repaint = true;
                        }
                        return;
                    }
                    let grab_offset = self
                        .workspace_blocks()
                        .iter()
                        .find(|(id, ..)| *id == source_workspace_id)
                        .map_or(0, |(_, top, _)| {
                            (i32::from(start_row) - top).clamp(0, i32::from(u16::MAX)) as u16
                        });
                    if check.is_ok() {
                        if let Some(target) =
                            self.workspace_drop_target_at(point, &source_workspace_id, grab_offset)
                        {
                            self.chrome_drag = Some(ClientChromeDrag::Workspace {
                                source_workspace_id,
                                target: Some(target),
                                grab_offset,
                            });
                            outcome.repaint = true;
                            self.update_space_drag_autoscroll(point);
                        }
                    }
                }
                return;
            }
            if let Some(press) = self.tab_press.as_ref().filter(|press| press.sidebar_line) {
                // A vertical list reorders nothing sideways: only a whole row
                // of vertical movement starts the drag, not a click's jitter.
                if mouse.row.abs_diff(press.start_row) >= 1 {
                    let (tab_id, workspace_id) = (press.tab_id.clone(), press.workspace_id.clone());
                    let started = self.tab_line_geometry(&workspace_id).and_then(|geometry| {
                        let index =
                            self.tab_line_drop_index_at(point, &workspace_id, &tab_id, &geometry)?;
                        Some((geometry, index))
                    });
                    if let Some((geometry, insert_index)) = started {
                        self.chrome_drag = Some(ClientChromeDrag::TabLine {
                            tab_id,
                            workspace_id,
                            insert_index: Some(insert_index),
                            geometry,
                        });
                        outcome.repaint = true;
                    }
                }
                return;
            }
            if let Some(press) = self.tab_press.as_ref() {
                let delta = mouse
                    .column
                    .abs_diff(press.start_column)
                    .max(mouse.row.abs_diff(press.start_row));
                // Child tabs are not dragged: their order follows their parent.
                if delta >= 1 && press.main_row {
                    if let Some(insert_index) = self.tab_drop_index_at(point) {
                        self.chrome_drag = Some(ClientChromeDrag::Tab {
                            tab_id: press.tab_id.clone(),
                            workspace_id: press.workspace_id.clone(),
                            insert_index: Some(insert_index),
                        });
                        outcome.repaint = true;
                    }
                }
                return;
            }
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            if let Some(drag) = self.chrome_drag.take() {
                self.workspace_press = None;
                self.tab_press = None;
                match drag {
                    ClientChromeDrag::Tab {
                        tab_id,
                        workspace_id,
                        ..
                    } => {
                        let insert_index = self.tab_drop_index_at(point);
                        let valid_drop = self.snapshot.as_deref().is_some_and(|snapshot| {
                            snapshot.focused_workspace_id.as_deref() == Some(workspace_id.as_str())
                                && snapshot.tabs.iter().any(|tab| {
                                    tab.tab_id == tab_id && tab.workspace_id == workspace_id
                                })
                                && insert_index.is_some_and(|index| {
                                    index <= super::tab_groups::main_row_tabs(snapshot).len()
                                })
                        });
                        let insert_index = self.snapshot.as_deref().map(|snapshot| {
                            super::tab_groups::flat_insert_index(
                                snapshot,
                                insert_index.unwrap_or_default(),
                            )
                        });
                        if let (true, Some(insert_index)) = (valid_drop, insert_index) {
                            self.push_endpoint_method(
                                crate::api::schema::Method::TabMove(
                                    crate::api::schema::TabMoveParams {
                                        tab_id,
                                        insert_index,
                                    },
                                ),
                                outcome,
                            );
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::TabLine {
                        tab_id,
                        workspace_id,
                        geometry,
                        ..
                    } => {
                        let insert_index =
                            self.tab_line_drop_index_at(point, &workspace_id, &tab_id, &geometry);
                        let method = self.snapshot.as_deref().and_then(|snapshot| {
                            let top_level = snapshot
                                .tabs
                                .iter()
                                .filter(|tab| {
                                    tab.workspace_id == workspace_id && tab.parent_tab_id.is_none()
                                })
                                .map(|tab| tab.tab_id.as_str())
                                .collect::<Vec<_>>();
                            let source = top_level.iter().position(|id| *id == tab_id)?;
                            let insert_index =
                                insert_index.filter(|index| *index <= top_level.len())?;
                            // Dropping a tab at its own place changes nothing.
                            if insert_index == source || insert_index == source + 1 {
                                return None;
                            }
                            Some(crate::api::schema::Method::TabMove(
                                crate::api::schema::TabMoveParams {
                                    tab_id: tab_id.clone(),
                                    insert_index: super::tab_groups::workspace_flat_insert_index(
                                        snapshot,
                                        &workspace_id,
                                        insert_index,
                                    ),
                                },
                            ))
                        });
                        if let Some(method) = method {
                            self.push_endpoint_method(method, outcome);
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::Workspace {
                        source_workspace_id,
                        target,
                        ..
                    } => {
                        if let Some(before_workspace_id) = target {
                            if let Some(method) = self.workspace_move_method(
                                &source_workspace_id,
                                before_workspace_id.as_deref(),
                            ) {
                                self.push_endpoint_method(method, outcome);
                            }
                        }
                        outcome.repaint = true;
                    }
                    ClientChromeDrag::PaneScrollbar {
                        hit,
                        grab_row_offset,
                        last_sent_offset,
                        ..
                    } => {
                        let current_hit = self
                            .hits
                            .panes
                            .iter()
                            .find(|current| current.pane_id == hit.pane_id)
                            .cloned()
                            .unwrap_or(hit);
                        if let Some(offset) = Self::pane_scrollbar_offset(
                            &current_hit,
                            mouse.row,
                            Some(grab_row_offset),
                        ) {
                            if last_sent_offset != Some(offset) {
                                self.push_pane_scroll_offset(current_hit.pane_id, offset, outcome);
                            }
                        }
                    }
                    ClientChromeDrag::PaneSplit {
                        hit,
                        tab_id,
                        grab_offset,
                        last_sent_ratio,
                        ..
                    } => {
                        let target_is_current =
                            self.pane_split_target_is_current(&hit, &tab_id) == Some(true);
                        let ratio = Self::pane_split_ratio(&hit, grab_offset, point);
                        if target_is_current
                            && last_sent_ratio
                                .is_none_or(|sent| (sent - ratio).abs() > f32::EPSILON)
                        {
                            self.push_endpoint_method(
                                crate::api::schema::Method::LayoutSetSplitRatio(
                                    crate::api::schema::LayoutSetSplitRatioParams {
                                        tab_id: Some(tab_id),
                                        pane_id: None,
                                        path: hit.path,
                                        ratio,
                                    },
                                ),
                                outcome,
                            );
                        }
                    }
                    ClientChromeDrag::SidebarWidth | ClientChromeDrag::SidebarSection => {
                        self.persist_chrome_preferences(outcome);
                    }
                    ClientChromeDrag::WorkspaceScrollbar { .. }
                    | ClientChromeDrag::AgentScrollbar { .. }
                    | ClientChromeDrag::HelpScrollbar { .. }
                    | ClientChromeDrag::NavigatorScrollbar { .. }
                    | ClientChromeDrag::ProductAnnouncementScrollbar { .. }
                    | ClientChromeDrag::ReleaseNotesScrollbar { .. } => {}
                }
                return;
            }
            if let Some(press) = self.workspace_press.take() {
                self.finish_endpoint_workspace_press(press, outcome);
                return;
            }
            if let Some(press) = self.tab_press.take() {
                // A main-row tab stands for its whole group; its own entry in
                // the second row selects the tab itself.
                let tab_id = if press.main_row && !press.sidebar_line {
                    self.group_entry_tab(&press.tab_id)
                } else {
                    press.tab_id
                };
                self.push_endpoint_method(
                    crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget { tab_id }),
                    outcome,
                );
                return;
            }
        }
        // A row's menu is open over the list: it takes the pointer, and a
        // click closes it alone.
        if matches!(&self.overlay, Some(ClientShellOverlay::NotificationLog(log)) if log.menu.is_some())
        {
            let item = self
                .hits
                .list_menu_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .map(|(_, index)| *index);
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(index) = item {
                        self.highlight_list_row_menu_item(index);
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(button) => {
                    if let Some(menu) = self.take_list_row_menu() {
                        if let (Some(index), MouseButton::Left) = (item, button) {
                            self.activate_list_row_menu_item(menu, index, outcome);
                        }
                    }
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::NotificationLog(_))) {
            let row_hit = self
                .hits
                .notification_log_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some((_, index)) = row_hit {
                        self.highlight_notification_log_row(index);
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, index)) = row_hit {
                        self.activate_notification_log_row(index, outcome);
                    } else {
                        // Also the button: it closes what it opened.
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                // A right click on a row opens its tab's menu over the list.
                MouseEventKind::Down(MouseButton::Right) => {
                    if let Some((_, index)) = row_hit {
                        if self.open_list_row_menu(index, point.0, point.1) {
                            outcome.repaint = true;
                        }
                    }
                }
                // A middle click on a bookmark removes it.
                MouseEventKind::Down(MouseButton::Middle) => {
                    if let Some((_, index)) = row_hit {
                        self.remove_bookmark_row(index, outcome);
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {}
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_))) {
            let row_hit = self
                .hits
                .global_menu_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let (Some((_, index)), Some(ClientShellOverlay::GlobalMenu(menu))) =
                        (row_hit, self.overlay.as_mut())
                    {
                        menu.highlighted = index;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.global_launcher, point) {
                        self.toggle_global_menu();
                        outcome.repaint = true;
                    } else if let Some((_, index)) = row_hit {
                        self.activate_global_menu_item(index, outcome);
                    } else {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::ContextMenu(_))) {
            let row_hit = self
                .hits
                .context_menu_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .copied();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let (Some((_, index)), Some(ClientShellOverlay::ContextMenu(menu))) =
                        (row_hit, self.overlay.as_mut())
                    {
                        menu.highlighted = index;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, index)) = row_hit {
                        self.activate_context_menu_item(index, outcome);
                    } else {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(
            self.overlay,
            Some(
                ClientShellOverlay::WorktreeCreate(_)
                    | ClientShellOverlay::WorktreeOpen(_)
                    | ClientShellOverlay::WorktreeRemove(_)
            )
        ) {
            match mouse.kind {
                MouseEventKind::ScrollUp
                    if matches!(self.overlay, Some(ClientShellOverlay::WorktreeOpen(_))) =>
                {
                    self.move_worktree_open_selection(-1);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown
                    if matches!(self.overlay, Some(ClientShellOverlay::WorktreeOpen(_))) =>
                {
                    self.move_worktree_open_selection(1);
                    outcome.repaint = true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.overlay_cancel, point) {
                        let busy =
                            matches!(
                                self.overlay,
                                Some(
                                    ClientShellOverlay::WorktreeCreate(
                                        ClientWorktreeCreateOverlay { creating: true, .. }
                                    ) | ClientShellOverlay::WorktreeOpen(
                                        ClientWorktreeOpenOverlay { opening: true, .. }
                                    ) | ClientShellOverlay::WorktreeRemove(
                                        ClientWorktreeRemoveOverlay { removing: true, .. }
                                    )
                                )
                            );
                        if !busy {
                            self.overlay = None;
                            outcome.repaint = true;
                        }
                    } else if super::contains(self.hits.worktree_search, point) {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.search_focused = true;
                            outcome.repaint = true;
                        }
                    } else if let Some((_, index)) = self
                        .hits
                        .worktree_rows
                        .iter()
                        .find(|(rect, _)| super::contains(*rect, point))
                        .copied()
                    {
                        if let Some(ClientShellOverlay::WorktreeOpen(open)) = self.overlay.as_mut()
                        {
                            open.selected = index;
                        }
                        self.submit_worktree_open(outcome);
                    } else if super::contains(self.hits.overlay_primary, point) {
                        match self.overlay.as_ref() {
                            Some(ClientShellOverlay::WorktreeCreate(_)) => {
                                self.submit_worktree_create(outcome)
                            }
                            Some(ClientShellOverlay::WorktreeOpen(_)) => {
                                self.submit_worktree_open(outcome)
                            }
                            Some(ClientShellOverlay::WorktreeRemove(_)) => {
                                self.submit_worktree_remove(outcome)
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Settings(_))) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some((_, section)) = self
                    .hits
                    .settings_tabs
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .copied()
                {
                    self.select_settings_section(section, outcome);
                } else if let Some((_, index)) = self
                    .hits
                    .settings_choices
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .copied()
                {
                    self.select_settings_choice(index);
                    let immediate = matches!(
                        self.overlay,
                        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                            section: ClientSettingsSection::Indicators
                                | ClientSettingsSection::Sound
                                | ClientSettingsSection::Toast
                                | ClientSettingsSection::Usage,
                            ..
                        }))
                    );
                    if immediate {
                        self.apply_settings_choice(outcome);
                    }
                    outcome.repaint = true;
                } else if super::contains(self.hits.overlay_primary, point) {
                    self.apply_settings_choice(outcome);
                } else if super::contains(self.hits.overlay_cancel, point)
                    || !super::contains(self.hits.settings_popup, point)
                {
                    let installing = matches!(
                        self.overlay,
                        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                            installing_integrations: true,
                            ..
                        }))
                    );
                    if !installing {
                        self.cancel_settings_overlay();
                        outcome.repaint = true;
                    }
                }
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Help(_))) {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        let next = help.scroll.saturating_sub(3);
                        if next != help.scroll {
                            help.scroll = next;
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        let next = help.scroll.saturating_add(3).min(self.hits.help_max_scroll);
                        if next != help.scroll {
                            help.scroll = next;
                            outcome.repaint = true;
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.help_scrollbar, point) {
                        if let Some(metrics) = self.hits.help_scroll_metrics {
                            if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                                metrics,
                                self.hits.help_scrollbar,
                                mouse.row,
                            ) {
                                self.chrome_drag =
                                    Some(ClientChromeDrag::HelpScrollbar { grab_row_offset });
                            } else {
                                let offset = crate::ui::scrollbar_offset_from_row(
                                    metrics,
                                    self.hits.help_scrollbar,
                                    mouse.row,
                                );
                                if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut()
                                {
                                    help.scroll =
                                        metrics.max_offset_from_bottom.saturating_sub(offset);
                                    outcome.repaint = true;
                                }
                            }
                        }
                    } else if super::contains(self.hits.overlay_cancel, point) {
                        let search_focused = matches!(
                            self.overlay,
                            Some(ClientShellOverlay::Help(ClientHelpOverlay {
                                search_focused: true,
                                ..
                            }))
                        );
                        if search_focused {
                            if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                                help.search_focused = false;
                                help.query.clear();
                                help.scroll = 0;
                            }
                        } else {
                            self.overlay = None;
                        }
                        outcome.repaint = true;
                    } else if !super::contains(self.hits.help_popup, point) {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Navigator(_))) {
            let row_hit = self
                .hits
                .navigator_rows
                .iter()
                .find(|(rect, _)| super::contains(*rect, point))
                .cloned();
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some((_, target)) = row_hit {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.selected = Some(target);
                        }
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if super::contains(self.hits.navigator_scrollbar, point) {
                        if let Some(metrics) = self.hits.navigator_scroll_metrics {
                            if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                                metrics,
                                self.hits.navigator_scrollbar,
                                mouse.row,
                            ) {
                                self.chrome_drag =
                                    Some(ClientChromeDrag::NavigatorScrollbar { grab_row_offset });
                            } else {
                                let offset = crate::ui::scrollbar_offset_from_row(
                                    metrics,
                                    self.hits.navigator_scrollbar,
                                    mouse.row,
                                );
                                self.scroll_navigator_to(
                                    metrics.max_offset_from_bottom.saturating_sub(offset),
                                    metrics.viewport_rows,
                                );
                                outcome.repaint = true;
                            }
                        }
                    } else if super::contains(self.hits.navigator_search, point) {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.search_focused = true;
                            navigator.filter = None;
                        }
                        outcome.repaint = true;
                    } else if let Some((_, target)) = row_hit {
                        if let Some(ClientShellOverlay::Navigator(navigator)) =
                            self.overlay.as_mut()
                        {
                            navigator.selected = Some(target);
                        }
                        self.accept_navigator_selection(outcome);
                    } else if !super::contains(self.hits.navigator_popup, point) {
                        self.overlay = None;
                        outcome.repaint = true;
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.move_navigator_selection(-3);
                    outcome.repaint = true;
                }
                MouseEventKind::ScrollDown => {
                    self.move_navigator_selection(3);
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }
        if self.overlay.is_some() {
            if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
                return;
            }
            if super::contains(self.hits.overlay_primary, point) {
                match self.overlay.as_ref() {
                    Some(ClientShellOverlay::Rename(_)) => self.save_rename_overlay(outcome),
                    Some(ClientShellOverlay::ConfirmClose(_)) => {
                        self.accept_close_confirmation(outcome);
                    }
                    Some(ClientShellOverlay::Usage(_)) => self.refresh_usage(outcome),
                    _ => {}
                }
            } else if super::contains(self.hits.overlay_clear, point) {
                if let Some(ClientShellOverlay::Rename(rename)) = self.overlay.as_mut() {
                    rename.input.clear();
                    outcome.repaint = true;
                }
            } else {
                self.overlay = None;
                outcome.repaint = true;
            }
            return;
        }

        if mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            let selection_hit = self.active_selection_pane();
            if let Some(hit) = selection_hit {
                self.update_selection_drag(&hit, mouse.column, mouse.row, outcome);
                // Consume every motion, but do not rebuild a frame for every intermediate position.
                outcome.repaint |= !outcome.actions.is_empty()
                    || self.request_selection_drag_repaint(std::time::Instant::now());
                return;
            }
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left)
            && self.word_selection_gesture.is_some()
        {
            self.finish_word_selection(outcome);
            outcome.repaint = true;
            return;
        }
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) && self.selection.is_some() {
            self.stop_selection_autoscroll();
            let copied = self
                .selection
                .as_mut()
                .is_some_and(crate::selection::Selection::finish);
            if copied && self.config.copy_on_select {
                self.request_selection_copy(outcome, true);
                self.selection = None;
            } else if self
                .selection
                .as_ref()
                .is_some_and(crate::selection::Selection::is_just_click)
            {
                self.selection = None;
            }
            if copied {
                self.last_pane_click = None;
            }
            outcome.repaint = true;
            return;
        }
        if self.scroll_in_progress_selection(mouse, outcome) {
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                let pane_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point))
                    .cloned();
                if let Some(hit) = pane_hit {
                    let pane_owns_right_click = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| {
                            snapshot
                                .panes
                                .iter()
                                .find(|pane| pane.pane_id == hit.pane_id)
                        })
                        .is_some_and(|pane| pane.right_click_passthrough)
                        && mouse.modifiers.is_empty();
                    let configured_modifiers = self
                        .config
                        .right_click_passthrough_modifiers
                        .filter(|modifiers| *modifiers == mouse.modifiers);
                    if hit.mouse_reporting
                        && (pane_owns_right_click || configured_modifiers.is_some())
                    {
                        let stripped_modifiers =
                            configured_modifiers.unwrap_or(crossterm::event::KeyModifiers::empty());
                        self.push_pane_mouse_event(
                            &hit,
                            mouse,
                            mouse.modifiers.difference(stripped_modifiers),
                            outcome,
                        );
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id: hit.pane_id.clone(),
                            }),
                            outcome,
                        );
                        self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                            last_position: self.pane_mouse_position(&hit, mouse),
                            hit,
                            button: MouseButton::Right,
                            stripped_modifiers,
                            last_event: mouse,
                        });
                        return;
                    }
                }
                if !self.config.mouse_capture {
                    return;
                }
                if self.on_gone_square(point) {
                    return;
                }
                let launch = self
                    .hits
                    .space_launch_agent
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .cloned();
                if let Some((button, workspace_id)) = launch {
                    self.open_agent_picker(workspace_id, button, outcome);
                    return;
                }
                if let Some(tab_id) = self.space_tab_at(point) {
                    self.open_tab_context_menu(tab_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let workspace_id = (!self.sidebar_collapsed)
                    .then(|| self.active_endpoint_workspace_at(point))
                    .flatten();
                if let Some(workspace_id) = workspace_id {
                    self.open_workspace_context_menu(workspace_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let tab_id = self
                    .hits
                    .tabs
                    .iter()
                    .chain(&self.hits.child_tabs)
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, tab_id)| tab_id.clone());
                if let Some(tab_id) = tab_id {
                    self.open_tab_context_menu(tab_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                    return;
                }
                let pane_id = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| hit.pane_id.clone());
                if let Some(pane_id) = pane_id {
                    self.open_pane_context_menu(pane_id, mouse.column, mouse.row);
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if super::contains(self.hits.tab_bar, point)
                    || super::contains(self.hits.child_tab_bar, point) =>
            {
                let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                    -1
                } else {
                    1
                };
                // Each row steps through its own tabs and stops at its ends.
                let in_child_row = super::contains(self.hits.child_tab_bar, point);
                let tab_id = if in_child_row {
                    self.child_row_step(delta)
                } else {
                    self.main_row_step(delta)
                };
                if let Some(tab_id) = tab_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                            tab_id,
                        }),
                        outcome,
                    );
                }
            }
            MouseEventKind::ScrollUp if super::contains(self.hits.agent_body, point) => {
                let next = self.agent_scroll.saturating_sub(1);
                if next != self.agent_scroll {
                    self.agent_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollDown if super::contains(self.hits.agent_body, point) => {
                let next = self
                    .agent_scroll
                    .saturating_add(1)
                    .min(self.hits.agent_max_scroll);
                if next != self.agent_scroll {
                    self.agent_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollUp if super::contains(self.hits.workspace_body, point) => {
                let next = self
                    .workspace_scroll
                    .saturating_sub(self.workspace_wheel_step());
                if next != self.workspace_scroll {
                    self.workspace_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::ScrollDown if super::contains(self.hits.workspace_body, point) => {
                let next = self
                    .workspace_scroll
                    .saturating_add(self.workspace_wheel_step())
                    .min(self.hits.workspace_max_scroll);
                if next != self.workspace_scroll {
                    self.workspace_scroll = next;
                    outcome.repaint = true;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.selection.take().is_some() {
                    outcome.repaint = true;
                }
                self.stop_selection_autoscroll();
                self.selection_highlight_clear_deadline = None;
                self.word_selection_gesture = None;
                let previous_pane_click = self.last_pane_click.take();
                self.workspace_press = None;
                self.tab_press = None;
                self.chrome_drag = None;
                // The toggle is painted over the agent scrollbar's last cell.
                if super::contains(self.hits.sidebar_toggle, point) {
                    self.sidebar_collapsed = !self.sidebar_collapsed;
                    self.sidebar_collapsed_manual = true;
                    self.invalidate_pane_surface();
                    outcome.repaint = true;
                    outcome.resize = true;
                    self.persist_chrome_preferences(outcome);
                    return;
                }
                if super::contains(self.hits.sidebar_divider, point) {
                    let now = std::time::Instant::now();
                    let double_click = self.last_sidebar_divider_click.is_some_and(|last| {
                        now.duration_since(last) <= std::time::Duration::from_millis(350)
                    });
                    self.last_sidebar_divider_click = Some(now);
                    if double_click {
                        self.sidebar_width = self.config.sidebar_width;
                        self.sidebar_width_manual = false;
                        self.invalidate_pane_surface();
                        outcome.repaint = true;
                        outcome.resize = true;
                        self.persist_chrome_preferences(outcome);
                    } else {
                        self.chrome_drag = Some(ClientChromeDrag::SidebarWidth);
                        self.set_sidebar_width_from_column(mouse.column, outcome);
                    }
                    return;
                }
                if super::contains(self.hits.sidebar_section_divider, point) {
                    self.chrome_drag = Some(ClientChromeDrag::SidebarSection);
                    self.set_sidebar_section_from_row(mouse.row, outcome);
                    return;
                }
                if super::contains(self.hits.workspace_scrollbar, point) {
                    if let Some(metrics) = self.hits.workspace_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.workspace_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::WorkspaceScrollbar { grab_row_offset });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.workspace_scrollbar,
                                mouse.row,
                            );
                            let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                            if next != self.workspace_scroll {
                                self.workspace_scroll = next;
                                outcome.repaint = true;
                            }
                        }
                    }
                    return;
                }
                if super::contains(self.hits.agent_scrollbar, point) {
                    if let Some(metrics) = self.hits.agent_scroll_metrics {
                        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(
                            metrics,
                            self.hits.agent_scrollbar,
                            mouse.row,
                        ) {
                            self.chrome_drag =
                                Some(ClientChromeDrag::AgentScrollbar { grab_row_offset });
                        } else {
                            let offset = crate::ui::scrollbar_offset_from_row(
                                metrics,
                                self.hits.agent_scrollbar,
                                mouse.row,
                            );
                            let next = metrics.max_offset_from_bottom.saturating_sub(offset);
                            if next != self.agent_scroll {
                                self.agent_scroll = next;
                                outcome.repaint = true;
                            }
                        }
                    }
                    return;
                }
                if super::contains(self.hits.usage_footer, point) {
                    self.open_usage_overlay(outcome);
                    return;
                }
                if let Some(rect) = self
                    .hits
                    .space_sort_buttons
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(rect, _)| *rect)
                {
                    self.open_space_sort_menu(rect.x, rect.y.saturating_add(1));
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.agent_sort_toggle, point) {
                    let sort = match self.config.agent_panel_sort {
                        crate::config::AgentPanelSortConfig::Spaces => {
                            crate::config::AgentPanelSortConfig::Priority
                        }
                        crate::config::AgentPanelSortConfig::Priority => {
                            crate::config::AgentPanelSortConfig::Spaces
                        }
                    };
                    self.config.agent_panel_sort = sort;
                    self.agent_panel_sort_manual = true;
                    self.agent_scroll = 0;
                    self.persist_chrome_preferences(outcome);
                    outcome.repaint = true;
                    return;
                }
                if self.handle_endpoint_machine_click(point, outcome) {
                    return;
                }
                if super::contains(self.hits.global_launcher, point) {
                    self.toggle_global_menu();
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.notification_log_button, point) {
                    self.toggle_notification_log(outcome);
                    outcome.repaint = true;
                    return;
                }
                for (rect, view) in [
                    (
                        self.hits.working_list_button,
                        super::notification_log::NotificationLogView::Working,
                    ),
                    (
                        self.hits.asking_list_button,
                        super::notification_log::NotificationLogView::Asking,
                    ),
                    (
                        self.hits.bookmarks_list_button,
                        super::notification_log::NotificationLogView::Bookmarks,
                    ),
                ] {
                    if super::contains(rect, point) {
                        self.toggle_notification_view(view, outcome);
                        outcome.repaint = true;
                        return;
                    }
                }
                if super::contains(self.hits.space_filter_button, point) {
                    // The button opens the bar for typing, or closes it.
                    if self.space_filter.open {
                        self.space_filter.close();
                    } else {
                        self.space_filter.open = true;
                        self.space_filter.focused = true;
                    }
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.space_filter_close, point) {
                    self.space_filter.close();
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.space_filter_bar, point) {
                    self.space_filter.focused = true;
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.new_workspace, point) {
                    self.record_binding(
                        crate::input::KeybindMatch::Action(
                            crate::input::KeybindAction::NewWorkspace,
                        ),
                        outcome,
                    );
                    return;
                }
                if super::contains(self.hits.new_tab, point) {
                    self.record_binding(
                        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
                        outcome,
                    );
                    return;
                }
                if super::contains(self.hits.tab_scroll_left, point) {
                    self.tab_scroll = self.tab_scroll.saturating_sub(1);
                    outcome.repaint = true;
                    return;
                }
                if super::contains(self.hits.tab_scroll_right, point) {
                    let tab_count = self.snapshot.as_deref().map_or(0, |snapshot| {
                        super::tab_groups::main_row_tabs(snapshot).len()
                    });
                    self.tab_scroll = self
                        .tab_scroll
                        .saturating_add(1)
                        .min(tab_count.saturating_sub(1));
                    outcome.repaint = true;
                    return;
                }
                let group_toggle = self.hits.workspaces.iter().find_map(|hit| {
                    let (rect, key) = hit.group_toggle.as_ref()?;
                    super::contains(*rect, point).then(|| (hit.endpoint_id.clone(), key.clone()))
                });
                if let Some((endpoint_id, key)) = group_toggle {
                    self.toggle_collapsed_group(&endpoint_id, key);
                    outcome.repaint = true;
                    self.persist_chrome_preferences(outcome);
                    return;
                }
                // The launch button starts its agent in a new tab.
                let launch = self
                    .hits
                    .space_launch_agent
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .cloned();
                if let Some((button, workspace_id)) = launch {
                    self.click_launch_button(workspace_id, button, outcome);
                    return;
                }
                // The push status chip opens the space's branch menu.
                let push_status = self
                    .hits
                    .space_push_status
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .cloned();
                if let Some((chip, workspace_id)) = push_status {
                    self.open_branch_menu(workspace_id, chip, outcome);
                    return;
                }
                // The `+` on a space's name line opens a tab in that space.
                let new_tab = self
                    .hits
                    .space_new_tab
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, workspace_id)| workspace_id.clone());
                if let Some(workspace_id) = new_tab {
                    // A collapsed space shows the new tab.
                    if self.unfold_space_tabs(&workspace_id) {
                        self.persist_chrome_preferences(outcome);
                    }
                    self.push_endpoint_method(
                        crate::api::schema::Method::TabCreate(
                            crate::api::schema::TabCreateParams {
                                workspace_id: Some(workspace_id),
                                cwd: None,
                                focus: true,
                                label: None,
                                env: Default::default(),
                            },
                        ),
                        outcome,
                    );
                    return;
                }
                // A tab line waits for the release: a drag reorders it, a
                // click opens it.
                if let Some(press) = self.space_tab_line_press(&mouse) {
                    self.tab_press = Some(press);
                    return;
                }
                // A tab line or square under a space acts on its tab, not
                // the space.
                if let Some(target) = self.space_tab_click(point) {
                    match target {
                        Some(tab_id) => self.push_endpoint_method(
                            crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                                tab_id,
                            }),
                            outcome,
                        ),
                        None => outcome.repaint = true,
                    }
                    return;
                }
                let workspace_press = self
                    .hits
                    .workspaces
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .map(|hit| ClientWorkspacePress {
                        endpoint_id: hit.endpoint_id.clone(),
                        workspace_id: hit.workspace_id.clone(),
                        start_column: mouse.column,
                        start_row: mouse.row,
                        refused: None,
                    });
                if let Some(workspace_press) = workspace_press {
                    self.workspace_press = Some(workspace_press);
                    return;
                }
                let tab_press = self
                    .config
                    .mouse_capture
                    .then(|| {
                        self.hits
                            .tabs
                            .iter()
                            .map(|hit| (hit, true))
                            .chain(self.hits.child_tabs.iter().map(|hit| (hit, false)))
                            .find(|((rect, _), _)| super::contains(*rect, point))
                            .and_then(|((_, tab_id), main_row)| {
                                let tab = self
                                    .snapshot
                                    .as_deref()?
                                    .tabs
                                    .iter()
                                    .find(|tab| tab.tab_id == *tab_id)?;
                                Some(ClientTabPress {
                                    tab_id: tab.tab_id.clone(),
                                    workspace_id: tab.workspace_id.clone(),
                                    main_row,
                                    sidebar_line: false,
                                    start_column: mouse.column,
                                    start_row: mouse.row,
                                })
                            })
                    })
                    .flatten();
                if let Some(tab_press) = tab_press {
                    self.tab_press = Some(tab_press);
                    return;
                }
                if self.handle_endpoint_agent_click(point, outcome) {
                    return;
                }
                let agent_pane_id = self
                    .hits
                    .agents
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, pane_id)| pane_id.clone());
                if let Some(pane_id) = agent_pane_id {
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id,
                        }),
                        outcome,
                    );
                    return;
                }
                let scrollbar_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| {
                        hit.scrollbar_rect
                            .is_some_and(|rect| super::contains(rect, point))
                            && hit
                                .scroll
                                .is_some_and(|metrics| metrics.max_offset_from_bottom > 0)
                    })
                    .cloned();
                if let Some(hit) = scrollbar_hit {
                    self.mode = ClientShellMode::Terminal;
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id: hit.pane_id.clone(),
                        }),
                        outcome,
                    );
                    let (Some(track), Some(metrics)) = (hit.scrollbar_rect, hit.scroll) else {
                        return;
                    };
                    if let Some(grab_row_offset) =
                        crate::ui::scrollbar_thumb_grab_offset(metrics, track, mouse.row)
                    {
                        self.chrome_drag = Some(ClientChromeDrag::PaneScrollbar {
                            hit,
                            grab_row_offset,
                            last_sent_offset: None,
                            last_sent_at: None,
                        });
                    } else if let Some(offset) = Self::pane_scrollbar_offset(&hit, mouse.row, None)
                    {
                        self.push_pane_scroll_offset(hit.pane_id, offset, outcome);
                    }
                    return;
                }
                let split_hit = self
                    .hits
                    .pane_splits
                    .iter()
                    .find(|hit| super::contains(hit.hit_rect, point))
                    .cloned();
                if let Some(hit) = split_hit {
                    let Some(tab_id) = self
                        .snapshot
                        .as_deref()
                        .and_then(|snapshot| snapshot.focused_tab_id.clone())
                    else {
                        return;
                    };
                    let pointer = match hit.direction {
                        crate::protocol::PaneSurfaceSplitDirection::Horizontal => mouse.column,
                        crate::protocol::PaneSurfaceSplitDirection::Vertical => mouse.row,
                    };
                    self.chrome_drag = Some(ClientChromeDrag::PaneSplit {
                        grab_offset: i32::from(hit.pos) - i32::from(pointer),
                        last_sent_ratio: None,
                        last_sent_at: None,
                        hit,
                        tab_id,
                    });
                    return;
                }
                let pane_hit = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.rect, point))
                    .cloned();
                if let Some(hit) = pane_hit {
                    if hit.mouse_reporting && super::contains(hit.inner_rect, point) {
                        self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                        self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                            last_position: self.pane_mouse_position(&hit, mouse),
                            hit: hit.clone(),
                            button: MouseButton::Left,
                            stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                            last_event: mouse,
                        });
                    } else if super::contains(hit.inner_rect, point) {
                        let click = ClientPaneClick {
                            pane_id: hit.pane_id.clone(),
                            viewport_row: mouse.row.saturating_sub(hit.inner_rect.y),
                            col: mouse.column.saturating_sub(hit.inner_rect.x),
                            at: std::time::Instant::now(),
                        };
                        if mouse.modifiers.is_empty()
                            && previous_pane_click
                                .as_ref()
                                .is_some_and(|previous| previous.is_double_click_for(&click))
                        {
                            self.request_word_selection(
                                &hit,
                                click.viewport_row,
                                click.col,
                                outcome,
                            );
                        } else {
                            if mouse.modifiers.is_empty() {
                                self.last_pane_click = Some(click);
                            }
                            self.selection = Some(crate::selection::Selection::anchor(
                                hit.pane_id.clone(),
                                mouse.row.saturating_sub(hit.inner_rect.y),
                                mouse.column.saturating_sub(hit.inner_rect.x),
                                hit.scroll,
                            ));
                        }
                    }
                    self.push_endpoint_method(
                        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                            pane_id: hit.pane_id,
                        }),
                        outcome,
                    );
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point) && hit.mouse_reporting)
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                    self.pane_mouse_gesture = Some(ClientPaneMouseGesture {
                        last_position: self.pane_mouse_position(&hit, mouse),
                        hit,
                        button: MouseButton::Middle,
                        stripped_modifiers: crossterm::event::KeyModifiers::empty(),
                        last_event: mouse,
                    });
                    return;
                }
                self.close_chrome_target_at(point, outcome);
            }
            MouseEventKind::Up(MouseButton::Left | MouseButton::Middle)
            | MouseEventKind::Drag(MouseButton::Left | MouseButton::Middle) => {}
            MouseEventKind::Moved => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point) && hit.mouse_reporting)
                    .cloned()
                {
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                }
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                if let Some(hit) = self
                    .hits
                    .panes
                    .iter()
                    .find(|hit| super::contains(hit.inner_rect, point))
                    .cloned()
                {
                    if self.focused_pane_id().as_deref() != Some(hit.pane_id.as_str()) {
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id: hit.pane_id.clone(),
                            }),
                            outcome,
                        );
                    }
                    self.push_pane_mouse_event(&hit, mouse, mouse.modifiers, outcome);
                }
            }
            _ => {}
        }
    }

    fn pane_mouse_position(&self, hit: &PaneHit, mouse: MouseEvent) -> ClientMousePosition {
        let cell = ClientMousePosition::Cell {
            column: mouse.column.saturating_sub(hit.inner_rect.x),
            row: mouse.row.saturating_sub(hit.inner_rect.y),
        };
        if hit.sgr_pixel_mouse && hit.pixel_width > 0 && hit.pixel_height > 0 {
            self.host_mouse_pixels
                .and_then(|pixels| {
                    pixels
                        .pane_position(hit.inner_rect, hit.pixel_width, hit.pixel_height)
                        .and_then(|position| match position {
                            crate::input::mouse::Position::Pixels { x, y } => {
                                Some(ClientMousePosition::Pixels {
                                    x,
                                    y,
                                    column: mouse.column.saturating_sub(hit.inner_rect.x),
                                    row: mouse.row.saturating_sub(hit.inner_rect.y),
                                })
                            }
                            crate::input::mouse::Position::Cell { .. } => None,
                        })
                })
                .unwrap_or(cell)
        } else {
            cell
        }
    }

    pub(super) fn push_pane_mouse_event(
        &self,
        hit: &PaneHit,
        mouse: MouseEvent,
        modifiers: crossterm::event::KeyModifiers,
        outcome: &mut ClientShellInput,
    ) {
        let Some(kind) = crate::protocol::ClientMouseKind::from_crossterm(mouse.kind) else {
            return;
        };
        let position = self.pane_mouse_position(hit, mouse);
        let geometry = matches!(position, ClientMousePosition::Pixels { .. }).then_some(
            crate::protocol::ClientMouseGeometry {
                cols: hit.inner_rect.width,
                rows: hit.inner_rect.height,
                width_px: hit.pixel_width,
                height_px: hit.pixel_height,
            },
        );
        let target = if hit.popup {
            ClientInputTarget::Popup(hit.pane_id.clone())
        } else {
            ClientInputTarget::Pane(hit.pane_id.clone())
        };
        push_target_event(
            target,
            ClientPaneInputEvent::Mouse {
                kind,
                position,
                geometry,
                modifiers: modifiers.bits(),
                lines: self.config.mouse_scroll_lines.min(u16::MAX as usize) as u16,
            },
            outcome,
        );
    }
}
