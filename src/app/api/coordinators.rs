//! `coordinator.*` requests and the coordinator tab role. Coordination
//! tenures live in the worker store (`crate::workers::coordinators`); these
//! run on the app because a tenure is bound to a pane, whose directory
//! names its repository, and because a tab's coordinator role follows the
//! tenures: a tab is marked coordinator exactly while one of its panes is
//! bound to an active tenure ([`App::sync_coordinator_roles`]).

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    CoordinatorAckParams, CoordinatorAllowlistRefusalParams, CoordinatorEndParams,
    CoordinatorHandoffAbortParams, CoordinatorHandoffParams, CoordinatorHandoffSuccessorParams,
    CoordinatorInfo, CoordinatorReadyParams, CoordinatorRecordOverrideParams,
    CoordinatorStartParams, CoordinatorStatusParams, NotificationShowParams, NotificationShowSound,
    ResponseResult, TabRole,
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
    crate::workers::repository_of_dir(dir).ok_or_else(|| {
        let hint = if crate::workers::home_repo::is_home(std::path::Path::new(dir)) {
            "; the home directory has no .git of its own: claim the repository rooted there \
             with `herdr coordinator start --repo ~` from a shell whose git finds it, which \
             names its git directory"
        } else {
            ""
        };
        WorkerError::Invalid(format!("{dir} is not in a git repository{hint}"))
    })
}

