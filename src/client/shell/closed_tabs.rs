//! Reopening the last closed tab (`keys.reopen_tab`, `prefix+u` by default).
//!
//! Closing a tab ends its processes, so this opens a new tab in the same
//! space, in the same place, with its working directory and its own name; it
//! is not an undo. A plain shell starts fresh: no command is replayed, no
//! agent is resumed, no jobs come back. A tab with jobs comes back without
//! them, and says so. A job tab closed on its own cannot come back: its
//! entry only says so, so the press never reopens an older, unrelated tab.
//!
//! Client-local and in memory: only a close this client made and the server
//! accepted is remembered (not a process that exited, nor another client's
//! close), at most [`MAX_CLOSED_TABS`] of them, the newest first out.
//!
//! A server with `tab.reopen_closed` keeps what it closed (in memory, the
//! last closes): the tab comes back with its layout, the names and
//! directories of its panes, and its agents resumed where their sessions
//! can be. When it no longer has the tab (a restart, too many closes since),
//! the tab opens with a fresh shell as above, and says so.

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
    /// How many jobs (child tabs) closed with it; they do not come back.
    pub(super) jobs: usize,
    /// The closed tab was itself a job: reopening it only says it cannot.
    pub(super) job: bool,
    /// The closed tab's id, which the server keeps it under.
    pub(super) tab_id: String,
    /// The reopen was sent as `tab.reopen_closed`, which puts the tab in its
    /// place itself.
    pub(super) from_server: bool,
}

impl ClientShellState {
    /// What reopening `tab_id` needs, taken before it closes. Jobs closed
    /// together with their parent are not recorded: the parent's entry
    /// counts them.
    pub(super) fn closed_tab_record(&self, tab_id: &str) -> Option<ClosedTab> {
        let snapshot = self.snapshot.as_deref()?;
        let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
        if tab.parent_tab_id.is_some() {
            return Some(ClosedTab {
                endpoint_id: self.active_endpoint_id.clone(),
                workspace_id: tab.workspace_id.clone(),
                label: Some(tab.label.clone()),
                cwd: None,
                after_tab_id: None,
                jobs: 0,
                job: true,
                tab_id: tab_id.to_owned(),
                from_server: false,
            });
        }
        let jobs = super::tab_groups::child_tabs(snapshot, tab_id).len();
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
            jobs,
            job: false,
            tab_id: tab_id.to_owned(),
            from_server: false,
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

    /// Closes a pane; the tab's last one closes the tab, which is then
    /// remembered like a closed tab.
    pub(super) fn push_pane_close(&mut self, pane_id: String, outcome: &mut ClientShellInput) {
        let closed = self.snapshot.as_deref().and_then(|snapshot| {
            let pane = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id)?;
            let last_pane = !snapshot
                .panes
                .iter()
                .any(|other| other.tab_id == pane.tab_id && other.pane_id != pane_id);
            last_pane.then(|| pane.tab_id.clone())
        });
        let closed = closed
            .and_then(|tab_id| self.closed_tab_record(&tab_id))
            .map(Box::new);
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget { pane_id }),
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

    /// Says what a reopen press did or could not do. Every press shows it,
    /// also the same message again (a seen-once notice left a repeated
    /// press with no answer at all).
    fn reopen_notice(&mut self, body: &str) {
        self.push_endpoint_notice(
            ClientEndpointNoticeKind::Rejected,
            "reopen_tab",
            "Reopen tab",
            body,
        );
    }

    /// Opens the most recently closed tab again, newest first. Entries whose
    /// space is gone (or on another machine) are skipped; the entry is used
    /// up as soon as the request is sent, so a second press never repeats it.
    pub(super) fn reopen_closed_tab(&mut self, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let Some(snapshot) = self.snapshot.as_deref() else {
            self.reopen_notice("not connected: nothing to reopen yet");
            return;
        };
        // Spaces of this machine that are gone take their entries with them;
        // another machine's entries wait for that machine.
        let active = self.active_endpoint_id.clone();
        let spaces = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect::<std::collections::HashSet<_>>();
        self.closed_tabs
            .retain(|closed| closed.endpoint_id != active || spaces.contains(&closed.workspace_id));
        let Some(at) = self
            .closed_tabs
            .iter()
            .rposition(|closed| closed.endpoint_id == active)
        else {
            self.reopen_notice("no closed tab to reopen");
            return;
        };
        let Some(closed) = self.closed_tabs.remove(at) else {
            return;
        };
        if closed.job {
            let name = closed.label.unwrap_or_default();
            self.reopen_notice(&format!(
                "job {name} cannot be reopened; press again for the tab closed before it"
            ));
            return;
        }
        let reopen = crate::api::schema::Method::TabReopenClosed(
            crate::api::schema::TabReopenClosedParams {
                tab_id: closed.tab_id.clone(),
                after_tab_id: closed.after_tab_id.clone(),
                focus: true,
            },
        );
        if self.supports_endpoint_method(&reopen) {
            let mut closed = closed;
            closed.from_server = true;
            // The reopened tab takes the focus: open its collapsed space.
            let workspace_id = closed.workspace_id.clone();
            self.expand_for_jump(Some(&workspace_id), outcome);
            self.push_endpoint_method_with_kind(
                reopen,
                PendingEndpointKind::ReopenTab {
                    closed: Box::new(closed),
                },
                outcome,
            );
            return;
        }
        self.reopen_with_fresh_shell(closed, outcome);
    }

    /// Reopen without the server's copy: a new tab with a fresh shell in the
    /// tab's directory, with its name, moved to its place afterwards.
    fn reopen_with_fresh_shell(&mut self, mut closed: ClosedTab, outcome: &mut ClientShellInput) {
        closed.from_server = false;
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
    }

    /// Puts the reopened tab back after the tab that stood before it, when
    /// that one is still there; otherwise it stays last. A failed create
    /// keeps the entry's place in line for another try.
    pub(super) fn complete_reopen(
        &mut self,
        closed: ClosedTab,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let result = match result {
            // The server no longer has it: open it the plain way.
            Err(error)
                if closed.from_server && error.code.as_deref() == Some("closed_tab_not_found") =>
            {
                let name = closed.label.clone().unwrap_or_else(|| "the tab".into());
                self.reopen_notice(&format!(
                    "{name} was no longer kept; reopened with a fresh shell"
                ));
                let mut outcome = ClientShellInput::default();
                self.reopen_with_fresh_shell(closed, &mut outcome);
                return (true, outcome.actions);
            }
            result => result,
        };
        let Ok(crate::api::schema::ResponseResult::TabCreated { tab, .. }) = result else {
            // The create was refused (a missing directory, say): keep the
            // entry for another try. The notice came from the generic path.
            self.closed_tabs.push_back(closed);
            return (true, Vec::new());
        };
        if closed.jobs > 0 && closed.from_server {
            let jobs = if closed.jobs == 1 { "job" } else { "jobs" };
            self.reopen_notice(&format!(
                "{} reopened; its {} {jobs} did not come back",
                tab.label, closed.jobs
            ));
        } else if closed.jobs > 0 {
            let jobs = if closed.jobs == 1 {
                "job was"
            } else {
                "jobs were"
            };
            self.reopen_notice(&format!(
                "{} reopened with a fresh shell; its {} {jobs} not restored",
                tab.label, closed.jobs
            ));
        }
        let mut outcome = ClientShellInput::default();
        // The machine may have changed since the create was sent: the new
        // tab is on the other one, so leave it where it is. The server put
        // a reopened tab in its place already.
        if closed.endpoint_id != self.active_endpoint_id || closed.from_server {
            return (true, Vec::new());
        }
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
