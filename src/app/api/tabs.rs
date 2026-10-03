use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateChildParams, TabCreateParams,
    TabListParams, TabMoveParams, TabRenameParams, TabSetParentParams, TabSetStatusParams,
    TabTarget,
};
use crate::app::{App, Mode};

use super::responses::{encode_error, encode_success};

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
        self.create_tab_in_workspace(id, ws_idx, None, cwd, focus, label, env)
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
        self.create_tab_in_workspace(id, ws_idx, Some(parent_idx), cwd, focus, label, env)
    }

    /// Creates a tab in a workspace and, with a parent, nests it before any
    /// client sees it, so it never shows as a top-level tab first.
    #[allow(clippy::too_many_arguments)] // the fields of the two create requests
    pub(super) fn create_tab_in_workspace(
        &mut self,
        id: String,
        ws_idx: usize,
        parent_idx: Option<usize>,
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
                if let Some(parent_idx) = parent_idx {
                    let ws = &mut self.state.workspaces[ws_idx];
                    let root_pane = ws.tabs[tab_idx].root_pane;
                    if let Err(err) = ws.set_tab_parent(tab_idx, Some(parent_idx)) {
                        tracing::warn!(err, "could not nest a new tab under its parent");
                    }
                    // Nesting reorders tabs; find this one again by its identity.
                    if let Some(idx) = ws.tabs.iter().position(|tab| tab.root_pane == root_pane) {
                        tab_idx = idx;
                    }
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
}
