//! Reopening the last closed tab (`keys.reopen_tab`, `prefix+u` by default).
//!
//! Closing a tab ends its processes, so this opens a new tab in the same
//! space, in the same place, with its working directory and its own name; it
//! is not an undo. A plain shell starts fresh: no command is replayed, no
//! agent is resumed, no jobs come back.
//!
//! Client-local and in memory: only a close this client made and the server
//! accepted is remembered (not a process that exited, nor another client's
//! close), at most [`MAX_CLOSED_TABS`] of them, the newest first out. A
//! server-owned history is the later step for those and for restarts.

use super::*;

/// Closed tabs kept; the oldest is dropped past this.
pub(super) const MAX_CLOSED_TABS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClosedTab {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    /// The tab's own name; none for an automatic one, which would be stale.
    pub(super) label: Option<String>,
    /// The working directory of its focused pane.
    pub(super) cwd: Option<String>,
    /// The top-level tab that stood before it, to open after; none when it
    /// was first.
    pub(super) after_tab_id: Option<String>,
}

impl ClientShellState {
    /// What reopening `tab_id` needs, taken before it closes. None for a tab
    /// with a parent or with child tabs (jobs): those do not come back.
    pub(super) fn closed_tab_record(&self, tab_id: &str) -> Option<ClosedTab> {
        let snapshot = self.snapshot.as_deref()?;
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
        if tab.parent_tab_id.is_some()
            || snapshot
                .tabs
                .iter()
                .any(|other| other.parent_tab_id.as_deref() == Some(tab_id))
        {
            return None;
        }
        let tops = snapshot
            .tabs
            .iter()
            .filter(|other| other.workspace_id == tab.workspace_id && other.parent_tab_id.is_none())
            .collect::<Vec<_>>();
        let at = tops.iter().position(|other| other.tab_id == tab_id)?;
        let panes = snapshot
            .panes
            .iter()
            .filter(|pane| pane.tab_id == tab_id)
            .collect::<Vec<_>>();
        let pane = panes.iter().find(|pane| pane.focused).or(panes.first())?;
        Some(ClosedTab {
            endpoint_id: self.active_endpoint_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            label: tab.custom_label.then(|| tab.label.clone()),
            cwd: pane.foreground_cwd.clone().or_else(|| pane.cwd.clone()),
            after_tab_id: at
                .checked_sub(1)
                .and_then(|before| tops.get(before))
                .map(|before| before.tab_id.clone()),
        })
    }

    /// Closes a tab, remembering it for [`Self::reopen_closed_tab`] once the
    /// server has accepted the close.
    pub(super) fn push_tab_close(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        let closed = self.closed_tab_record(&tab_id).map(Box::new);
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::TabClose(crate::api::schema::TabTarget { tab_id }),
            PendingEndpointKind::TabClose { closed },
            outcome,
        );
    }

    pub(super) fn remember_closed_tab(&mut self, closed: ClosedTab) {
        self.closed_tabs.push_back(closed);
        while self.closed_tabs.len() > MAX_CLOSED_TABS {
            self.closed_tabs.pop_front();
        }
    }

    /// Opens the most recently closed tab again, newest first. Entries whose
    /// space is gone (or on another machine) are skipped; the entry is used
    /// up as soon as the request is sent, so a second press never repeats it.
    pub(super) fn reopen_closed_tab(&mut self, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        while let Some(closed) = self.closed_tabs.pop_back() {
            let space_exists = closed.endpoint_id == self.active_endpoint_id
                && self.snapshot.as_deref().is_some_and(|snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.workspace_id == closed.workspace_id)
                });
            if !space_exists {
                continue;
            }
            let params = crate::api::schema::TabCreateParams {
                workspace_id: Some(closed.workspace_id.clone()),
                cwd: closed.cwd.clone(),
                focus: true,
                label: closed.label.clone(),
                env: Default::default(),
            };
            self.push_endpoint_method_with_kind(
                crate::api::schema::Method::TabCreate(params),
                PendingEndpointKind::ReopenTab {
                    closed: Box::new(closed),
                },
                outcome,
            );
            return;
        }
        self.receive_endpoint_unavailable("no closed tab to reopen".to_owned());
    }

    /// Puts the reopened tab back after the tab that stood before it, when
    /// that one is still there; otherwise it stays last. A failed create
    /// keeps the entry's place in line for another try.
    pub(super) fn complete_reopen(
        &mut self,
        closed: ClosedTab,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Ok(crate::api::schema::ResponseResult::TabCreated { tab, .. }) = result else {
            // The create was refused (a missing directory, say): keep the
            // entry for another try. The notice came from the generic path.
            self.closed_tabs.push_back(closed);
            return (true, Vec::new());
        };
        let mut outcome = ClientShellInput::default();
        let insert = self.snapshot.as_deref().and_then(|snapshot| {
            let tops = snapshot
                .tabs
                .iter()
                .filter(|other| {
                    other.workspace_id == closed.workspace_id
                        && other.parent_tab_id.is_none()
                        && other.tab_id != tab.tab_id
                })
                .collect::<Vec<_>>();
            let main_index = match closed.after_tab_id.as_deref() {
                None => 0,
                Some(after) => tops.iter().position(|other| other.tab_id == after)? + 1,
            };
            // Already last: nothing to move.
            (main_index < tops.len()).then(|| {
                super::tab_groups::workspace_flat_insert_index(
                    snapshot,
                    &closed.workspace_id,
                    main_index,
                )
            })
        });
        if let Some(insert_index) = insert {
            self.push_endpoint_method(
                crate::api::schema::Method::TabMove(crate::api::schema::TabMoveParams {
                    tab_id: tab.tab_id,
                    insert_index,
                }),
                &mut outcome,
            );
        }
        (true, outcome.actions)
    }
}