/// Records `git_dir` as the git directory of the repository rooted at the
/// home directory `repo` names (an explicit claim's): a directory other
/// than HOME needs no git directory, and the claim then goes on as usual.
fn register_home(repo: &str, git_dir: &str) -> Result<(), WorkerError> {
    if !crate::workers::home_repo::is_home(std::path::Path::new(repo)) {
        return Ok(());
    }
    crate::workers::home_repo::register(std::path::Path::new(repo), std::path::Path::new(git_dir))?;
    crate::workers::forget_repositories();
    Ok(())
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
        // A session whose tenure was handed over starts no new one.
        if let Err(error) = coordinators.fence_pane_session(
            Some(&self.canonical_pane_id(pane)),
            self.pane_agent_session(pane).as_deref(),
        ) {
            return encode_error(id, error.code(), error.to_string());
        }
        let started = match (&params.repo, &params.home_git_dir) {
            (None, Some(_)) => Err(WorkerError::Invalid(
                "home_git_dir needs repo, the home directory".into(),
            )),
            (Some(repo), Some(git_dir)) => register_home(repo, git_dir),
            _ => Ok(()),
        }
        .and_then(|()| {
            self.start_coordinator(coordinators, ws_idx, pane_id, params.repo.as_deref())
        });
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
        // From a frozen pane the end would silently drop the pending
        // handoff; from a superseded one it is no tenure's to end.
        if let Some(pane) = &params.pane_id {
            if let Err(error) = coordinators.fence_pane_session(
                Some(&self.canonical_pane_id(pane)),
                self.pane_agent_session(pane).as_deref(),
            ) {
                return encode_error(id, error.code(), error.to_string());
            }
        }
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
        let fenced = coordinators
            .fence_tenure(params.coordinator_id.as_deref())
            .and_then(|()| {
                coordinators.fence_pane_session(
                    from_pane.as_deref(),
                    from_pane
                        .as_deref()
                        .and_then(|pane| self.pane_agent_session(pane))
                        .as_deref(),
                )
            });
        if let Err(error) = fenced {
            return encode_error(id, error.code(), error.to_string());
        }
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

    /// The pane's agent cleared its session (`/clear`) from `from_session`
    /// into the one the pane has now: a tenure bound to the pane keeps
    /// coordinating with the new session
    /// ([`WorkerSupervisor::coordinator_session_cleared`]).
    pub(super) fn clear_coordinator_session_in(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        from_session: Option<&str>,
    ) {
        let Some(coordinators) = crate::workers::coordinators() else {
            return;
        };
        let Some(pane) = self.pane_metadata(ws_idx, pane_id) else {
            return;
        };
        let Some(session) = pane.agent_session.map(|session| session.value) else {
            return;
        };
        if from_session == Some(session.as_str()) {
            return;
        }
        match coordinators.coordinator_session_cleared(
            &pane.pane_id,
            from_session,
            &session,
            Some(&pane.workspace_id),
        ) {
            Ok(Some(_)) => self.sync_coordinator_roles(),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                %error,
                pane_id = pane.pane_id,
                "cannot move the coordination tenure to the cleared agent session"
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
    /// directory in it), else of the pane's directory. The repository
    /// rooted at the home directory only with `repo`: a pane's directory
    /// there is a candidate, never a claim.
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
        let explicit = repo;
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
        if explicit.is_none() && crate::workers::home_repo::is_home_repository(&repo) {
            return Err(WorkerError::Invalid(format!(
                "the directory {dir} of pane {} is in the repository rooted at the home \
                 directory {repo}, which a coordinator claims only explicitly: `herdr \
                 coordinator start --repo ~`",
                pane.pane_id
            )));
        }
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
    /// The agent session the pane `pane` runs now.
    pub(crate) fn pane_agent_session(&self, pane: &str) -> Option<String> {
        let (ws_idx, pane_id) = self.parse_pane_id(&self.canonical_pane_id(pane))?;
        self.pane_metadata(ws_idx, pane_id)?
            .agent_session
            .map(|session| session.value)
    }

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

/// The name of the agent a handoff starts in the successor's pane.
fn successor_agent_name(epoch: i64) -> String {
    format!("successor-e{epoch}")
}

impl App {
    /// The coordinator's pane `pane` as the handoff needs it: its position
    /// and the label of the agent it runs.
    fn handoff_pane(
        &self,
        pane: &str,
    ) -> Result<
        (
            usize,
            crate::layout::PaneId,
            String,
            crate::api::schema::AgentStatus,
        ),
        WorkerError,
    > {
        let (ws_idx, pane_id) = self
            .parse_pane_id(pane)
            .ok_or_else(|| WorkerError::Invalid(format!("pane not found: {pane}")))?;
        let metadata = self
            .pane_metadata(ws_idx, pane_id)
            .ok_or_else(|| WorkerError::Invalid(format!("pane not found: {pane}")))?;
        let kind = metadata.agent.ok_or_else(|| {
            WorkerError::HandoffRefused(format!("pane {pane} runs no recognized agent"))
        })?;
        Ok((ws_idx, pane_id, kind, metadata.agent_status))
    }

    /// `coordinator.ready`: the caller's tenure freezes with the agent's note.
    ///
    /// The command runs from inside the agent's own turn, so its live state
    /// is `working`; "at a turn end" is therefore enforced as: the agent is
    /// one a handoff can start (has a launch prompt) and is not blocked on a
    /// question or permission, and the skill ends its turn right after
    /// `handoff --successor`. A kind without a launch prompt is refused here,
    /// so nothing is left frozen.
    pub(super) fn handle_coordinator_ready(
        &mut self,
        id: String,
        params: CoordinatorReadyParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let pane = self.canonical_pane_id(&params.pane_id);
        let result = (|| {
            coordinators
                .fence_pane_session(Some(&pane), self.pane_agent_session(&pane).as_deref())
                .or_else(|error| {
                    // A frozen tenure takes a new note.
                    if error.code() == "coordinator_frozen" {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })?;
            let (_, _, kind, status) = self.handoff_pane(&pane)?;
            if crate::workers::coordinator_handoff::launch_prompt(&kind, std::path::Path::new("/"))
                .is_none()
            {
                return Err(WorkerError::HandoffRefused(format!(
                    "agent kind {kind} cannot take a coordinator over by itself"
                )));
            }
            if status == crate::api::schema::AgentStatus::Blocked {
                return Err(WorkerError::HandoffRefused(
                    "the agent is blocked on a question or a permission, not at a turn end".into(),
                ));
            }
            coordinators.handoff_ready(&pane, std::path::Path::new(&params.note_path))
        })();
        match result {
            Ok(coordinator) => encode_success(id, ResponseResult::Coordinator { coordinator }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// `coordinator.handoff_successor`: writes the snapshot and manifest,
    /// opens a pane next to the caller's without moving focus and starts one
    /// fresh session of the same agent kind there, with its single launch
    /// prompt. Any failure after that aborts the handoff: the pane closes
    /// and the tenure unfreezes.
    pub(super) fn handle_coordinator_handoff_successor(
        &mut self,
        id: String,
        params: CoordinatorHandoffSuccessorParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let pane = self.canonical_pane_id(&params.pane_id);
        let prepared = match coordinators
            .fence_pane_session(Some(&pane), self.pane_agent_session(&pane).as_deref())
            .or_else(|error| {
                // The frozen tenure is the one that starts its successor.
                if error.code() == "coordinator_frozen" {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .and_then(|()| self.handoff_pane(&pane))
            .and_then(|(_, _, kind, _)| {
                coordinators
                    .handoff_prepare(&pane, &kind)
                    .map(|prepared| (prepared, kind))
            }) {
            Ok(prepared) => prepared,
            Err(error) => return encode_error(id, error.code(), error.to_string()),
        };
        let (prepared, kind) = prepared;
        match self.start_successor(&pane, &kind, &prepared) {
            Ok(successor) => {
                let started =
                    coordinators.handoff_successor_started(&prepared.coordinator_id, &successor);
                match started {
                    Ok(coordinator) => {
                        encode_success(id, ResponseResult::Coordinator { coordinator })
                    }
                    Err(error) => {
                        self.abort_started_handoff(&prepared.coordinator_id, &successor, &error);
                        encode_error(id, error.code(), error.to_string())
                    }
                }
            }
            Err(error) => {
                coordinators.handoff_abort(
                    &prepared.coordinator_id,
                    &format!("the successor could not start ({error})"),
                );
                encode_error(id, error.code(), error.to_string())
            }
        }
    }

    fn abort_started_handoff(
        &mut self,
        coordinator_id: &str,
        successor: &str,
        error: &WorkerError,
    ) {
        if let Some(coordinators) = crate::workers::coordinators() {
            coordinators.handoff_abort(coordinator_id, &format!("the handoff failed ({error})"));
        }
        let _ = self.close_pane(
            String::new(),
            &crate::api::schema::PaneTarget {
                pane_id: successor.to_owned(),
            },
        );
    }

    /// Opens the successor's pane beside `old_pane` and starts the agent in
    /// it. A pane that was opened is closed again when the start fails.
    fn start_successor(
        &mut self,
        old_pane: &str,
        kind: &str,
        prepared: &crate::workers::coordinator_handoff::Prepared,
    ) -> Result<String, WorkerError> {
        let epoch = crate::workers::coordinators()
            .and_then(|c| c.coordinator_of_pane(old_pane))
            .map(|tenure| tenure.epoch)
            .unwrap_or_default();
        let split = self.handle_pane_split(
            String::new(),
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(old_pane.to_owned()),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: false,
                right_click: Default::default(),
                env: Default::default(),
            },
        );
        let split: serde_json::Value = serde_json::from_str(&split)
            .map_err(|error| WorkerError::Invalid(format!("unreadable split response: {error}")))?;
        let Some(successor) = split["result"]["pane"]["pane_id"]
            .as_str()
            .map(str::to_owned)
        else {
            return Err(WorkerError::Invalid(format!(
                "cannot open the successor's pane: {}",
                split["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error")
            )));
        };
        let started = self.start_agent(crate::api::schema::AgentStartParams {
            name: successor_agent_name(epoch),
            kind: kind.to_owned(),
            pane_id: successor.clone(),
            args: Vec::new(),
            timeout_ms: None,
            prompt: Some(prepared.prompt.clone()),
            caller_pane_id: None,
        });
        match started {
            Ok(_) => Ok(successor),
            Err(error) => {
                let body = self.agent_start_error_body(error);
                let _ = self.close_pane(
                    String::new(),
                    &crate::api::schema::PaneTarget { pane_id: successor },
                );
                Err(WorkerError::Invalid(format!(
                    "cannot start the successor: {}",
                    body.message
                )))
            }
        }
    }

    /// `coordinator.ack`: the successor takes the tenure over, and in the
    /// same step the old coordinator's pane closes (which ends its agent
    /// session). A refused ack changes nothing.
    pub(super) fn handle_coordinator_ack(
        &mut self,
        id: String,
        params: CoordinatorAckParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let pane = self.canonical_pane_id(&params.pane_id);
        let target = self
            .parse_pane_id(&pane)
            .and_then(|(ws_idx, pane_id)| self.pane_metadata(ws_idx, pane_id));
        let Some(target) = target else {
            return encode_error(id, "pane_not_found", format!("pane not found: {pane}"));
        };
        let session = target.agent_session.map(|session| session.value);
        let acked = coordinators.handoff_ack(
            &target.pane_id,
            params.epoch,
            &params.snapshot_sha256,
            session.as_deref(),
            Some(&target.workspace_id),
        );
        self.sync_coordinator_roles();
        match acked {
            Ok(acked) => {
                // The old session ends with its pane. It is fenced as
                // superseded already, so a pane that cannot be closed is
                // reported, not hidden.
                let closed = self.close_pane(
                    String::new(),
                    &crate::api::schema::PaneTarget {
                        pane_id: acked.old_pane.clone(),
                    },
                );
                let close_error = response_error(closed);
                if let Some(error) = &close_error {
                    tracing::warn!(pane = %acked.old_pane, %error, "cannot close the superseded coordinator pane");
                }
                self.sync_coordinator_roles();
                encode_success(
                    id,
                    ResponseResult::CoordinatorAcked {
                        coordinator: acked.tenure,
                        old_pane_id: acked.old_pane,
                        old_pane_closed: close_error.is_none(),
                        close_error: close_error.map(|error| {
                            format!(
                                "the old pane could not be closed ({error}); its session is \
                                 fenced as superseded"
                            )
                        }),
                    },
                )
            }
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    /// `coordinator.handoff_abort`: the pending handoff ends and the old
    /// tenure unfreezes. The successor's pane is closed unless the
    /// successor itself asked.
    pub(super) fn handle_coordinator_handoff_abort(
        &mut self,
        id: String,
        params: CoordinatorHandoffAbortParams,
    ) -> String {
        let Some(coordinators) = crate::workers::coordinators() else {
            return unavailable(id);
        };
        let pane = params
            .pane_id
            .as_deref()
            .map(|pane| self.canonical_pane_id(pane));
        let aborted = match coordinators
            .handoff_abort_request(pane.as_deref(), params.coordinator_id.as_deref())
        {
            Ok(aborted) => aborted,
            Err(error) => return encode_error(id, error.code(), error.to_string()),
        };
        let successor_pane_closed = aborted.close_pane.as_ref().is_some_and(|successor| {
            let closed = self.close_pane(
                String::new(),
                &crate::api::schema::PaneTarget {
                    pane_id: successor.clone(),
                },
            );
            response_error(closed).is_none()
        });
        self.sync_coordinator_roles();
        encode_success(
            id,
            ResponseResult::CoordinatorHandoffAborted {
                coordinator: aborted.tenure,
                successor_pane_id: aborted.close_pane,
                successor_pane_closed,
            },
        )
    }
}

/// The message of a failed `close_pane` (its encoded error response), `None`
/// for a success.
fn response_error(closed: Result<(), String>) -> Option<String> {
    let response = closed.err()?;
    let value: serde_json::Value = serde_json::from_str(&response).unwrap_or_default();
    Some(
        value["error"]["message"]
            .as_str()
            .unwrap_or("unknown error")
            .to_owned(),
    )
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
                home_git_dir: None,
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
                home_git_dir: None,
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

    /// Moves tab `tab_idx`'s panes to `dir`.
    fn set_cwd(app: &mut App, tab_idx: usize, dir: &std::path::Path) {
        let terminals: Vec<_> = app.state.workspaces[0].tabs[tab_idx]
            .panes
            .values()
            .map(|pane| pane.attached_terminal_id.clone())
            .collect();
        for terminal_id in terminals {
            app.state.terminals.insert(
                terminal_id.clone(),
                crate::terminal::TerminalState::new(terminal_id, dir.to_owned()),
            );
        }
    }

    fn start(app: &mut App, tab_idx: usize, repo: Option<&str>, git_dir: Option<&str>) -> String {
        let pane_id = pane_of(app, tab_idx);
        app.handle_coordinator_start(
            "req".into(),
            CoordinatorStartParams {
                repo: repo.map(str::to_owned),
                pane_id: Some(pane_id),
                home_git_dir: git_dir.map(str::to_owned),
            },
        )
    }

    /// The home directory's repository (a dotfiles repository with its git
    /// directory outside HOME): `coordinator start` in HOME failed with
    /// `invalid_request: <HOME> is not in a git repository`, because herdr
    /// looks for `.git` upward. Only an explicit `--repo ~`, with the git
    /// directory the CLI asks git for, claims it; a pane's directory there
    /// (by `coordinator.start` or the coordinator role) is never a claim,
    /// and a repository nested in HOME is claimed as itself.
    #[test]
    fn the_home_repository_is_claimed_only_explicitly() {
        use crate::workers::home_repo::tests::HomeLayout;
        let guard = crate::workers::home_repo::HomeForTest::set();
        let layout = HomeLayout::new("claim");
        guard.at(&layout.home);
        let (mut app, root) = app("home");
        set_cwd(&mut app, 0, &layout.home);
        set_cwd(&mut app, 1, &layout.home);
        let home = layout.home.display().to_string();
        let git_dir = layout.git_dir.display().to_string();

        // The failure, reproduced: HOME has no `.git`.
        let failed = start(&mut app, 0, None, None);
        assert!(failed.contains("invalid_request"), "{failed}");
        assert!(
            failed.contains(&format!("{home} is not in a git repository")),
            "{failed}"
        );
        assert!(failed.contains("--repo ~"), "{failed}");
        // A git directory without the home directory is refused.
        let refused = start(&mut app, 0, None, Some(&git_dir));
        assert!(refused.contains("home_git_dir needs repo"), "{refused}");

        // The explicit claim records the git directory and claims HOME.
        let started = start(&mut app, 0, Some(&home), Some(&git_dir));
        assert!(
            started.contains(&format!("\"repo\":\"{home}\"")),
            "{started}"
        );
        assert_eq!(roles(&app), [Some(TabRole::Coordinator), None]);
        let ended = app.handle_coordinator_end(
            "req".into(),
            CoordinatorEndParams {
                reason: None,
                coordinator_id: None,
                pane_id: Some(pane_of(&app, 0)),
            },
        );
        assert!(ended.contains("end_reason"), "{ended}");

        // Now that herdr knows HOME's repository, a pane there is still
        // only a candidate, by request or by role.
        let refused = start(&mut app, 1, None, None);
        assert!(refused.contains("claims only explicitly"), "{refused}");
        let refused = set_role(&mut app, 1, Some(TabRole::Coordinator)).unwrap_err();
        assert!(refused.contains("claims only explicitly"), "{refused}");
        assert!(active(&app).is_empty());

        // A repository nested in HOME is claimed from its pane as itself.
        let nested = layout.home.join("proj");
        std::fs::create_dir_all(&nested).unwrap();
        crate::workers::home_repo::tests::run(&nested, &["init", "-q"]);
        set_cwd(&mut app, 1, &nested);
        let started = start(&mut app, 1, None, None);
        assert!(
            started.contains(&format!("\"repo\":\"{}\"", nested.display())),
            "{started}"
        );
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

    fn give_agent(app: &mut App, pane: &str, agent: crate::detect::Agent) {
        let (ws_idx, pane_id) = app.parse_pane_id(pane).unwrap();
        let terminal_id = app.state.workspaces[ws_idx]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_agent_process_at(agent, std::time::Instant::now());
    }

    fn error_code(response: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(response).unwrap();
        value["error"]["code"].as_str().unwrap_or("").to_owned()
    }

    /// A coordinator hands over through `ready` and `ack`: a kind with no
    /// launch prompt is refused before anything freezes, a frozen tenure's
    /// `handoff --to` is refused, a wrong ack changes nothing, and the right
    /// one moves the tenure and the tab's role and closes the old pane.
    #[test]
    fn ready_and_ack_move_the_tenure_and_close_the_old_pane() {
        let (mut app, root) = app("self-service");
        let note = root.join("note.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&note, "carry on\n").unwrap();
        set_role(&mut app, 1, Some(TabRole::Coordinator)).unwrap();
        let old = pane_of(&app, 1);
        let successor = pane_of(&app, 0);
        let ready = |app: &mut App| {
            app.handle_coordinator_ready(
                "req".into(),
                CoordinatorReadyParams {
                    pane_id: old.clone(),
                    note_path: note.display().to_string(),
                },
            )
        };
        // No recognized agent, then one without a launch prompt.
        assert_eq!(error_code(&ready(&mut app)), "handoff_refused");
        give_agent(&mut app, &old, crate::detect::Agent::Codex);
        assert_eq!(error_code(&ready(&mut app)), "handoff_refused");
        assert!(active(&app)[0].handoff.is_none());
        let coordinators = crate::workers::coordinators().unwrap();
        // The ack of a tenure that is not handing over is refused.
        let ack = |app: &mut App, epoch: i64, sha: &str| {
            app.handle_coordinator_ack(
                "req".into(),
                CoordinatorAckParams {
                    pane_id: successor.clone(),
                    epoch,
                    snapshot_sha256: sha.into(),
                },
            )
        };
        assert_eq!(error_code(&ack(&mut app, 1, "ab")), "handoff_refused");

        give_agent(&mut app, &old, crate::detect::Agent::Claude);
        let response = ready(&mut app);
        assert!(response.contains("\"state\":\"frozen\""), "{response}");
        let handoff = app.handle_coordinator_handoff(
            "req".into(),
            CoordinatorHandoffParams {
                to_pane_id: successor.clone(),
                coordinator_id: None,
                pane_id: Some(old.clone()),
            },
        );
        assert_eq!(error_code(&handoff), "coordinator_frozen");
        // The successor's start (the pane and agent launch need a real
        // terminal) is stood in for by the supervisor's own steps.
        let prepared = coordinators.handoff_prepare(&old, "claude").unwrap();
        coordinators
            .handoff_successor_started(&prepared.coordinator_id, &successor)
            .unwrap();
        let status = active(&app).remove(0);
        let pending = status.handoff.unwrap();
        let sha = pending.snapshot_sha256.unwrap();
        assert_eq!(error_code(&ack(&mut app, 2, &sha)), "handoff_refused");
        assert_eq!(
            error_code(&ack(&mut app, 1, &"0".repeat(64))),
            "handoff_refused"
        );
        assert_eq!(active(&app)[0].pane_id.as_deref(), Some(old.as_str()));

        let response = ack(&mut app, 1, &sha);
        assert!(response.contains("\"epoch\":2"), "{response}");
        let tenure = active(&app).remove(0);
        assert_eq!(tenure.pane_id.as_deref(), Some(successor.as_str()));
        assert!(tenure.handoff.is_none());
        // The old pane is closed (its tab with it), the role followed.
        assert!(app.parse_pane_id(&old).is_none());
        assert_eq!(roles(&app), vec![Some(TabRole::Coordinator)]);
        let _ = std::fs::remove_dir_all(root);
    }

    fn report_session(app: &mut App, pane: &str, seq: u64, session: &str, start: &str) {
        let response = app.handle_pane_report_agent_session(
            "req".into(),
            crate::api::schema::PaneReportAgentSessionParams {
                pane_id: pane.to_owned(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(seq),
                agent_session_id: Some(session.to_owned()),
                agent_session_path: None,
                session_start_source: Some(start.to_owned()),
                resume_argv: None,
                turn_reports: false,
            },
        );
        serde_json::from_str::<SuccessResponse>(&response).unwrap();
    }

    #[test]
    fn a_cleared_coordinator_session_keeps_its_tenure() {
        let (mut app, root) = app("clear");
        let coordinator = pane_of(&app, 1);
        report_session(&mut app, &coordinator, 1, "s-before", "startup");
        set_role(&mut app, 1, Some(TabRole::Coordinator)).unwrap();
        let tenure = active(&app).remove(0);
        assert_eq!(tenure.session_id.as_deref(), Some("s-before"));

        // `/clear` in the coordinator's pane: the same tenure, now with the
        // new session, and the tab keeps its crown.
        report_session(&mut app, &coordinator, 2, "s-after", "clear");
        let cleared = active(&app);
        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].coordinator_id, tenure.coordinator_id);
        assert_eq!(
            (
                cleared[0].pane_id.as_deref(),
                cleared[0].session_id.as_deref()
            ),
            (Some(coordinator.as_str()), Some("s-after"))
        );
        assert_eq!(roles(&app), [None, Some(TabRole::Coordinator)]);

        // `claude --resume s-after` in the other pane finds the tenure.
        let other = pane_of(&app, 0);
        report_session(&mut app, &other, 1, "s-after", "resume");
        let resumed = active(&app);
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].coordinator_id, tenure.coordinator_id);
        assert_eq!(resumed[0].pane_id.as_deref(), Some(other.as_str()));
        assert_eq!(roles(&app), [Some(TabRole::Coordinator), None]);
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
