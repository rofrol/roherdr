use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateChildParams, TabCreateParams,
    TabListParams, TabMoveParams, TabRenameParams, TabSetParentParams, TabSetStatusParams,
    TabTarget,
};
use crate::app::{App, Mode};

use super::responses::{encode_error, encode_success};

/// Where `create_tab_in_workspace` puts the new tab.
#[derive(Clone, Copy)]
pub(super) enum NewTabPlace {
    End,
    /// Nested under this top-level tab.
    Child(usize),
    /// Right after this tab's group.
    After(usize),
}

impl App {
    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(_) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            self.tab_list_info(ws_idx)
        } else {
            let mut tabs = Vec::new();
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab_idx in 0..ws.tabs.len() {
                    if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            tabs
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        let ws_idx = if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        self.create_tab_in_workspace(id, ws_idx, NewTabPlace::End, cwd, focus, label, env)
    }

    pub(super) fn handle_tab_create_after(
        &mut self,
        id: String,
        params: crate::api::schema::TabCreateAfterParams,
    ) -> String {
        let crate::api::schema::TabCreateAfterParams {
            after_tab_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        let Some((ws_idx, after_idx)) = self.parse_tab_id(&after_tab_id) else {
            return tab_not_found(id, &after_tab_id);
        };
        self.create_tab_in_workspace(
            id,
            ws_idx,
            NewTabPlace::After(after_idx),
            cwd,
            focus,
            label,
            env,
        )
    }

    /// What reopening the tab needs, taken before its panes are torn down. A
    /// job tab is not kept: it cannot come back without its job.
    pub(super) fn closed_tab_record(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::app::ClosedTabRecord> {
        let ws = self.state.workspaces.get(ws_idx)?;
        if ws.tab_parent_index(tab_idx).is_some() {
            return None;
        }
        let tab = ws.tabs.get(tab_idx)?;
        Some(crate::app::ClosedTabRecord {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: ws.id.clone(),
            snapshot: crate::persist::capture_tab(
                tab,
                &self.state.terminals,
                &self.terminal_runtimes,
            ),
        })
    }

    /// Keeps a closed tab for `tab.reopen_closed`, the oldest going first.
    pub(super) fn keep_closed_tab(&mut self, record: Option<crate::app::ClosedTabRecord>) {
        if let Some(record) = record {
            self.closed_tab_history.push_back(record);
            while self.closed_tab_history.len() > crate::app::MAX_CLOSED_TAB_HISTORY {
                self.closed_tab_history.pop_front();
            }
        }
    }

    /// Opens a tab closed through `tab.close` again: its layout, the
    /// directories and names of its panes, and its agents resumed where their
    /// sessions can be. Its entry is used up, so it never comes back twice.
    pub(super) fn handle_tab_reopen_closed(
        &mut self,
        id: String,
        params: crate::api::schema::TabReopenClosedParams,
    ) -> String {
        let Some(at) = self
            .closed_tab_history
            .iter()
            .rposition(|record| record.tab_id == params.tab_id)
        else {
            return encode_error(
                id,
                "closed_tab_not_found",
                format!("closed tab {} is not kept", params.tab_id),
            );
        };
        let Some(record) = self.closed_tab_history.remove(at) else {
            return encode_error(id, "closed_tab_not_found", "closed tab is not kept");
        };
        let Some(ws_idx) = self.parse_workspace_id(&record.workspace_id) else {
            return workspace_not_found(id, &record.workspace_id);
        };
        let (rows, cols) = self.state.new_pane_size(crate::ui::NewPanePlacement::Alone);
        // Agent sessions running or about to resume anywhere: a reopened
        // pane must not resume one of them again.
        let live_sessions = self
            .state
            .terminals
            .values()
            .flat_map(|terminal| {
                let pending = terminal
                    .pending_agent_resume_plan
                    .as_ref()
                    .map(|plan| plan.dedupe_key.clone());
                // The session the agent reports, as a session save reads it.
                let reported = terminal.hook_authority.as_ref().and_then(|authority| {
                    let session_ref = authority.session_ref.as_ref()?;
                    Some(crate::agent_resume::dedupe_key(
                        &authority.source,
                        &authority.agent_label,
                        session_ref,
                    ))
                });
                let persisted = terminal.persisted_agent_session.as_ref().map(|session| {
                    crate::agent_resume::dedupe_key(
                        &session.source,
                        &session.agent,
                        &session.session_ref,
                    )
                });
                pending.into_iter().chain(reported).chain(persisted)
            })
            .collect::<std::collections::HashSet<_>>();
        let ws = &mut self.state.workspaces[ws_idx];
        let number = ws.next_public_tab_number;
        ws.next_public_tab_number += 1;
        // New public pane numbers, in the saved panes' order.
        let mut old_raw = record.snapshot.panes.keys().copied().collect::<Vec<_>>();
        old_raw.sort_unstable();
        let numbers = old_raw
            .into_iter()
            .map(|raw| {
                let number = ws.next_public_pane_number;
                ws.next_public_pane_number += 1;
                (raw, number)
            })
            .collect::<std::collections::HashMap<_, _>>();
        let public_ids = numbers
            .iter()
            .map(|(raw, number)| {
                (
                    *raw,
                    crate::workspace::public_pane_id_for_number(&record.workspace_id, *number),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let Some((mut tab, terminals, runtimes, reverse_ids)) = crate::persist::restore_closed_tab(
            &record.snapshot,
            number,
            &record.workspace_id,
            &public_ids,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.resume_agents_on_restore,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
            live_sessions,
        ) else {
            return encode_error(
                id,
                "tab_create_failed",
                "the closed tab could not be rebuilt",
            );
        };
        tab.number = number;
        let root_pane = tab.root_pane;
        let ws = &mut self.state.workspaces[ws_idx];
        for pane_id in tab.layout.pane_ids() {
            let public = reverse_ids
                .get(&pane_id)
                .and_then(|raw| numbers.get(raw))
                .copied()
                .unwrap_or_else(|| {
                    let number = ws.next_public_pane_number;
                    ws.next_public_pane_number += 1;
                    number
                });
            ws.public_pane_numbers.insert(pane_id, public);
        }
        let pane_ids = tab.layout.pane_ids();
        ws.tabs.push(tab);
        let mut tab_idx = ws.tabs.len() - 1;
        // After the group of the tab that stood before it, else first.
        let insert = match params.after_tab_id.as_deref().and_then(|after| {
            ws.tabs.iter().position(|tab| {
                crate::workspace::public_tab_id_for_number(&ws.id, tab.number) == after
            })
        }) {
            Some(after_idx) => {
                let top = ws.tab_parent_index(after_idx).unwrap_or(after_idx);
                ws.tab_children(top)
                    .into_iter()
                    .chain(std::iter::once(top))
                    .max()
                    .unwrap_or(top)
                    + 1
            }
            None => 0,
        };
        ws.move_tab(tab_idx, insert);
        if let Some(idx) = ws.tabs.iter().position(|tab| tab.root_pane == root_pane) {
            tab_idx = idx;
        }
        for terminal in terminals {
            self.state.terminals.insert(terminal.id.clone(), terminal);
        }
        for pane_id in pane_ids {
            self.state.remove_alias_shadowed_by_new_pane(pane_id);
        }
        for (terminal_id, runtime) in runtimes {
            self.terminal_runtimes.insert(terminal_id, runtime);
        }
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        self.emit_tab_created_events(ws_idx, tab_idx);
        match self.tab_created_result(ws_idx, tab_idx) {
            Some(result) => encode_success(id, result),
            None => encode_error(id, "tab_create_failed", "the reopened tab is incomplete"),
        }
    }

    pub(super) fn handle_tab_create_child(
        &mut self,
        id: String,
        params: TabCreateChildParams,
    ) -> String {
        let TabCreateChildParams {
            parent_tab_id,
            cwd,
            focus,
            label,
            env,
        } = params;
        let Some((ws_idx, parent_idx)) = self.parse_tab_id(&parent_tab_id) else {
            return tab_not_found(id, &parent_tab_id);
        };
        // Checked before creating, so a bad parent leaves no stray tab behind.
        if self.state.workspaces[ws_idx]
            .tab_parent_index(parent_idx)
            .is_some()
        {
            return encode_error(
                id,
                "tab_create_failed",
                "the parent must be a top-level tab",
            );
        }
        self.create_tab_in_workspace(
            id,
            ws_idx,
            NewTabPlace::Child(parent_idx),
            cwd,
            focus,
            label,
            env,
        )
    }

    /// Creates a tab in a workspace and puts it in its place (nested under a
    /// parent, or after a tab's group) before any client sees it, so it never
    /// shows at the end first.
    #[allow(clippy::too_many_arguments)] // the fields of the create requests
    pub(super) fn create_tab_in_workspace(
        &mut self,
        id: String,
        ws_idx: usize,
        place: NewTabPlace,
        cwd: Option<String>,
        focus: bool,
        label: Option<String>,
        env: std::collections::HashMap<String, String>,
    ) -> String {
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            self.resolve_new_terminal_cwd(self.focused_pane_cwd_in_workspace(ws_idx))
        });
        let (rows, cols) = self.state.new_pane_size(crate::ui::NewPanePlacement::Alone);
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let extra_env = match super::env::normalize_launch_env(env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            });
        match result {
            Ok((mut tab_idx, terminal, runtime)) => {
                let ws = &mut self.state.workspaces[ws_idx];
                let root_pane = ws.tabs[tab_idx].root_pane;
                match place {
                    NewTabPlace::End => {}
                    NewTabPlace::Child(parent_idx) => {
                        if let Err(err) = ws.set_tab_parent(tab_idx, Some(parent_idx)) {
                            tracing::warn!(err, "could not nest a new tab under its parent");
                        }
                    }
                    NewTabPlace::After(after_idx) => {
                        // After the tab's whole group (from a job tab, its
                        // parent's), never between a parent and its jobs.
                        let top = ws.tab_parent_index(after_idx).unwrap_or(after_idx);
                        let group_end = ws
                            .tab_children(top)
                            .into_iter()
                            .chain(std::iter::once(top))
                            .max()
                            .unwrap_or(top);
                        ws.move_tab(tab_idx, group_end + 1);
                    }
                }
                // Placing reorders tabs; find this one again by its identity.
                if let Some(idx) = ws.tabs.iter().position(|tab| tab.root_pane == root_pane) {
                    tab_idx = idx;
                }
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                if let Some(label) = label {
                    let workspace_id = self.state.workspaces[ws_idx].id.clone();
                    let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
                        crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
                    });
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&workspace_id, &tab_id);
                    }
                }
                if focus {
                    self.state.switch_workspace_tab(ws_idx, tab_idx);
                    self.state.mode = Mode::Terminal;
                }
                self.schedule_session_save();
                self.emit_tab_created_events(ws_idx, tab_idx);
                encode_success(
                    id,
                    self.tab_created_result(ws_idx, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_else(|| {
            crate::workspace::public_tab_id_for_number(&workspace_id, tab_idx + 1)
        });
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.set_custom_name(params.label.clone());
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                workspace_id: self.public_workspace_id(ws_idx),
                label: params.label,
            },
        });
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        if params.insert_index > ws.tabs.len() {
            return encode_error(
                id,
                "tab_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let tab_id = self
            .public_tab_id(ws_idx, tab_idx)
            .unwrap_or_else(|| crate::workspace::public_tab_id_for_number(&ws.id, tab_idx + 1));
        let workspace_id = self.public_workspace_id(ws_idx);
        let insert_index = params.insert_index;
        let moved = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.move_tab(tab_idx, insert_index));
        let tabs = self.tab_list_info(ws_idx);
        if moved {
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::TabMoved,
                data: EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index,
                    tabs: tabs.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        let children = ws.tab_children(tab_idx).len();
        if children > 0 {
            // Child tabs usually run work (jobs); the caller confirms and closes
            // them first, so this never discards their output unasked.
            return encode_error(
                id,
                "tab_has_children",
                format!(
                    "tab {} has {children} child tab(s); close them first",
                    target.tab_id
                ),
            );
        }
        let closes_workspace = ws.tabs.len() <= 1;
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let pane_ids = ws
            .tabs
            .get(tab_idx)
            .map(|tab| tab.layout.pane_ids())
            .unwrap_or_default();

        if closes_workspace {
            if let Err(response) = self.require_restored_group_close_ready(
                &id,
                &self.state.workspace_close_indices(ws_idx),
            ) {
                return response;
            }
            if self.state.confirm_implicit_worktree_group_close(ws_idx) {
                return encode_error(
                    id,
                    "confirmation_required",
                    "closing this tab would close a worktree group",
                );
            }
            let workspace = self.workspace_info(ws_idx);
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.state.remove_plugin_pane_records(pane_ids);
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::TabClosed,
                data: EventData::TabClosed {
                    tab_id,
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id,
                    workspace: Some(workspace),
                },
            });
            return encode_success(id, ResponseResult::Ok {});
        }

        let record = self.closed_tab_record(ws_idx, tab_idx);
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        if !ws.close_tab(tab_idx) {
            return encode_error(
                id,
                "tab_close_failed",
                format!("tab {} could not be closed", target.tab_id),
            );
        }
        self.keep_closed_tab(record);
        self.state.remove_plugin_pane_records(pane_ids);
        self.state.remove_unattached_terminal_ids(terminal_ids);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id,
                workspace_id,
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_tab_set_parent(
        &mut self,
        id: String,
        params: TabSetParentParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let parent_idx = match params.parent_tab_id.as_deref() {
            Some(parent_tab_id) => match self.parse_tab_id(parent_tab_id) {
                Some((parent_ws_idx, parent_idx)) if parent_ws_idx == ws_idx => Some(parent_idx),
                Some(_) => {
                    return encode_error(
                        id,
                        "tab_set_parent_failed",
                        "the parent must be in the same workspace",
                    )
                }
                None => return tab_not_found(id, parent_tab_id),
            },
            None => None,
        };
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        let root_pane = ws.tabs[tab_idx].root_pane;
        if let Err(err) = ws.set_tab_parent(tab_idx, parent_idx) {
            return encode_error(id, "tab_set_parent_failed", err);
        }
        // Nesting reorders tabs; find this one again by its identity.
        let Some(tab_idx) = ws.tabs.iter().position(|tab| tab.root_pane == root_pane) else {
            return tab_not_found(id, &params.tab_id);
        };
        self.schedule_session_save();
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_set_status(
        &mut self,
        id: String,
        params: TabSetStatusParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.status = params.status;
        // Only a running job can be idle; any other status clears it.
        tab.activity = params
            .activity
            .filter(|_| params.status == Some(crate::api::schema::TabStatus::Running));
        self.schedule_session_save();
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_bookmark(
        &mut self,
        id: String,
        params: crate::api::schema::TabBookmarkParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        if tab.bookmarked != params.bookmarked {
            tab.bookmarked = params.bookmarked;
            self.schedule_session_save();
        }
        match self.tab_info(ws_idx, tab_idx) {
            Some(tab) => encode_success(id, ResponseResult::TabInfo { tab }),
            None => tab_not_found(id, &params.tab_id),
        }
    }

    pub(super) fn handle_tab_set_role(
        &mut self,
        id: String,
        params: crate::api::schema::TabSetRoleParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        if tab.role != params.role {
            tab.role = params.role;
            self.schedule_session_save();
        }
        match self.tab_info(ws_idx, tab_idx) {
            Some(tab) => encode_success(id, ResponseResult::TabInfo { tab }),
            None => tab_not_found(id, &params.tab_id),
        }
    }

    pub(super) fn handle_tab_set_job_metadata(
        &mut self,
        id: String,
        params: crate::api::schema::TabSetJobMetadataParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        if let Some(job) = &params.job {
            let fields = [
                Some(job.id.as_str()),
                Some(job.name.as_str()),
                job.why.as_deref(),
                Some(job.origin.as_str()),
                job.owner_pane.as_deref(),
            ];
            if job.id.trim().is_empty()
                || job.name.trim().is_empty()
                || fields
                    .into_iter()
                    .flatten()
                    .any(|text| text.len() > 4096 || text.chars().any(char::is_control))
            {
                return encode_error(
                    id,
                    "invalid_job_metadata",
                    "job fields must be nonempty identifiers and bounded single-line text",
                );
            }
        }
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.job = params.job;
        self.schedule_session_save();
        match self.tab_info(ws_idx, tab_idx) {
            Some(tab) => encode_success(id, ResponseResult::TabInfo { tab }),
            None => tab_not_found(id, &params.tab_id),
        }
    }

    fn tab_list_info(&self, ws_idx: usize) -> Vec<crate::api::schema::TabInfo> {
        self.state
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                (0..ws.tabs.len())
                    .filter_map(|idx| self.tab_info(ws_idx, idx))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::{
        api::schema::SuccessResponse,
        config::{Config, ShellModeConfig},
        workspace::Workspace,
    };

    #[test]
    fn api_tab_close_focuses_main_neighbor_instead_of_nested_job() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut ws = Workspace::test_new("tabs");
        let job1 = ws.test_add_tab(Some("job1"));
        let job2 = ws.test_add_tab(Some("job2"));
        let main = ws.test_add_tab(Some("main"));
        ws.set_tab_parent(job1, Some(0)).unwrap();
        ws.set_tab_parent(job2, Some(0)).unwrap();
        ws.switch_tab(main);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        let parent_id = app.public_tab_id(0, 0).unwrap();
        let job_ids = [
            app.public_tab_id(0, job1).unwrap(),
            app.public_tab_id(0, job2).unwrap(),
        ];
        let closed_id = app.public_tab_id(0, main).unwrap();

        let response = app.handle_tab_close("req".into(), TabTarget { tab_id: closed_id });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        let tabs = app.tab_list_info(0);
        assert_eq!(tabs.len(), 3);
        assert_eq!(
            tabs.iter().find(|tab| tab.focused).unwrap().tab_id,
            parent_id
        );
        for (tab, id) in tabs[1..].iter().zip(job_ids) {
            assert_eq!(tab.tab_id, id);
            assert_eq!(tab.parent_tab_id.as_deref(), Some(parent_id.as_str()));
        }
        app.state.workspaces[0].assert_invariants_for_test();
    }

    #[test]
    fn api_tab_close_last_tab_closes_workspace_and_emits_both_events() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![Workspace::test_new("tabs")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let workspace_id = app.public_workspace_id(0);

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: tab_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.active.is_none());
        let events = event_hub.events_after(0);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            [EventKind::TabClosed, EventKind::WorkspaceClosed]
        );
        assert!(matches!(
            &events[0].1.data,
            EventData::TabClosed {
                tab_id: closed_tab_id,
                workspace_id: closed_workspace_id,
            } if closed_tab_id == &tab_id && closed_workspace_id == &workspace_id
        ));
        assert!(matches!(
            &events[1].1.data,
            EventData::WorkspaceClosed {
                workspace_id: closed_workspace_id,
                workspace: Some(workspace),
            } if closed_workspace_id == &workspace_id
                && workspace.workspace_id == workspace_id
        ));
    }

    #[test]
    fn api_job_metadata_registers_rejects_controls_and_clears_without_identity_changes() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("jobs")];
        app.state.active = Some(0);
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let job = crate::api::schema::TabJobMetadata {
            id: "test-job".into(),
            name: "Build".into(),
            why: Some("verify".into()),
            origin: "test".into(),
            owner_pane: None,
        };
        let response = app.handle_tab_set_job_metadata(
            "req".into(),
            crate::api::schema::TabSetJobMetadataParams {
                tab_id: tab_id.clone(),
                job: Some(job.clone()),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("unexpected response");
        };
        assert_eq!(tab.job, Some(job.clone()));
        assert_eq!(tab.tab_id, tab_id);
        app.state.workspaces[0].assert_invariants_for_test();
        let mut invalid = job.clone();
        invalid.name = "bad\u{1b}[2J".into();
        let response = app.handle_tab_set_job_metadata(
            "req".into(),
            crate::api::schema::TabSetJobMetadataParams {
                tab_id: tab_id.clone(),
                job: Some(invalid),
            },
        );
        assert!(response.contains("invalid_job_metadata"));
        assert_eq!(app.state.workspaces[0].tabs[0].job, Some(job));
        app.handle_tab_set_job_metadata(
            "req".into(),
            crate::api::schema::TabSetJobMetadataParams {
                tab_id: tab_id.clone(),
                job: None,
            },
        );
        assert!(app.tab_info(0, 0).unwrap().job.is_none());
        assert_eq!(app.public_tab_id(0, 0).as_deref(), Some(tab_id.as_str()));
        app.state.workspaces[0].assert_invariants_for_test();
    }

    #[test]
    fn api_child_tabs_report_parent_and_status_and_block_closing_the_parent() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("b"));
        workspace.test_add_tab(Some("job"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let parent = app.public_tab_id(0, 0).unwrap();
        let job = app.public_tab_id(0, 2).unwrap();

        let response = app.handle_tab_set_parent(
            "req".into(),
            TabSetParentParams {
                tab_id: job.clone(),
                parent_tab_id: Some(parent.clone()),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("unexpected response: {response}");
        };
        assert_eq!(tab.parent_tab_id.as_deref(), Some(parent.as_str()));
        assert_eq!(tab.tab_id, job, "public tab ids survive the reorder");

        app.handle_tab_set_status(
            "req".into(),
            TabSetStatusParams {
                activity: None,
                tab_id: job.clone(),
                status: Some(crate::api::schema::TabStatus::Failed),
            },
        );
        let labels = app
            .tab_list_info(0)
            .into_iter()
            .map(|tab| (tab.label, tab.status))
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                ("1".to_string(), None),
                ("job".into(), Some(crate::api::schema::TabStatus::Failed)),
                ("b".into(), None),
            ]
        );

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: parent.clone(),
            },
        );
        assert!(response.contains("tab_has_children"), "{response}");
        assert_eq!(app.state.workspaces[0].tabs.len(), 3);

        app.handle_tab_close("req".into(), TabTarget { tab_id: job });
        let response = app.handle_tab_close("req".into(), TabTarget { tab_id: parent });
        assert!(!response.contains("error"), "{response}");
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn api_tab_activity_marks_only_a_running_job_and_clears_with_any_status_change() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("job"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let tab_id = app.public_tab_id(0, 1).unwrap();
        let set = |app: &mut App, status, activity| {
            app.handle_tab_set_status(
                "req".into(),
                TabSetStatusParams {
                    tab_id: tab_id.clone(),
                    status,
                    activity,
                },
            );
            app.tab_info(0, 1).unwrap().activity
        };
        use crate::api::schema::{
            TabActivity::Idle,
            TabStatus::{Running, Succeeded},
        };
        assert_eq!(set(&mut app, Some(Running), Some(Idle)), Some(Idle));
        // The runner says nothing new: a status report without an activity clears it.
        assert_eq!(set(&mut app, Some(Running), None), None);
        assert_eq!(set(&mut app, Some(Running), Some(Idle)), Some(Idle));
        // A finished job is not idle, whatever is sent.
        assert_eq!(set(&mut app, Some(Succeeded), Some(Idle)), None);
    }

    #[test]
    fn api_tab_bookmark_sets_and_clears_the_flag_and_survives_a_snapshot() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("b"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let tab_id = app.public_tab_id(0, 1).unwrap();

        let bookmark = |app: &mut App, bookmarked| {
            let response = app.handle_tab_bookmark(
                "req".into(),
                crate::api::schema::TabBookmarkParams {
                    tab_id: tab_id.clone(),
                    bookmarked,
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::TabInfo { tab } = success.result else {
                panic!("unexpected response: {response}");
            };
            tab.bookmarked
        };
        assert!(bookmark(&mut app, true));
        // Repeating it changes nothing.
        assert!(bookmark(&mut app, true));
        assert_eq!(
            app.tab_list_info(0)
                .into_iter()
                .map(|tab| tab.bookmarked)
                .collect::<Vec<_>>(),
            [false, true]
        );
        // It is persisted with the session.
        let captured = crate::persist::capture(
            &app.state.workspaces,
            &app.state.terminals,
            &app.terminal_runtimes,
            app.state.active,
            app.state.selected,
        );
        assert!(captured.workspaces[0].tabs[1].bookmarked);
        assert!(!captured.workspaces[0].tabs[0].bookmarked);
        assert!(!bookmark(&mut app, false));
        let response = app.handle_tab_bookmark(
            "req".into(),
            crate::api::schema::TabBookmarkParams {
                tab_id: "no_such_tab".into(),
                bookmarked: true,
            },
        );
        assert!(response.contains("tab_not_found"), "{response}");
    }

    #[test]
    fn api_tab_set_role_marks_the_tab_and_survives_a_snapshot() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("todo"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let tab_id = app.public_tab_id(0, 1).unwrap();

        let set = |app: &mut App, role| {
            let response = app.handle_tab_set_role(
                "req".into(),
                crate::api::schema::TabSetRoleParams {
                    tab_id: tab_id.clone(),
                    role,
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::TabInfo { tab } = success.result else {
                panic!("unexpected response: {response}");
            };
            tab.role
        };
        use crate::api::schema::TabRole;
        assert_eq!(set(&mut app, Some(TabRole::Worker)), Some(TabRole::Worker));
        assert_eq!(
            set(&mut app, Some(TabRole::Coordinator)),
            Some(TabRole::Coordinator)
        );
        assert_eq!(
            app.tab_list_info(0)
                .into_iter()
                .map(|tab| tab.role)
                .collect::<Vec<_>>(),
            [None, Some(TabRole::Coordinator)]
        );
        let captured = crate::persist::capture(
            &app.state.workspaces,
            &app.state.terminals,
            &app.terminal_runtimes,
            app.state.active,
            app.state.selected,
        );
        assert_eq!(
            captured.workspaces[0].tabs[1].role,
            Some(TabRole::Coordinator)
        );
        assert_eq!(captured.workspaces[0].tabs[0].role, None);
        assert_eq!(set(&mut app, None), None);
        let response = app.handle_tab_set_role(
            "req".into(),
            crate::api::schema::TabSetRoleParams {
                tab_id: "no_such_tab".into(),
                role: Some(TabRole::Worker),
            },
        );
        assert!(response.contains("tab_not_found"), "{response}");
    }

    #[test]
    fn api_tab_move_reorders_tabs_in_target_workspace() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub.clone(),
        );
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("two"));
        workspace.test_add_tab(Some("three"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_root = app.state.workspaces[0].tabs[0].root_pane;
        let moved_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_tab_move(
            "req".into(),
            TabMoveParams {
                tab_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabList { tabs } = success.result else {
            panic!("expected tab list");
        };
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(tabs[2].tab_id, app.public_tab_id(0, 2).unwrap());
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::TabMoved {
                    tab_id,
                    workspace_id,
                    insert_index: 3,
                    tabs,
                } if tab_id == &moved_id
                    && workspace_id == &app.public_workspace_id(0)
                    && tabs[2].tab_id == moved_id
            )
        }));
    }

    #[tokio::test]
    async fn tab_create_follows_cached_focused_pane_cwd_without_runtime() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            event_hub,
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let workspace = Workspace::test_new("tabs");
        let focused_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let cached_cwd = std::env::temp_dir();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .cloned()
            .unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cached_cwd.clone();

        let response = app.handle_tab_create(
            "req".into(),
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::TabCreated { .. }));
        let created = &app.state.workspaces[0].tabs[1];
        let created_terminal_id = created.terminal_id(created.root_pane).unwrap();
        let created_cwd = &app.state.terminals.get(created_terminal_id).unwrap().cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&cached_cwd)
        );
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn tab_create_child_creates_the_tab_already_nested() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("b"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let parent = app.public_tab_id(0, 0).unwrap();
        let child_params = |parent_tab_id: &str| TabCreateChildParams {
            parent_tab_id: parent_tab_id.into(),
            cwd: Some(std::env::temp_dir().display().to_string()),
            focus: false,
            label: Some("job".into()),
            env: Default::default(),
        };

        let response = app.handle_tab_create_child("req".into(), child_params(&parent));

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabCreated { tab, .. } = success.result else {
            panic!("unexpected response: {response}");
        };
        assert_eq!(tab.parent_tab_id.as_deref(), Some(parent.as_str()));
        assert_eq!(tab.label, "job");
        let ws = &app.state.workspaces[0];
        assert_eq!(ws.tabs.len(), 3);
        assert_eq!(
            ws.tab_parent_index(1),
            Some(0),
            "the child follows its parent"
        );
        assert_eq!(
            app.public_tab_id(0, 1).as_deref(),
            Some(tab.tab_id.as_str())
        );

        // A child cannot be a parent; the request fails without creating a tab.
        let response = app.handle_tab_create_child("req".into(), child_params(&tab.tab_id));
        assert!(response.contains("tab_create_failed"), "{response}");
        assert_eq!(app.state.workspaces[0].tabs.len(), 3);
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn a_closed_tab_reopens_with_its_layout_name_and_place_once() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        // a, b (split in two panes, named "agent"), c.
        let mut workspace = Workspace::test_new("tabs");
        let b = workspace.test_add_tab(Some("agent"));
        workspace.active_tab = b;
        workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.test_add_tab(Some("c"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let first = app.public_tab_id(0, 0).unwrap();
        let closed = app.public_tab_id(0, b).unwrap();
        let panes_before = app.state.workspaces[0].tabs[b].layout.pane_ids().len();
        assert_eq!(panes_before, 2);

        let response = app.handle_tab_close(
            "req".into(),
            TabTarget {
                tab_id: closed.clone(),
            },
        );
        assert!(response.contains("\"ok\""), "{response}");
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);

        let reopen = |app: &mut App| {
            app.handle_tab_reopen_closed(
                "req".into(),
                crate::api::schema::TabReopenClosedParams {
                    tab_id: closed.clone(),
                    after_tab_id: Some(first.clone()),
                    focus: true,
                },
            )
        };
        let response = reopen(&mut app);
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabCreated { tab, .. } = success.result else {
            panic!("unexpected response: {response}");
        };
        assert_eq!(tab.label, "agent");
        assert_ne!(tab.tab_id, closed, "a new id: public ids are never reused");
        let ws = &app.state.workspaces[0];
        assert_eq!(ws.tabs.len(), 3);
        assert_eq!(ws.tabs[1].layout.pane_ids().len(), 2, "the split is back");
        assert_eq!(ws.tabs[1].custom_name.as_deref(), Some("agent"));
        assert_eq!(ws.active_tab, 1);
        for pane_id in ws.tabs[1].layout.pane_ids() {
            assert!(ws.public_pane_numbers.contains_key(&pane_id));
        }
        // Used up: a second reopen cannot bring it twice.
        assert!(reopen(&mut app).contains("closed_tab_not_found"));

        // Closing a tab's last pane keeps the tab too.
        let c = app.state.workspaces[0].tabs.len() - 1;
        let c_id = app.public_tab_id(0, c).unwrap();
        let c_pane = app.state.workspaces[0].tabs[c].root_pane;
        let c_pane_id = app.public_pane_id(0, c_pane).unwrap();
        let response = app.handle_pane_close(
            "req".into(),
            crate::api::schema::PaneTarget { pane_id: c_pane_id },
        );
        assert!(response.contains("\"ok\""), "{response}");
        let response = app.handle_tab_reopen_closed(
            "req".into(),
            crate::api::schema::TabReopenClosedParams {
                tab_id: c_id,
                after_tab_id: None,
                focus: false,
            },
        );
        assert!(response.contains("tab_created"), "{response}");
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("c")
        );
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn a_reopened_tab_resumes_its_agent_unless_that_session_runs_elsewhere() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("agent"));
        workspace.test_add_tab(Some("agent again"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let session = || crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        };
        let set_session = |app: &mut App, tab_idx: usize| {
            let pane = app.state.workspaces[0].tabs[tab_idx].root_pane;
            let terminal_id = app.state.terminal_id_for_pane(0, pane).unwrap();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .persisted_agent_session = Some(session());
        };
        let close_and_reopen = |app: &mut App, tab_idx: usize| {
            let tab_id = app.public_tab_id(0, tab_idx).unwrap();
            let response = app.handle_tab_close(
                "req".into(),
                TabTarget {
                    tab_id: tab_id.clone(),
                },
            );
            assert!(response.contains("\"ok\""), "{response}");
            let response = app.handle_tab_reopen_closed(
                "req".into(),
                crate::api::schema::TabReopenClosedParams {
                    tab_id,
                    after_tab_id: None,
                    focus: false,
                },
            );
            assert!(response.contains("tab_created"), "{response}");
            // Reopened first.
            let pane = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.terminal_id_for_pane(0, pane).unwrap();
            app.state.terminals[&terminal_id]
                .pending_agent_resume_plan
                .is_some()
        };

        set_session(&mut app, 1);
        assert!(close_and_reopen(&mut app, 1), "its agent resumes");

        // The same session is live in another tab (the reopened one is
        // pending already): a second copy must not resume it.
        set_session(&mut app, 2);
        assert!(
            !close_and_reopen(&mut app, 2),
            "never two agents on one session"
        );
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn tab_create_after_puts_the_tab_after_the_whole_group() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        // a, b, c; then a job nested under a.
        let mut workspace = Workspace::test_new("tabs");
        workspace.test_add_tab(Some("b"));
        workspace.test_add_tab(Some("c"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let first = app.public_tab_id(0, 0).unwrap();
        let response = app.handle_tab_create_child(
            "req".into(),
            TabCreateChildParams {
                parent_tab_id: first.clone(),
                cwd: Some(std::env::temp_dir().display().to_string()),
                focus: false,
                label: Some("job".into()),
                env: Default::default(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabCreated { tab: job, .. } = success.result else {
            panic!("unexpected response: {response}");
        };
        let after = |app: &mut App, after_tab_id: &str| {
            let response = app.handle_tab_create_after(
                "req".into(),
                crate::api::schema::TabCreateAfterParams {
                    after_tab_id: after_tab_id.into(),
                    cwd: Some(std::env::temp_dir().display().to_string()),
                    focus: true,
                    label: Some("new".into()),
                    env: Default::default(),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::TabCreated { tab, .. } = success.result else {
                panic!("unexpected response: {response}");
            };
            tab
        };
        let labels = |app: &App| {
            app.state.workspaces[0]
                .tabs
                .iter()
                .map(|tab| tab.custom_name.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        };

        // After `a`'s job, not between `a` and its job; focused, top-level.
        let created = after(&mut app, &first);
        assert_eq!(labels(&app)[..4], ["", "job", "new", "b"]);
        assert_eq!(created.parent_tab_id, None);
        assert_eq!(
            app.state.workspaces[0].active_tab, 2,
            "the new tab has the focus"
        );
        // From the job tab, after its parent's group as well.
        after(&mut app, &job.tab_id);
        assert_eq!(labels(&app)[..4], ["", "job", "new", "new"]);
        assert_eq!(labels(&app).len(), 6);
        assert!(app
            .handle_tab_create_after(
                "req".into(),
                crate::api::schema::TabCreateAfterParams {
                    after_tab_id: "nope".into(),
                    cwd: None,
                    focus: false,
                    label: None,
                    env: Default::default(),
                },
            )
            .contains("tab_not_found"));
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn tab_create_agent_opens_a_tab_and_rejects_unknown_kinds() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        app.state.workspaces = vec![Workspace::test_new("agents")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let workspace_id = app.state.workspaces[0].id.clone();
        let params = |kind: &str| crate::api::schema::TabCreateAgentParams {
            workspace_id: workspace_id.clone(),
            kind: kind.into(),
            focus: true,
        };

        // An alias or unknown name is refused before any tab exists.
        for kind in ["claude-code", "vim"] {
            let response = app.handle_tab_create_agent("req".into(), params(kind));
            assert!(response.contains("unsupported_agent_kind"), "{response}");
        }
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);

        let response = app.handle_tab_create_agent("req".into(), params("claude"));
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(
            matches!(success.result, ResponseResult::TabCreated { .. }),
            "{response}"
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn agent_kind_list_names_only_canonical_agents() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let response = app.handle_agent_kind_list("req".into());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentKindList { kinds } = success.result else {
            panic!("unexpected response: {response}");
        };
        for kind in kinds {
            assert!(
                crate::detect::parse_canonical_agent_label(&kind).is_some(),
                "{kind}"
            );
        }
    }

    #[tokio::test]
    async fn agent_handoff_opens_a_tab_after_the_source_with_a_pointer_prompt() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        let mut workspace = Workspace::test_new("handoff");
        workspace.test_add_tab(Some("other"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let root = crate::agent_handoff::tests::temp_dir("app");
        let claude_transcript = root.join("claude/projects/-elsewhere/abc-123.jsonl");
        let pi_transcript = root.join("pi/sessions/--w--/2026-10-06T00-00-00-000Z_01pi.jsonl");
        for path in [&claude_transcript, &pi_transcript] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "{}\n").unwrap();
        }
        let dirs = crate::agent_handoff::TranscriptDirs {
            claude: Some(root.join("claude")),
            pi: Some(root.join("pi")),
            ..Default::default()
        };
        let source_pane = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.terminal_id_for_pane(0, source_pane).unwrap();
        let pane_id = app.public_pane_id(0, source_pane).unwrap();
        let set_session = |app: &mut App, session| {
            app.state
                .terminals
                .get_mut(&source_terminal)
                .unwrap()
                .persisted_agent_session = session;
        };
        let params = |to: &str| crate::api::schema::AgentHandoffParams {
            pane_id: pane_id.clone(),
            to: to.into(),
            focus: false,
        };

        // No session known: an error, and no tab.
        let err = app
            .hand_off_agent("req".into(), params("pi"), &dirs)
            .unwrap_err();
        assert!(err.contains("agent_session_unknown"), "{err}");
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);

        // A session whose transcript is not on this machine is not guessed.
        set_session(
            &mut app,
            Some(crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("gone-1").unwrap(),
            }),
        );
        let err = app
            .hand_off_agent("req".into(), params("pi"), &dirs)
            .unwrap_err();
        assert!(err.contains("agent_transcript_not_found"), "{err}");
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);

        // Only the agents that take a first prompt.
        let err = app
            .hand_off_agent("req".into(), params("vim"), &dirs)
            .unwrap_err();
        assert!(err.contains("unsupported_agent_kind"), "{err}");

        // A Claude session goes to Pi in a new tab right after the source.
        set_session(
            &mut app,
            Some(crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("abc-123").unwrap(),
            }),
        );
        let (response, prompt) = app
            .hand_off_agent("req".into(), params("pi"), &dirs)
            .unwrap();
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(
            matches!(success.result, ResponseResult::TabCreated { .. }),
            "{response}"
        );
        assert!(
            prompt.starts_with(&format!(
                "Continue the work from claude session abc-123, transcript {}",
                claude_transcript.display()
            )),
            "{prompt}"
        );
        let tabs = &app.state.workspaces[0].tabs;
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0].root_pane, source_pane);
        assert_eq!(tabs[2].custom_name.as_deref(), Some("other"));

        // The source tab is untouched: same pane, same session.
        assert!(app.state.terminals[&source_terminal]
            .persisted_agent_session
            .is_some());

        // A Pi session, known by its session file, goes to Claude.
        set_session(
            &mut app,
            Some(crate::agent_resume::PersistedAgentSession {
                source: "herdr:pi".into(),
                agent: "pi".into(),
                session_ref: crate::agent_resume::AgentSessionRef::path(
                    pi_transcript.to_string_lossy(),
                )
                .unwrap(),
            }),
        );
        let (_, prompt) = app
            .hand_off_agent("req".into(), params("claude"), &dirs)
            .unwrap();
        assert!(
            prompt.starts_with(&format!(
                "Continue the work from pi session 01pi, transcript {}",
                pi_transcript.display()
            )),
            "{prompt}"
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 4);
        shutdown_test_runtimes(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
