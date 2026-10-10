//! `coordinator.*` requests and the coordinator tab role. Coordination
//! tenures live in the worker store (`crate::workers::coordinators`); these
//! run on the app because a tenure is bound to a pane, whose directory
//! names its repository, and because a tab's coordinator role follows the
//! tenures: a tab is marked coordinator exactly while one of its panes is
//! bound to an active tenure ([`App::sync_coordinator_roles`]).

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    CoordinatorAllowlistRefusalParams, CoordinatorEndParams, CoordinatorHandoffParams,
    CoordinatorInfo, CoordinatorRecordOverrideParams, CoordinatorStartParams,
    CoordinatorStatusParams, NotificationShowParams, NotificationShowSound, ResponseResult,
    TabRole,
};
use crate::app::App;
use crate::workers::{WorkerError, WorkerSupervisor};

fn unavailable(id: String) -> String {
    encode_error(
        id,
        "worker_io_error",
        "coordination tenures are unavailable on this server",
    )
}

/// The repository of directory `dir`, as tenures name it.
fn repository(dir: &str) -> Result<String, WorkerError> {
    crate::workers::repository_of_dir(dir)
        .ok_or_else(|| WorkerError::Invalid(format!("{dir} is not in a git repository")))
}

impl App {
    pub(super) fn handle_coordinator_start(
        &mut self,
        id: String,
        params: CoordinatorStartParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let Some(pane) = params.pane_id.as_deref() else {
            return encode_error(
                id,
                "invalid_request",
                "coordinator.start needs pane_id, the coordinator's pane: run it in a herdr pane",
            );
        };
        let Some((ws_idx, pane_id)) = self.parse_pane_id(pane) else {
            return encode_error(id, "pane_not_found", format!("pane not found: {pane}"));
        };
        let started = self.start_coordinator(coordinators, ws_idx, pane_id, params.repo.as_deref());
        self.sync_coordinator_roles();
        match started {
            Ok(coordinator) => encode_success(id, ResponseResult::Coordinator { coordinator }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    pub(super) fn handle_coordinator_end(
        &mut self,
        id: String,
        params: CoordinatorEndParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let tenure = match (&params.coordinator_id, &params.pane_id) {
            (Some(tenure), _) => tenure.clone(),
            (None, Some(pane)) => {
                let pane = self.canonical_pane_id(pane);
                match coordinators.coordinator_of_pane(&pane) {
                    Some(tenure) => tenure.coordinator_id,
                    None => {
                        return encode_error(
                            id,
                            "coordinator_not_found",
                            format!("pane {pane} is bound to no active coordinator"),
                        )
                    }
                }
            }
            (None, None) => {
                return encode_error(
                    id,
                    "invalid_request",
                    "coordinator.end needs coordinator_id or pane_id",
                )
            }
        };
        let reason = params
            .reason
            .as_deref()
            .unwrap_or(crate::workers::coordinators::ENDED);
        let ended = coordinators.coordinator_end(&tenure, reason, None);
        self.sync_coordinator_roles();
        match ended {
            Ok(coordinator) => encode_success(id, ResponseResult::Coordinator { coordinator }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// `coordinator.handoff`: the tenure named (else the one bound to
    /// `pane_id`) hands over to `to_pane_id` and its agent session.
    pub(super) fn handle_coordinator_handoff(
        &mut self,
        id: String,
        params: CoordinatorHandoffParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        if params.coordinator_id.is_none() && params.pane_id.is_none() {
            return encode_error(
                id,
                "invalid_request",
                "coordinator.handoff needs coordinator_id or pane_id",
            );
        }
        let Some(target) = self
            .parse_pane_id(&params.to_pane_id)
            .and_then(|(ws_idx, pane_id)| self.pane_metadata(ws_idx, pane_id))
        else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane not found: {}", params.to_pane_id),
            );
        };
        let from_pane = params
            .pane_id
            .as_deref()
            .map(|pane| self.canonical_pane_id(pane));
        let session = target.agent_session.map(|session| session.value);
        let handed = coordinators.coordinator_handoff(
            params.coordinator_id.as_deref(),
            from_pane.as_deref(),
            &target.pane_id,
            session.as_deref(),
            Some(&target.workspace_id),
        );
        self.sync_coordinator_roles();
        match handed {
            Ok(coordinator) => encode_success(id, ResponseResult::Coordinator { coordinator }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// The pane's agent session came back or changed: when that session
    /// coordinated a tenure it still can, the tenure is bound to this pane
    /// now ([`WorkerSupervisor::coordinator_resume`]), and the tabs' roles
    /// follow.
    pub(super) fn resume_coordinator_in(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(coordinators) = crate::workers::coordinators() else {
            return;
        };
        let Some(pane) = self.pane_metadata(ws_idx, pane_id) else {
            return;
        };
        let Some(session) = pane.agent_session.map(|session| session.value) else {
            return;
        };
        match coordinators.coordinator_resume(&pane.pane_id, &session, Some(&pane.workspace_id)) {
            Ok(Some(_)) => self.sync_coordinator_roles(),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                pane_id = pane.pane_id,
                "cannot resume the agent session's coordination tenure"
            ),
        }
    }

    pub(super) fn handle_coordinator_status(
        &mut self,
        id: String,
        params: CoordinatorStatusParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let listed = match params.repo.as_deref().map(repository).transpose() {
            Ok(repo) => coordinators.coordinator_status(repo.as_deref()),
            Err(error) => Err(error),
        };
        match listed {
            Ok(coordinators) => encode_success(id, ResponseResult::Coordinators { coordinators }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// Records an allowlist exception and shows it as an in-app toast; the
    /// headless server answers this method itself, with a notification for
    /// its client shells.
    pub(super) fn handle_coordinator_record_override(
        &mut self,
        id: String,
        params: CoordinatorRecordOverrideParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        if self.parse_pane_id(&params.pane_id).is_none() {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        }
        let pane = self.canonical_pane_id(&params.pane_id);
        match coordinators.record_override(&pane, &params) {
            Ok(record) => {
                let (title, body) = crate::workers::coordinators::override_notice(&record);
                let _ = self.handle_notification_show(
                    id.clone(),
                    NotificationShowParams {
                        title,
                        body: Some(body),
                        position: None,
                        sound: NotificationShowSound::Request,
                    },
                );
                encode_success(id, ResponseResult::CoordinatorOverride { record })
            }
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// Tells the allowlist hook what to do with a call it refused: the
    /// configured `[coordinator] allowlist` mode, and in shadow mode the
    /// would-deny it stored. Shadow mode is silent: no notification.
    pub(super) fn handle_coordinator_allowlist_refusal(
        &mut self,
        id: String,
        params: CoordinatorAllowlistRefusalParams,
    ) -> String {
        if self.parse_pane_id(&params.pane_id).is_none() {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        }
        let pane = self.canonical_pane_id(&params.pane_id);
        let mode = crate::workers::coordinators::configured_allowlist_mode();
        let record = crate::workers::coordinators()
            .and_then(|coordinators| coordinators.allowlist_refusal(&pane, &params, mode));
        encode_success(
            id,
            ResponseResult::CoordinatorAllowlistRefusal { mode, record },
        )
    }

    /// Starts a tenure bound to the pane: of the repository of `repo` (a
    /// directory in it), else of the pane's directory.
    pub(super) fn start_coordinator(
        &self,
        coordinators: &WorkerSupervisor,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        repo: Option<&str>,
    ) -> Result<CoordinatorInfo, WorkerError> {
        let pane = self
            .pane_metadata(ws_idx, pane_id)
            .ok_or_else(|| WorkerError::Invalid("the pane has no terminal".into()))?;
        let dir = match repo {
            Some(dir) => dir.to_owned(),
            None => pane.cwd.clone().ok_or_else(|| {
                WorkerError::Invalid(format!(
                    "the directory of pane {} is unknown; pass repo",
                    pane.pane_id
                ))
            })?,
        };
        let repo = repository(&dir)?;
        let session = pane.agent_session.map(|session| session.value);
        coordinators.coordinator_start(&repo, &pane.pane_id, session.as_deref())
    }

    /// The public id a tenure is bound with: the canonical form of an alias.
    pub(super) fn canonical_pane_id(&self, pane: &str) -> String {
        self.parse_pane_id(pane)
            .and_then(|(ws_idx, pane_id)| self.public_pane_id(ws_idx, pane_id))
            .unwrap_or_else(|| pane.to_owned())
    }

    /// Tab `tab_idx`'s panes' public ids.
    pub(super) fn tab_public_pane_ids(&self, ws_idx: usize, tab_idx: usize) -> Vec<String> {
        let Some(tab) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        else {
            return Vec::new();
        };
        tab.panes
            .keys()
            .filter_map(|pane_id| self.public_pane_id(ws_idx, *pane_id))
            .collect()
    }

    /// Marks as coordinator exactly the tabs with a pane bound to an active
    /// tenure, and unmarks the others; a worker role stays. The store is the
    /// source of truth: when it cannot be read, nothing changes.
    pub(crate) fn sync_coordinator_roles(&mut self) {
        let Some(coordinators) = crate::workers::coordinators() else {
            return;
        };
        let panes = match coordinators.coordinator_panes() {
            Ok(panes) => panes,
            Err(error) => {
                tracing::warn!(%error, "cannot read the coordination tenures");
                return;
            }
        };
        let mut changed = false;
        for ws_idx in 0..self.state.workspaces.len() {
            for tab_idx in 0..self.state.workspaces[ws_idx].tabs.len() {
                let crowned = self
                    .tab_public_pane_ids(ws_idx, tab_idx)
                    .iter()
                    .any(|pane| panes.contains(pane));
                let tab = &mut self.state.workspaces[ws_idx].tabs[tab_idx];
                let role = match (crowned, tab.role) {
                    (true, _) => Some(TabRole::Coordinator),
                    (false, Some(TabRole::Coordinator)) => None,
                    (false, role) => role,
                };
                if role != tab.role {
                    tab.role = role;
                    changed = true;
                }
            }
        }
        if changed {
            self.schedule_session_save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{SuccessResponse, TabSetRoleParams};
    use crate::config::Config;
    use crate::workspace::Workspace;

    /// An app with two tabs whose panes run in this checkout, and a worker
    /// store of its own as the tenures' store.
    fn app(name: &str) -> (App, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "herdr-app-coordinators-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        crate::workers::set_test_coordinators(WorkerSupervisor::open(
            root.join("workers"),
            "claude".into(),
        ));
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
        let checkout = std::env::current_dir().unwrap();
        for tab in &workspace.tabs {
            for pane in tab.panes.values() {
                let terminal_id = pane.attached_terminal_id.clone();
                app.state.terminals.insert(
                    terminal_id.clone(),
                    crate::terminal::TerminalState::new(terminal_id, checkout.clone()),
                );
            }
        }
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        (app, root)
    }

    fn pane_of(app: &App, tab_idx: usize) -> String {
        app.tab_public_pane_ids(0, tab_idx).remove(0)
    }

    fn set_role(app: &mut App, tab_idx: usize, role: Option<TabRole>) -> Result<(), String> {
        let response = app.handle_tab_set_role(
            "req".into(),
            TabSetRoleParams {
                tab_id: app.public_tab_id(0, tab_idx).unwrap(),
                role,
            },
        );
        serde_json::from_str::<SuccessResponse>(&response)
            .map(|_| ())
            .map_err(|_| response)
    }

    fn roles(app: &App) -> Vec<Option<TabRole>> {
        (0..app.state.workspaces[0].tabs.len())
            .map(|tab_idx| app.tab_info(0, tab_idx).unwrap().role)
            .collect()
    }

    fn active(_app: &App) -> Vec<CoordinatorInfo> {
        crate::workers::coordinators()
            .unwrap()
            .coordinator_status(None)
            .unwrap()
    }

    #[test]
    fn an_allowlist_refusal_answers_the_mode_and_records_only_in_shadow() {
        use crate::api::schema::{CoordinatorAllowlistMode, ErrorResponse, Method, Request};
        let (mut app, root) = app("would-deny");
        let pane_id = pane_of(&app, 1);
        let params = CoordinatorAllowlistRefusalParams {
            pane_id: pane_id.clone(),
            tool: "Bash".into(),
            command: "cargo build".into(),
            reason: "`cargo` is not allowed".into(),
            cwd: None,
            session_id: None,
        };
        let mut ask = |params: &CoordinatorAllowlistRefusalParams| {
            app.handle_api_request(Request {
                id: "r".into(),
                method: Method::CoordinatorAllowlistRefusal(params.clone()),
            })
        };
        for mode in [
            CoordinatorAllowlistMode::Enforce,
            CoordinatorAllowlistMode::Off,
            CoordinatorAllowlistMode::Shadow,
        ] {
            crate::workers::coordinators::set_test_allowlist_mode(mode);
            let response: SuccessResponse = serde_json::from_str(&ask(&params)).unwrap();
            let ResponseResult::CoordinatorAllowlistRefusal {
                mode: answered,
                record,
            } = response.result
            else {
                panic!("expected the allowlist mode");
            };
            assert_eq!(answered, mode);
            assert_eq!(
                record.is_some(),
                mode == CoordinatorAllowlistMode::Shadow,
                "{mode:?}"
            );
        }
        let (records, shapes) = crate::workers::coordinators()
            .unwrap()
            .would_deny(None)
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pane_id, pane_id);
        assert_eq!(shapes[0].shape, "Bash: cargo build");

        let unknown = CoordinatorAllowlistRefusalParams {
            pane_id: "w9:p9".into(),
            ..params
        };
        let response: ErrorResponse = serde_json::from_str(&ask(&unknown)).unwrap();
        assert_eq!(response.error.code, "pane_not_found");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_coordinator_role_claims_the_repository_and_follows_the_tenure() {
        let (mut app, root) = app("role");
        set_role(&mut app, 1, Some(TabRole::Coordinator)).unwrap();
        assert_eq!(roles(&app), [None, Some(TabRole::Coordinator)]);
        let tenures = active(&app);
        assert_eq!(tenures.len(), 1);
        assert_eq!(tenures[0].pane_id, Some(pane_of(&app, 1)));
        // Setting it again keeps the tenure.
        set_role(&mut app, 1, Some(TabRole::Coordinator)).unwrap();
        assert_eq!(active(&app), tenures);

        // A second coordinator of the repository is refused, by role or by
        // `coordinator.start`, naming the active tenure.
        let refused = set_role(&mut app, 0, Some(TabRole::Coordinator)).unwrap_err();
        assert!(refused.contains("coordinator_active"), "{refused}");
        assert!(refused.contains(&tenures[0].coordinator_id), "{refused}");
        let refused = app.handle_coordinator_start(
            "req".into(),
            CoordinatorStartParams {
                repo: None,
                pane_id: Some(pane_of(&app, 0)),
            },
        );
        assert!(refused.contains("coordinator_active"), "{refused}");
        assert_eq!(roles(&app), [None, Some(TabRole::Coordinator)]);

        // Clearing the role ends the tenure.
        set_role(&mut app, 1, None).unwrap();
        assert!(active(&app).is_empty());
        assert_eq!(roles(&app), [None, None]);

        // `coordinator.start` crowns the pane's tab; `coordinator.end` ends
        // it and the crown goes.
        let started = app.handle_coordinator_start(
            "req".into(),
            CoordinatorStartParams {
                repo: None,
                pane_id: Some(pane_of(&app, 0)),
            },
        );
        assert!(started.contains("\"epoch\":2"), "{started}");
        assert_eq!(roles(&app), [Some(TabRole::Coordinator), None]);
        let ended = app.handle_coordinator_end(
            "req".into(),
            CoordinatorEndParams {
                reason: Some("item done".into()),
                coordinator_id: None,
                pane_id: Some(pane_of(&app, 0)),
            },
        );
        assert!(ended.contains("\"end_reason\":\"item done\""), "{ended}");
        assert_eq!(roles(&app), [None, None]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_closed_coordinator_pane_orphans_its_tenure() {
        let (mut app, root) = app("closed");
        set_role(&mut app, 1, Some(TabRole::Coordinator)).unwrap();
        let tenure = active(&app).remove(0);
        // Its tab closes: the pane is gone.
        assert!(app.state.workspaces[0].close_tab(1));
        app.sync_worker_owner_panes();
        app.sync_coordinator_roles();
        assert!(active(&app).is_empty());
        let status = app.handle_coordinator_end(
            "req".into(),
            CoordinatorEndParams {
                reason: None,
                coordinator_id: Some(tenure.coordinator_id.clone()),
                pane_id: None,
            },
        );
        assert!(status.contains("\"end_reason\":\"orphaned\""), "{status}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_tab_marked_coordinator_without_a_tenure_loses_the_mark() {
        let (mut app, root) = app("legacy");
        // A role restored from a session saved before tenures existed.
        app.state.workspaces[0].tabs[1].role = Some(TabRole::Coordinator);
        app.state.workspaces[0].tabs[0].role = Some(TabRole::Worker);
        app.sync_coordinator_roles();
        assert_eq!(roles(&app), [Some(TabRole::Worker), None]);
        let _ = std::fs::remove_dir_all(root);
    }
}
