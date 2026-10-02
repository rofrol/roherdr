use super::*;

impl ClientShellState {
    pub(super) fn dismiss_product_announcement(&mut self, outcome: &mut ClientShellInput) {
        let announcement = match self.overlay.take() {
            Some(ClientShellOverlay::ProductAnnouncement(announcement)) => announcement,
            other => {
                self.overlay = other;
                return;
            }
        };
        if self.snapshot.is_none() {
            self.overlay = Some(ClientShellOverlay::ProductAnnouncement(announcement));
            return;
        }
        self.dismissed_product_announcement =
            Some((announcement.version.clone(), announcement.id.clone()));
        self.chrome_drag = None;
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ProductAnnouncementDismiss(
                crate::api::schema::ProductAnnouncementDismissParams {
                    version: announcement.version.clone(),
                    id: announcement.id.clone(),
                },
            ),
            PendingEndpointKind::ProductAnnouncementDismiss {
                version: announcement.version,
                id: announcement.id,
            },
            outcome,
        );
        outcome.repaint = true;
    }

    pub(super) fn scroll_product_announcement(&mut self, delta: isize) {
        let max_scroll = self.hits.product_announcement_max_scroll;
        if let Some(ClientShellOverlay::ProductAnnouncement(announcement)) = self.overlay.as_mut() {
            let current = usize::from(announcement.scroll);
            let next = if delta.is_negative() {
                current.saturating_sub(delta.unsigned_abs())
            } else {
                current.saturating_add(delta as usize)
            }
            .min(max_scroll);
            announcement.scroll = u16::try_from(next).unwrap_or(u16::MAX);
        }
    }

    pub(super) fn set_product_announcement_offset_from_bottom(
        &mut self,
        offset_from_bottom: usize,
    ) {
        let max_scroll = self.hits.product_announcement_max_scroll;
        if let Some(ClientShellOverlay::ProductAnnouncement(announcement)) = self.overlay.as_mut() {
            announcement.scroll =
                u16::try_from(max_scroll.saturating_sub(offset_from_bottom.min(max_scroll)))
                    .unwrap_or(u16::MAX);
        }
    }

    pub(super) fn open_release_notes(&mut self) {
        let Some(notes) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.release_notes.as_ref())
        else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::ReleaseNotes(release_notes_state(notes)));
        self.chrome_drag = None;
    }

    pub(super) fn dismiss_release_notes(&mut self, outcome: &mut ClientShellInput) {
        let notes = match self.overlay.take() {
            Some(ClientShellOverlay::ReleaseNotes(notes)) => notes,
            other => {
                self.overlay = other;
                return;
            }
        };
        self.chrome_drag = None;
        self.mode = if self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_workspace_id.as_deref())
            .is_some()
        {
            ClientShellMode::Terminal
        } else {
            ClientShellMode::Navigate
        };
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ReleaseNotesDismiss(
                crate::api::schema::ReleaseNotesDismissParams {
                    version: notes.version.clone(),
                },
            ),
            PendingEndpointKind::ReleaseNotesDismiss,
            outcome,
        );
        outcome.repaint = true;
    }

    pub(super) fn current_release_notes_input_geometry(
        &self,
    ) -> Option<(Rect, Option<Rect>, crate::pane::ScrollMetrics)> {
        let notes = match self.overlay.as_ref()? {
            ClientShellOverlay::ReleaseNotes(notes) => notes,
            _ => return None,
        };
        let (cols, rows) = self.last_composed_size?;
        let outer = crate::ui::centered_popup_rect(
            Rect::new(0, 0, cols, rows),
            crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
            crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
        )?;
        let inner = Rect::new(
            outer.x.saturating_add(1),
            outer.y.saturating_add(1),
            outer.width.saturating_sub(2),
            outer.height.saturating_sub(2),
        );
        if inner.height < 8 || inner.width < 20 {
            return None;
        }
        let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
        let close = crate::ui::release_notes_close_button_rect(Rect::new(
            stack.header.x,
            stack.header.y,
            stack.header.width,
            1,
        ));
        let install_command = self
            .snapshot
            .as_deref()
            .map(|snapshot| snapshot.update_install_command.as_str())
            .unwrap_or_default();
        let metrics = crate::ui::release_notes_scroll_metrics(
            notes,
            install_command,
            stack.content,
            &self.config.palette,
        );
        let track = crate::ui::release_notes_scrollbar_rect(stack.content, metrics);
        Some((close, track, metrics))
    }

    fn current_release_notes_max_scroll(&self) -> usize {
        self.current_release_notes_input_geometry()
            .map(|(_, _, metrics)| metrics.max_offset_from_bottom)
            .unwrap_or(self.hits.release_notes_max_scroll)
    }

    pub(super) fn scroll_release_notes(&mut self, delta: isize) {
        let max_scroll = self.current_release_notes_max_scroll();
        if let Some(ClientShellOverlay::ReleaseNotes(notes)) = self.overlay.as_mut() {
            let current = usize::from(notes.scroll);
            let next = if delta.is_negative() {
                current.saturating_sub(delta.unsigned_abs())
            } else {
                current.saturating_add(delta as usize)
            }
            .min(max_scroll);
            notes.scroll = u16::try_from(next).unwrap_or(u16::MAX);
        }
    }

    pub(super) fn set_release_notes_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.current_release_notes_max_scroll();
        if let Some(ClientShellOverlay::ReleaseNotes(notes)) = self.overlay.as_mut() {
            notes.scroll =
                u16::try_from(max_scroll.saturating_sub(offset_from_bottom.min(max_scroll)))
                    .unwrap_or(u16::MAX);
        }
    }

    pub(super) fn complete_onboarding(&mut self, outcome: &mut ClientShellInput) {
        if self.snapshot.is_none() {
            return;
        }
        if let Err(error) = crate::config::update_file_at(
            &self.config.local_config_path,
            "onboarding setting",
            |content| crate::config::upsert_top_level_bool(content, "onboarding", false),
        ) {
            self.set_local_config_diagnostic(Some(error));
        }
        self.config.startup_onboarding = false;
        self.open_settings_overlay();
        self.select_settings_section(ClientSettingsSection::Integrations, outcome);
    }

    pub(super) fn open_navigator_overlay(&mut self) {
        let mut navigator = ClientNavigatorOverlay {
            query: TextEditor::default(),
            search_focused: false,
            selected: None,
            scroll: 0,
            filter: None,
        };
        let rows =
            render::client_navigator_rows(&self.endpoints, &self.active_endpoint_id, &navigator);
        navigator.selected = rows
            .iter()
            .find(|row| row.current)
            .map(|row| row.target.clone());
        self.overlay = Some(ClientShellOverlay::Navigator(navigator));
    }

    pub(super) fn move_navigator_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() else {
            return;
        };
        let rows =
            render::client_navigator_rows(&self.endpoints, &self.active_endpoint_id, navigator);
        if rows.is_empty() {
            navigator.selected = None;
            return;
        }
        let selected =
            super::aggregate_navigation::navigator_selected_index(&rows, navigator).unwrap_or(0);
        let next =
            (selected as isize + delta).clamp(0, rows.len().saturating_sub(1) as isize) as usize;
        navigator.selected = Some(rows[next].target.clone());
    }

    pub(super) fn scroll_navigator_to(&mut self, scroll: usize, viewport_rows: usize) {
        let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() else {
            return;
        };
        let rows =
            render::client_navigator_rows(&self.endpoints, &self.active_endpoint_id, navigator);
        let viewport_rows = viewport_rows.max(1);
        navigator.scroll = scroll.min(rows.len().saturating_sub(viewport_rows));
        let selected =
            super::aggregate_navigation::navigator_selected_index(&rows, navigator).unwrap_or(0);
        // Keep the selection in the dragged viewport so rendering does not snap back to it.
        let selected = selected.clamp(navigator.scroll, navigator.scroll + viewport_rows - 1);
        navigator.selected = rows.get(selected).map(|row| row.target.clone());
    }

    fn move_navigator_workspace(&mut self, forward: bool) {
        let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() else {
            return;
        };
        let rows =
            render::client_navigator_rows(&self.endpoints, &self.active_endpoint_id, navigator);
        let Some(selected) =
            super::aggregate_navigation::navigator_selected_index(&rows, navigator)
        else {
            return;
        };
        let section = rows[..=selected]
            .iter()
            .rposition(|row| !matches!(row.target, ClientNavigatorTarget::Pane { .. }))
            .unwrap_or(selected);
        let mut destinations = rows.windows(2).enumerate().filter(|(index, pair)| {
            matches!(pair[0].target, ClientNavigatorTarget::Workspace { .. })
                && matches!(pair[1].target, ClientNavigatorTarget::Pane { .. })
                && if forward {
                    *index > section
                } else {
                    *index < section
                }
        });
        let destination = if forward {
            destinations.next()
        } else {
            destinations.next_back()
        };
        if let Some((_, pair)) = destination {
            navigator.selected = Some(pair[1].target.clone());
        }
    }

    pub(super) fn accept_navigator_selection(&mut self, outcome: &mut ClientShellInput) {
        let target = self.overlay.as_ref().and_then(|overlay| match overlay {
            ClientShellOverlay::Navigator(navigator) => {
                let rows = render::client_navigator_rows(
                    &self.endpoints,
                    &self.active_endpoint_id,
                    navigator,
                );
                super::aggregate_navigation::selected_navigator_target(&rows, navigator)
            }
            _ => None,
        });
        let Some(target) = target else {
            return;
        };
        let activated = match target {
            ClientNavigatorTarget::Machine { endpoint_id } => {
                self.activate_endpoint(endpoint_id, outcome)
            }
            ClientNavigatorTarget::Workspace {
                endpoint_id,
                workspace_id,
            } => self.focus_or_activate(
                endpoint_id,
                ClientEndpointFocusTarget::Workspace(workspace_id),
                outcome,
            ),
            ClientNavigatorTarget::Pane {
                endpoint_id,
                pane_id,
            } => self.focus_or_activate(
                endpoint_id,
                ClientEndpointFocusTarget::Pane(pane_id),
                outcome,
            ),
        };
        if activated {
            self.overlay = None;
        }
        outcome.repaint = true;
    }

    pub(super) fn workspace_action_id(&self) -> Option<String> {
        self.navigate_workspace_id
            .as_ref()
            .filter(|target| {
                target.endpoint_id == self.active_endpoint_id
                    && self.navigation_target_valid(target)
            })
            .map(|target| target.workspace_id.clone())
            .or_else(|| {
                self.snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.focused_workspace_id.clone())
            })
    }

    pub(super) fn open_new_workspace_overlay(&mut self) {
        let source_workspace_id = self.workspace_action_id();
        let cwd = self.snapshot.as_deref().and_then(|snapshot| {
            let workspace_id = source_workspace_id.as_deref()?;
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .map(|workspace| workspace.new_workspace_cwd.clone())
        });
        let suggested_name = cwd
            .as_deref()
            .map(std::path::Path::new)
            .map(crate::workspace::derive_label_from_cwd)
            .unwrap_or_else(|| "workspace".to_owned());
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "new workspace",
            input: TextEditor::new(&suggested_name, true),
            target: ClientRenameTarget::NewWorkspace {
                source_workspace_id,
                cwd,
                suggested_name,
            },
        }));
    }

    pub(super) fn open_rename_workspace_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace_id) = self.workspace_action_id() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename workspace",
            input: TextEditor::new(&workspace.label, false),
            target: ClientRenameTarget::Workspace { workspace_id },
        }));
    }

    pub(super) fn open_new_tab_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace_id) = snapshot.focused_workspace_id.clone() else {
            return;
        };
        let default_name = (snapshot
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace_id)
            .count()
            + 1)
        .to_string();
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "new tab",
            input: TextEditor::new(&default_name, true),
            target: ClientRenameTarget::NewTab {
                workspace_id,
                default_name,
            },
        }));
    }

    pub(super) fn open_rename_tab_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(tab_id) = snapshot.focused_tab_id.as_deref() else {
            return;
        };
        let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id) else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename tab",
            input: TextEditor::new(&tab.label, false),
            target: ClientRenameTarget::Tab {
                tab_id: tab.tab_id.clone(),
                auto_name: !tab.custom_label,
                original_name: tab.label.clone(),
            },
        }));
    }

    pub(super) fn open_rename_pane_overlay(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(pane_id) = snapshot.focused_pane_id.as_deref() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename pane",
            input: TextEditor::new(
                pane.label.as_deref().unwrap_or_default(),
                pane.label.is_none(),
            ),
            target: ClientRenameTarget::Pane {
                pane_id: pane.pane_id.clone(),
            },
        }));
    }

    pub(super) fn insert_overlay_text(&mut self, text: &str) -> bool {
        if self.insert_filter_text(text) {
            return true;
        }
        if self.insert_worktree_overlay_text(text) {
            return true;
        }
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Rename(rename)) => {
                rename.input.insert(text);
                true
            }
            Some(ClientShellOverlay::Help(help)) if help.search_focused => {
                if help.query.insert(text) {
                    help.scroll = 0;
                }
                true
            }
            Some(ClientShellOverlay::Navigator(navigator)) if navigator.search_focused => {
                if navigator.query.insert(text) {
                    navigator.filter = None;
                    navigator.selected = None;
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn route_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        use crossterm::event::KeyModifiers;

        if matches!(self.overlay, Some(ClientShellOverlay::Onboarding)) {
            if matches!(
                key.code,
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')
            ) {
                self.complete_onboarding(outcome);
            }
            return;
        }

        if matches!(
            self.overlay,
            Some(ClientShellOverlay::ProductAnnouncement(_))
        ) {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.dismiss_product_announcement(outcome),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll_product_announcement(-1);
                    outcome.repaint = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll_product_announcement(1);
                    outcome.repaint = true;
                }
                KeyCode::PageUp => {
                    self.scroll_product_announcement(-8);
                    outcome.repaint = true;
                }
                KeyCode::PageDown => {
                    self.scroll_product_announcement(8);
                    outcome.repaint = true;
                }
                KeyCode::Home => {
                    if let Some(ClientShellOverlay::ProductAnnouncement(announcement)) =
                        self.overlay.as_mut()
                    {
                        announcement.scroll = 0;
                    }
                    outcome.repaint = true;
                }
                KeyCode::End => {
                    if let Some(ClientShellOverlay::ProductAnnouncement(announcement)) =
                        self.overlay.as_mut()
                    {
                        announcement.scroll =
                            u16::try_from(self.hits.product_announcement_max_scroll)
                                .unwrap_or(u16::MAX);
                    }
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::ReleaseNotes(_))) {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.dismiss_release_notes(outcome),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.scroll_release_notes(-1);
                    outcome.repaint = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll_release_notes(1);
                    outcome.repaint = true;
                }
                KeyCode::PageUp => {
                    self.scroll_release_notes(-8);
                    outcome.repaint = true;
                }
                KeyCode::PageDown => {
                    self.scroll_release_notes(8);
                    outcome.repaint = true;
                }
                KeyCode::Home => {
                    if let Some(ClientShellOverlay::ReleaseNotes(notes)) = self.overlay.as_mut() {
                        notes.scroll = 0;
                    }
                    outcome.repaint = true;
                }
                KeyCode::End => {
                    let max_scroll = self.current_release_notes_max_scroll();
                    if let Some(ClientShellOverlay::ReleaseNotes(notes)) = self.overlay.as_mut() {
                        notes.scroll = u16::try_from(max_scroll).unwrap_or(u16::MAX);
                    }
                    outcome.repaint = true;
                }
                _ => {}
            }
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::Usage(_))) {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                KeyCode::Char('r') => self.refresh_usage(outcome),
                _ => {}
            }
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::NotificationLog(_))) {
            // A bookmark row's menu is open: Enter removes, Esc or any other
            // key closes only the menu.
            if matches!(&self.overlay, Some(ClientShellOverlay::NotificationLog(log)) if log.menu.is_some())
            {
                if let Some(menu) = self.take_bookmark_menu() {
                    if key.code == KeyCode::Enter {
                        self.remove_bookmark(menu.tab_id, outcome);
                    }
                }
                outcome.repaint = true;
                return;
            }
            match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_notification_log_selection(-1);
                    outcome.repaint = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_notification_log_selection(1);
                    outcome.repaint = true;
                }
                KeyCode::Enter => {
                    let rows = self.notification_log_rows();
                    if let Some(highlighted) = self.notification_log_highlighted(&rows) {
                        self.activate_notification_log_row(highlighted, outcome);
                    }
                }
                KeyCode::Delete | KeyCode::Backspace | KeyCode::Char('x') => {
                    self.remove_highlighted_bookmark(outcome);
                }
                _ => {}
            }
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::GlobalMenu(_))) {
            match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_global_menu_selection(-1);
                    outcome.repaint = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_global_menu_selection(1);
                    outcome.repaint = true;
                }
                KeyCode::Enter => {
                    let highlighted = match self.overlay.as_ref() {
                        Some(ClientShellOverlay::GlobalMenu(menu)) => menu.highlighted,
                        _ => return,
                    };
                    self.activate_global_menu_item(highlighted, outcome);
                }
                _ => {}
            }
            return;
        }

        if self.route_settings_key(key, outcome) {
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::ContextMenu(_))) {
            match key.code {
                KeyCode::Esc => {
                    self.overlay = None;
                    outcome.repaint = true;
                }
                KeyCode::Up => {
                    self.move_context_menu_selection(-1);
                    outcome.repaint = true;
                }
                KeyCode::Down => {
                    self.move_context_menu_selection(1);
                    outcome.repaint = true;
                }
                KeyCode::Enter => {
                    let highlighted = match self.overlay.as_ref() {
                        Some(ClientShellOverlay::ContextMenu(menu)) => menu.highlighted,
                        _ => return,
                    };
                    self.activate_context_menu_item(highlighted, outcome);
                }
                _ => {}
            }
            return;
        }

        if self.route_worktree_overlay_key(key, outcome) {
            return;
        }
        if matches!(self.overlay, Some(ClientShellOverlay::Navigator(_))) {
            let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
            let search_focused = matches!(
                self.overlay,
                Some(ClientShellOverlay::Navigator(ClientNavigatorOverlay {
                    search_focused: true,
                    ..
                }))
            );
            if code == KeyCode::Esc {
                if search_focused {
                    if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                        navigator.search_focused = false;
                    }
                } else {
                    self.overlay = None;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Enter {
                self.accept_navigator_selection(outcome);
                return;
            }
            if search_focused {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    if let Some(content_changed) = navigator.query.handle_key(key) {
                        if content_changed {
                            navigator.filter = None;
                            navigator.selected = None;
                        }
                        outcome.repaint = true;
                        return;
                    }
                }
                if code == KeyCode::Up
                    || code == KeyCode::Char('p') && modifiers == KeyModifiers::CONTROL
                {
                    self.move_navigator_selection(-1);
                    outcome.repaint = true;
                    return;
                }
                if code == KeyCode::Down
                    || code == KeyCode::Char('n') && modifiers == KeyModifiers::CONTROL
                {
                    self.move_navigator_selection(1);
                    outcome.repaint = true;
                    return;
                }
                return;
            }
            if matches!(code, KeyCode::Left | KeyCode::Right) && modifiers.is_empty() {
                self.move_navigator_workspace(code == KeyCode::Right);
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Backspace && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    if navigator.filter.take().is_some() {
                        navigator.selected = None;
                    }
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Home && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.selected = None;
                    navigator.scroll = 0;
                }
                outcome.repaint = true;
                return;
            }
            if matches!(code, KeyCode::End | KeyCode::Char('G')) && modifiers.is_empty() {
                let last = self.overlay.as_ref().and_then(|overlay| match overlay {
                    ClientShellOverlay::Navigator(navigator) => render::client_navigator_rows(
                        &self.endpoints,
                        &self.active_endpoint_id,
                        navigator,
                    )
                    .last()
                    .map(|row| row.target.clone()),
                    _ => None,
                });
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.selected = last;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('/') && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.search_focused = true;
                    navigator.filter = None;
                }
                outcome.repaint = true;
                return;
            }
            if matches!(code, KeyCode::Down | KeyCode::Char('j')) && modifiers.is_empty() {
                self.move_navigator_selection(1);
                outcome.repaint = true;
                return;
            }
            if matches!(code, KeyCode::Up | KeyCode::Char('k')) && modifiers.is_empty() {
                self.move_navigator_selection(-1);
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL) {
                self.move_navigator_selection(8);
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('u') && modifiers.contains(KeyModifiers::CONTROL) {
                self.move_navigator_selection(-8);
                outcome.repaint = true;
                return;
            }
            if let Some(filter) = match code {
                KeyCode::Char('b') if modifiers.is_empty() => Some(ClientNavigatorFilter::Blocked),
                KeyCode::Char('w') if modifiers.is_empty() => Some(ClientNavigatorFilter::Working),
                KeyCode::Char('i') if modifiers.is_empty() => Some(ClientNavigatorFilter::Idle),
                KeyCode::Char('d') if modifiers.is_empty() => Some(ClientNavigatorFilter::Done),
                _ => None,
            } {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.query.clear();
                    navigator.filter = Some(filter);
                    navigator.selected = None;
                }
                outcome.repaint = true;
                return;
            }
            if code == KeyCode::Char('a') && modifiers.is_empty() {
                if let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.as_mut() {
                    navigator.query.clear();
                    navigator.filter = None;
                    navigator.selected = None;
                }
                outcome.repaint = true;
                return;
            }
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::Help(_))) {
            let text_character = crate::input::keybind_help_text_char(key);
            let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
            let search_focused = matches!(
                self.overlay,
                Some(ClientShellOverlay::Help(ClientHelpOverlay {
                    search_focused: true,
                    ..
                }))
            );
            if search_focused {
                if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                    if let Some(content_changed) = help.query.handle_key(key) {
                        if content_changed {
                            help.scroll = 0;
                        }
                        outcome.repaint = true;
                        return;
                    }
                }
                match code {
                    KeyCode::Esc => {
                        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                            help.search_focused = false;
                            help.query.clear();
                            help.scroll = 0;
                        }
                    }
                    KeyCode::Enter => self.overlay = None,
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Char('n' | 'p')
                        if !matches!(code, KeyCode::Char(_))
                            || modifiers == KeyModifiers::CONTROL =>
                    {
                        let delta = match code {
                            KeyCode::Up | KeyCode::Char('p') => -1,
                            KeyCode::Down | KeyCode::Char('n') => 1,
                            KeyCode::PageUp => -8,
                            KeyCode::PageDown => 8,
                            _ => unreachable!(),
                        };
                        if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                            help.scroll = help
                                .scroll
                                .saturating_add_signed(delta)
                                .min(self.hits.help_max_scroll);
                        }
                    }
                    _ => {}
                }
                outcome.repaint = true;
                return;
            }

            match code {
                KeyCode::Esc | KeyCode::Enter => self.overlay = None,
                KeyCode::Home => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        help.scroll = 0;
                    }
                }
                KeyCode::End => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        help.scroll = self.hits.help_max_scroll;
                    }
                }
                KeyCode::Up
                | KeyCode::Char('k')
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::PageUp
                | KeyCode::PageDown => {
                    let delta = match code {
                        KeyCode::Up | KeyCode::Char('k') => -1,
                        KeyCode::Down | KeyCode::Char('j') => 1,
                        KeyCode::PageUp => -8,
                        KeyCode::PageDown => 8,
                        _ => unreachable!(),
                    };
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        help.scroll = help
                            .scroll
                            .saturating_add_signed(delta)
                            .min(self.hits.help_max_scroll);
                    }
                }
                _ if text_character == Some('/') => {
                    if let Some(ClientShellOverlay::Help(help)) = self.overlay.as_mut() {
                        help.search_focused = true;
                        help.scroll = 0;
                    }
                }
                _ if text_character == Some('?') => self.overlay = None,
                _ => {}
            }
            outcome.repaint = true;
            return;
        }

        if matches!(self.overlay, Some(ClientShellOverlay::ConfirmClose(_))) {
            if key.code == KeyCode::Enter {
                self.accept_close_confirmation(outcome);
            } else if key.code == KeyCode::Esc {
                // Closing a tab or a pane began in the terminal: cancelling it
                // must not leave the space selected as if navigating. A
                // workspace close returns to navigate mode, where it began.
                let from_terminal = matches!(
                    &self.overlay,
                    Some(ClientShellOverlay::ConfirmClose(confirm))
                        if confirm.tab_target.is_some() || confirm.pane_target.is_some()
                );
                self.overlay = None;
                if !from_terminal {
                    self.mode = ClientShellMode::Navigate;
                    self.navigate_workspace_id = self.focused_navigation_target();
                    self.reveal_navigation_workspace = true;
                }
                outcome.repaint = true;
            }
            return;
        }

        let Some(ClientShellOverlay::Rename(rename)) = self.overlay.as_mut() else {
            return;
        };
        if key.code == KeyCode::Enter {
            self.save_rename_overlay(outcome);
            return;
        }
        if key.code == KeyCode::Esc {
            self.overlay = None;
            outcome.repaint = true;
            return;
        }
        if key
            .generated_text
            .as_deref()
            .is_some_and(|text| !text.is_empty())
        {
            outcome.repaint |= rename.input.handle_key(key).is_some();
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            rename.input.clear();
            outcome.repaint = true;
            return;
        }
        if key.code == KeyCode::Backspace && key.modifiers.contains(KeyModifiers::SUPER) {
            rename.input.clear();
            outcome.repaint = true;
            return;
        }
        if rename.input.handle_key(key).is_some() {
            outcome.repaint = true;
        }
    }

    pub(super) fn save_rename_overlay(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Rename(rename)) = self.overlay.take() else {
            return;
        };
        let trimmed = rename.input.trim();
        let method = match rename.target {
            ClientRenameTarget::NewWorkspace {
                source_workspace_id,
                cwd,
                suggested_name,
            } => Some(crate::api::schema::Method::WorkspaceCreate(
                crate::api::schema::WorkspaceCreateParams {
                    source_workspace_id,
                    cwd,
                    focus: true,
                    label: (!trimmed.is_empty() && trimmed != suggested_name)
                        .then(|| trimmed.to_owned()),
                    env: Default::default(),
                },
            )),
            ClientRenameTarget::Workspace { workspace_id } => (!trimmed.is_empty()).then(|| {
                crate::api::schema::Method::WorkspaceRename(
                    crate::api::schema::WorkspaceRenameParams {
                        workspace_id,
                        label: trimmed.to_owned(),
                    },
                )
            }),
            ClientRenameTarget::NewTab {
                workspace_id,
                default_name,
            } => Some(crate::api::schema::Method::TabCreate(
                crate::api::schema::TabCreateParams {
                    workspace_id: Some(workspace_id),
                    cwd: None,
                    focus: true,
                    label: (!trimmed.is_empty() && trimmed != default_name)
                        .then(|| trimmed.to_owned()),
                    env: Default::default(),
                },
            )),
            ClientRenameTarget::Tab {
                tab_id,
                auto_name,
                original_name,
            } => (!(trimmed.is_empty() || auto_name && trimmed == original_name)).then(|| {
                crate::api::schema::Method::TabRename(crate::api::schema::TabRenameParams {
                    tab_id,
                    label: trimmed.to_owned(),
                })
            }),
            ClientRenameTarget::Pane { pane_id } => Some(crate::api::schema::Method::PaneRename(
                crate::api::schema::PaneRenameParams {
                    pane_id,
                    label: Some(trimmed.to_owned()),
                },
            )),
        };
        if let Some(method) = method {
            self.push_endpoint_method(method, outcome);
        }
        outcome.repaint = true;
    }

    pub(super) fn request_workspace_close(
        &mut self,
        workspace_id: String,
        close_group: Option<bool>,
        outcome: &mut ClientShellInput,
    ) {
        let running = self.running_summary(|snapshot| {
            super::close_impact::workspaces_running_work(snapshot, &[workspace_id.as_str()])
        });
        if self.config.confirm_close || running.is_some() {
            self.open_close_confirmation(workspace_id, None, close_group);
            return;
        }
        let close_group = close_group.unwrap_or_else(|| {
            self.snapshot.as_deref().is_some_and(|snapshot| {
                snapshot.workspaces.iter().any(|workspace| {
                    workspace.workspace_id == workspace_id
                        && super::sidebar::workspace_close_is_group(snapshot, workspace)
                })
            })
        });
        self.push_endpoint_method(
            crate::api::schema::Method::WorkspaceClose(crate::api::schema::WorkspaceCloseParams {
                workspace_id,
                close_group,
            }),
            outcome,
        );
    }

    /// What the close would stop, when the user wants to be asked about it.
    fn running_summary(
        &self,
        running_work: impl FnOnce(&crate::protocol::ClientShellSnapshot) -> Vec<String>,
    ) -> Option<String> {
        if !self.config.confirm_close_running {
            return None;
        }
        let snapshot = self.snapshot.as_deref()?;
        // Name the tabs as the sidebar does (their task), not by number.
        let mut named = snapshot.clone();
        for tab in &mut named.tabs {
            tab.label = super::render::tabs::sidebar_tab_label(tab, snapshot, &self.config);
        }
        super::close_impact::summary(&running_work(&named))
    }

    /// A tab's name as the sidebar shows it.
    fn tab_display_label(&self, tab_id: &str) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
        Some(super::render::tabs::sidebar_tab_label(
            tab,
            snapshot,
            &self.config,
        ))
    }

    pub(super) fn request_tab_close(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        let running = self.running_summary(|snapshot| {
            let mut tab_ids = vec![tab_id.as_str()];
            tab_ids.extend(
                super::tab_groups::child_tabs(snapshot, &tab_id)
                    .into_iter()
                    .map(|tab| tab.tab_id.as_str()),
            );
            super::close_impact::tabs_running_work(snapshot, &tab_ids)
        });
        if self.request_parent_tab_close(&tab_id, running.clone(), outcome) {
            return;
        }
        let workspace_id = self.snapshot.as_deref().and_then(|snapshot| {
            let target = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
            ((self.config.confirm_close || running.is_some())
                && !snapshot
                    .tabs
                    .iter()
                    .any(|tab| tab.workspace_id == target.workspace_id && tab.tab_id != tab_id))
            .then(|| target.workspace_id.clone())
        });
        if let Some(workspace_id) = workspace_id {
            if self.open_close_confirmation(workspace_id, Some(tab_id.clone()), None) {
                outcome.repaint = true;
                return;
            }
        }
        if let Some(running) = running {
            if self.open_running_tab_confirmation(&tab_id, running) {
                outcome.repaint = true;
                return;
            }
        }
        self.push_tab_close(tab_id, outcome);
    }

    fn open_running_tab_confirmation(&mut self, tab_id: &str, running: String) -> bool {
        let Some(target) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))
        else {
            return false;
        };
        let label = self
            .tab_display_label(tab_id)
            .unwrap_or_else(|| target.label.clone());
        let workspace_id = target.workspace_id.clone();
        let Some(workspace) = self.navigation_target(&self.active_endpoint_id, &workspace_id)
        else {
            return false;
        };
        // The line above already names the tab.
        let running = running.replace(&format!(" in {label}"), "");
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                workspace_id,
                close_group: false,
                tab_target: Some(ClientTabCloseConfirmation {
                    tab_id: tab_id.to_owned(),
                    workspace,
                    children: Vec::new(),
                    children_only: false,
                }),
                pane_target: None,
                title: "Close tab?".to_owned(),
                detail: label,
                running: Some(running),
            },
        ));
        true
    }

    /// Closes a pane, asking first when that would stop running work.
    pub(super) fn request_pane_close(&mut self, pane_id: String, outcome: &mut ClientShellInput) {
        let running = self
            .running_summary(|snapshot| super::close_impact::pane_running_work(snapshot, &pane_id));
        let target = self.snapshot.as_deref().and_then(|snapshot| {
            let pane = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id)?;
            let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == pane.tab_id)?;
            Some((
                pane.workspace_id.clone(),
                pane.label.clone(),
                self.tab_display_label(&tab.tab_id)
                    .unwrap_or_else(|| tab.label.clone()),
            ))
        });
        if let (Some(running), Some((workspace_id, pane_label, tab_label))) = (running, target) {
            self.overlay = Some(ClientShellOverlay::ConfirmClose(
                ClientConfirmCloseOverlay {
                    workspace_id,
                    close_group: false,
                    tab_target: None,
                    pane_target: Some(pane_id),
                    title: "Close pane?".to_owned(),
                    detail: match pane_label {
                        Some(pane_label) => format!("{pane_label} in {tab_label}"),
                        None => tab_label.clone(),
                    },
                    // The line above already names the tab.
                    running: Some(running.replace(&format!(" in {tab_label}"), "")),
                },
            ));
            outcome.repaint = true;
            return;
        }
        self.push_endpoint_method(
            crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget { pane_id }),
            outcome,
        );
    }

    /// A tab with child tabs (usually jobs) closes only together with them,
    /// after the user confirms with a summary of their statuses.
    fn request_parent_tab_close(
        &mut self,
        tab_id: &str,
        running: Option<String>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        let children = super::tab_groups::child_tabs(snapshot, tab_id);
        if children.is_empty() {
            return false;
        }
        let summary = super::tab_groups::children_summary(&children);
        let children = children
            .iter()
            .map(|tab| tab.tab_id.clone())
            .collect::<Vec<_>>();
        let Some(target) = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id) else {
            return false;
        };
        let label = target.label.clone();
        let workspace_id = target.workspace_id.clone();
        if !self.config.confirm_close && running.is_none() {
            for child in children {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabClose(crate::api::schema::TabTarget {
                        tab_id: child,
                    }),
                    outcome,
                );
            }
            self.push_endpoint_method(
                crate::api::schema::Method::TabClose(crate::api::schema::TabTarget {
                    tab_id: tab_id.to_owned(),
                }),
                outcome,
            );
            return true;
        }
        let Some(workspace) = self.navigation_target(&self.active_endpoint_id, &workspace_id)
        else {
            return false;
        };
        let count = children.len();
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                workspace_id,
                close_group: false,
                tab_target: Some(ClientTabCloseConfirmation {
                    tab_id: tab_id.to_owned(),
                    workspace,
                    children,
                    children_only: false,
                }),
                pane_target: None,
                title: "Close tab and its child tabs?".to_owned(),
                detail: format!(
                    "{label} — {count} child {}: {summary}",
                    if count == 1 { "tab" } else { "tabs" }
                ),
                running,
            },
        ));
        outcome.repaint = true;
        true
    }

    /// Asks before stopping a tab's running jobs, naming them; closing their
    /// tabs stops them. The tab itself stays.
    pub(super) fn confirm_stop_running_jobs(
        &mut self,
        tab_id: &str,
        outcome: &mut ClientShellInput,
    ) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let running = super::tab_groups::child_tabs(snapshot, tab_id)
            .into_iter()
            .filter(|child| child.status == Some(crate::api::schema::TabStatus::Running))
            .collect::<Vec<_>>();
        if running.is_empty() {
            return;
        }
        let names = running
            .iter()
            .map(|child| child.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let children = running
            .iter()
            .map(|child| child.tab_id.clone())
            .collect::<Vec<_>>();
        let Some(target) = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id) else {
            return;
        };
        let (label, workspace_id) = (target.label.clone(), target.workspace_id.clone());
        let Some(workspace) = self.navigation_target(&self.active_endpoint_id, &workspace_id)
        else {
            return;
        };
        let count = children.len();
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                workspace_id,
                close_group: false,
                tab_target: Some(ClientTabCloseConfirmation {
                    tab_id: tab_id.to_owned(),
                    workspace,
                    children,
                    children_only: true,
                }),
                pane_target: None,
                title: format!(
                    "Stop {count} running {}?",
                    if count == 1 { "job" } else { "jobs" }
                ),
                detail: format!("{label}: {names}"),
                running: None,
            },
        ));
        outcome.repaint = true;
    }

    pub(super) fn accept_close_confirmation(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::ConfirmClose(confirm)) = self.overlay.take() else {
            return;
        };
        outcome.repaint = true;
        if let Some(pane_id) = confirm.pane_target {
            if !self
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.panes.iter().any(|pane| pane.pane_id == pane_id))
            {
                self.receive_endpoint_unavailable(
                    "Close target changed; try closing the pane again".into(),
                );
                return;
            }
            self.push_endpoint_method(
                crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget { pane_id }),
                outcome,
            );
            return;
        }
        let method = if let Some(target) = confirm.tab_target {
            if target.workspace.endpoint_id != self.active_endpoint_id
                || !self.navigation_target_valid(&target.workspace)
                || !self.snapshot.as_deref().is_some_and(|snapshot| {
                    snapshot.tabs.iter().any(|tab| {
                        tab.tab_id == target.tab_id
                            && tab.workspace_id == target.workspace.workspace_id
                    })
                })
            {
                self.receive_endpoint_unavailable(
                    "Close target changed; try closing the tab again".into(),
                );
                return;
            }
            let children_only = target.children_only;
            for child in target.children {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabClose(crate::api::schema::TabTarget {
                        tab_id: child,
                    }),
                    outcome,
                );
            }
            if children_only {
                return;
            }
            self.push_tab_close(target.tab_id, outcome);
            return;
        } else {
            crate::api::schema::Method::WorkspaceClose(crate::api::schema::WorkspaceCloseParams {
                workspace_id: confirm.workspace_id,
                close_group: confirm.close_group,
            })
        };
        self.push_endpoint_method(method, outcome);
    }

    pub(super) fn open_confirm_close_overlay(&mut self, workspace_id: String) {
        self.open_close_confirmation(workspace_id, None, None);
    }

    fn open_close_confirmation(
        &mut self,
        workspace_id: String,
        tab_id: Option<String>,
        close_group: Option<bool>,
    ) -> bool {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return false;
        };
        let group_key = workspace
            .worktree
            .as_ref()
            .filter(|_| {
                close_group.unwrap_or_else(|| {
                    super::sidebar::workspace_close_is_group(snapshot, workspace)
                })
            })
            .map(|worktree| worktree.key.as_str());
        let group = group_key
            .map(|key| {
                snapshot
                    .workspaces
                    .iter()
                    .filter(|member| {
                        member
                            .worktree
                            .as_ref()
                            .is_some_and(|worktree| worktree.key == key)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![workspace]);
        let closes_group = group.len() > 1;
        // Keep parent-group tab closes on the existing server confirmation path.
        if tab_id.is_some() && closes_group {
            return false;
        }
        let tab_target = if let Some(tab_id) = tab_id {
            let Some(workspace) = self.navigation_target(&self.active_endpoint_id, &workspace_id)
            else {
                return false;
            };
            Some(ClientTabCloseConfirmation {
                tab_id,
                workspace,
                children: Vec::new(),
                children_only: false,
            })
        } else {
            None
        };
        let running = if self.config.confirm_close_running {
            let workspace_ids = group
                .iter()
                .map(|member| member.workspace_id.as_str())
                .collect::<Vec<_>>();
            super::close_impact::summary(&super::close_impact::workspaces_running_work(
                snapshot,
                &workspace_ids,
            ))
        } else {
            None
        };
        let pane_count = group
            .iter()
            .map(|member| {
                snapshot
                    .panes
                    .iter()
                    .filter(|pane| pane.workspace_id == member.workspace_id)
                    .count()
            })
            .sum::<usize>();
        let panes = if pane_count == 1 {
            "1 pane".to_owned()
        } else {
            format!("{pane_count} panes")
        };
        let scope = if closes_group {
            format!("{} workspaces, {panes}", group.len())
        } else {
            panes
        };
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                workspace_id,
                close_group: closes_group,
                tab_target,
                pane_target: None,
                running,
                title: if closes_group {
                    "Close worktree group?".to_owned()
                } else {
                    "Close workspace?".to_owned()
                },
                detail: format!("{} — {scope}", workspace.label),
            },
        ));
        true
    }
}
