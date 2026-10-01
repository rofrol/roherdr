//! Notification history: a button at the right end of the spaces header
//! lists the active machine's past notifications (`notification.list`,
//! newest first, with the time each arrived); an entry navigates like
//! clicking its toast. The button counts the notifications that arrived for
//! a tab this client has not shown since; showing the tab clears them. Kept
//! per client, so one client does not clear another's count.
//!
//! Nothing is requested in the background: the count comes from the
//! notifications the client receives anyway, and the list is fetched when
//! the dropdown opens. A background request would hold the machine's
//! command lane, and a click in that moment would be refused as busy.

use std::collections::HashMap;

use crate::api::schema::NotificationRecord;

use super::*;

/// Entries the dropdown shows at most; the server keeps 100.
const MAX_ROWS: usize = 15;

#[derive(Debug, Default)]
pub(super) struct NotificationLog {
    /// Oldest first, as the server returned them when the dropdown opened.
    pub(super) entries: Vec<NotificationRecord>,
    /// Notifications that arrived for each tab not shown since.
    unread_tabs: HashMap<String, usize>,
    /// The machine and server run the counts belong to.
    source: Option<(ClientEndpointId, String)>,
    unsupported_boot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientNotificationLogOverlay {
    /// Index into the rows, newest first.
    pub(super) highlighted: usize,
    /// What the rows list.
    pub(super) view: NotificationLogView,
}

/// The dropdowns behind the header buttons: the past notifications, and
/// the agents working or asking now, both drawn as one list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NotificationLogView {
    History,
    Working,
    Asking,
    /// The bookmarked tabs, in the order of the spaces and their tabs.
    Bookmarks,
}

impl ClientShellState {
    /// Forgets the counts and entries of another machine or server run.
    fn sync_notification_log_source(&mut self) {
        let source = self
            .endpoint_boot_id(&self.active_endpoint_id)
            .map(|boot| (self.active_endpoint_id.clone(), boot.to_owned()));
        if self.notification_log.source != source {
            self.notification_log.entries.clear();
            self.notification_log.unread_tabs.clear();
            self.notification_log.source = source;
        }
    }

    /// Counts a notification for its tab unless that tab is shown.
    pub(super) fn notification_log_received(&mut self, tab_id: Option<&str>) {
        self.sync_notification_log_source();
        let Some(tab_id) = tab_id else {
            return;
        };
        let shown = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.as_deref())
            == Some(tab_id);
        if !shown {
            *self
                .notification_log
                .unread_tabs
                .entry(tab_id.to_owned())
                .or_default() += 1;
        }
    }

    /// Showing a tab reads its notifications.
    pub(super) fn mark_focused_tab_notifications_read(&mut self) {
        if let Some(tab_id) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.as_deref())
        {
            self.notification_log.unread_tabs.remove(tab_id);
        }
    }

    pub(super) fn complete_notification_list(
        &mut self,
        endpoint_id: ClientEndpointId,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let entries = match result {
            Ok(crate::api::schema::ResponseResult::NotificationList { notifications }) => {
                notifications
            }
            Ok(_) => return (false, Vec::new()),
            Err(error) => {
                if error.code.as_deref() == Some("unsupported_method") {
                    self.notification_log.unsupported_boot =
                        self.endpoint_boot_id(&endpoint_id).map(str::to_owned);
                }
                return (false, Vec::new());
            }
        };
        if endpoint_id != self.active_endpoint_id {
            return (false, Vec::new());
        }
        self.sync_notification_log_source();
        self.notification_log.entries = entries;
        (true, Vec::new())
    }

    /// The history button's count, or none when the machine cannot list
    /// its notifications (the button then hides).
    pub(super) fn notification_log_button(&self) -> Option<usize> {
        let method = crate::api::schema::Method::NotificationList(Default::default());
        let boot_id = self.endpoint_boot_id(&self.active_endpoint_id);
        if !self.supports_endpoint_method(&method)
            || (boot_id.is_some() && self.notification_log.unsupported_boot.as_deref() == boot_id)
        {
            return None;
        }
        let log = &self.notification_log;
        let current = log.source.as_ref().is_some_and(|(endpoint, boot)| {
            *endpoint == self.active_endpoint_id && Some(boot.as_str()) == boot_id
        });
        Some(if current {
            log.unread_tabs.values().sum()
        } else {
            0
        })
    }

    /// The view of the open dropdown, or none when no list is open.
    pub(super) fn open_notification_list(&self) -> Option<NotificationLogView> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::NotificationLog(log)) => Some(log.view),
            _ => None,
        }
    }

    /// The view the open dropdown shows.
    pub(super) fn notification_log_view(&self) -> NotificationLogView {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::NotificationLog(log)) => log.view,
            _ => NotificationLogView::History,
        }
    }

    /// The dropdown's rows: the notifications newest first, or the agents
    /// of the view in the snapshot's order, as records so the same row text
    /// and jump apply.
    pub(super) fn notification_log_rows(&self) -> Vec<NotificationRecord> {
        match self.notification_log_view() {
            NotificationLogView::History => self
                .notification_log
                .entries
                .iter()
                .rev()
                .take(MAX_ROWS)
                .cloned()
                .collect(),
            NotificationLogView::Bookmarks => self.bookmark_rows(),
            view => self.agent_rows(view),
        }
    }

    pub(super) fn bookmark_count(&self) -> usize {
        self.snapshot.as_deref().map_or(0, |snapshot| {
            snapshot.tabs.iter().filter(|tab| tab.bookmarked).count()
        })
    }

    /// Bookmarked tabs as records, in the order of the spaces, then the tabs
    /// inside each space: computed from the snapshot, so a moved, renamed or
    /// reordered tab or space shows right away.
    fn bookmark_rows(&self) -> Vec<NotificationRecord> {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return Vec::new();
        };
        snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| {
                snapshot
                    .tabs
                    .iter()
                    .filter(|tab| tab.bookmarked && tab.workspace_id == workspace.workspace_id)
            })
            .map(|tab| NotificationRecord {
                id: 0,
                unix_ms: 0,
                kind: "bookmark".into(),
                title: tab.label.clone(),
                body: None,
                agent: None,
                workspace_id: Some(tab.workspace_id.clone()),
                tab_id: Some(tab.tab_id.clone()),
                pane_id: None,
                task: None,
                request: None,
                repeats: None,
            })
            .collect()
    }

    /// The tab state icon and colour for each row of the open list, as the
    /// sidebar's tab lines draw them, for the rows whose tab still exists;
    /// none for the others (they keep their mark in the text).
    pub(super) fn notification_row_icons(
        &self,
        rows: &[NotificationRecord],
    ) -> Vec<Option<(&'static str, ratatui::style::Color)>> {
        let snapshot = self.snapshot.as_deref();
        rows.iter()
            .map(|row| {
                let snapshot = snapshot?;
                let tab = snapshot
                    .tabs
                    .iter()
                    .find(|tab| Some(tab.tab_id.as_str()) == row.tab_id.as_deref())?;
                Some(super::space_tabs::tab_state_icon(
                    snapshot,
                    tab,
                    &self.config,
                ))
            })
            .collect()
    }

    /// Removes the bookmark of the highlighted row (bookmarks view only).
    pub(super) fn remove_highlighted_bookmark(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_ref() else {
            return;
        };
        if log.view != NotificationLogView::Bookmarks {
            return;
        }
        let highlighted = log.highlighted;
        self.remove_bookmark_row(highlighted, outcome);
    }

    pub(super) fn remove_bookmark_row(&mut self, index: usize, outcome: &mut ClientShellInput) {
        let Some(tab_id) = self
            .bookmark_rows()
            .into_iter()
            .nth(index)
            .and_then(|row| row.tab_id)
        else {
            return;
        };
        self.push_endpoint_method(
            crate::api::schema::Method::TabBookmark(crate::api::schema::TabBookmarkParams {
                tab_id,
                bookmarked: false,
            }),
            outcome,
        );
        // The list shrinks when the snapshot comes back; keep the highlight
        // inside it meanwhile.
        outcome.repaint = true;
    }

    /// Agents that ask for the user (blocked on an approval or awaiting a
    /// reply) and agents working, one pane in at most one of them: attention
    /// wins. Counted from the snapshot the client already has.
    fn agent_is_asking(agent: &crate::protocol::ClientShellAgent) -> bool {
        agent.agent_status == crate::api::schema::AgentStatus::Blocked || agent.awaiting_reply
    }

    /// Working as the sidebar shows it: the agent works, or it waits on a
    /// job it started (the purple icon), unless it asks for the user.
    fn agent_is_working(
        snapshot: &crate::protocol::ClientShellSnapshot,
        agent: &crate::protocol::ClientShellAgent,
    ) -> bool {
        !Self::agent_is_asking(agent)
            && (agent.agent_status == crate::api::schema::AgentStatus::Working
                || super::agent_mark(snapshot, agent) == super::AgentMark::WaitsOnJob)
    }

    pub(super) fn agent_indicator_counts(&self) -> (usize, usize) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return (0, 0);
        };
        let asking = snapshot
            .agents
            .iter()
            .filter(|agent| Self::agent_is_asking(agent))
            .count();
        let working = snapshot
            .agents
            .iter()
            .filter(|agent| Self::agent_is_working(snapshot, agent))
            .count();
        (working, asking)
    }

    fn agent_rows(&self, view: NotificationLogView) -> Vec<NotificationRecord> {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return Vec::new();
        };
        snapshot
            .agents
            .iter()
            .filter(|agent| match view {
                NotificationLogView::Asking => Self::agent_is_asking(agent),
                _ => Self::agent_is_working(snapshot, agent),
            })
            .take(MAX_ROWS)
            .map(|agent| NotificationRecord {
                id: 0,
                unix_ms: 0,
                kind: if view == NotificationLogView::Asking {
                    "asking"
                } else {
                    "working"
                }
                .into(),
                title: agent
                    .display_agent
                    .clone()
                    .or(agent.agent.clone())
                    .unwrap_or_default(),
                body: if view == NotificationLogView::Asking {
                    Some(
                        if agent.agent_status == crate::api::schema::AgentStatus::Blocked {
                            "approval".to_owned()
                        } else {
                            "reply".to_owned()
                        },
                    )
                } else if agent.agent_status != crate::api::schema::AgentStatus::Working {
                    Some("waits on a job".to_owned())
                } else {
                    None
                },
                agent: agent.display_agent.clone().or(agent.agent.clone()),
                workspace_id: Some(agent.workspace_id.clone()),
                tab_id: Some(agent.tab_id.clone()),
                pane_id: Some(agent.pane_id.clone()),
                task: agent
                    .terminal_title_stripped
                    .as_deref()
                    .or(agent.terminal_title.as_deref())
                    .and_then(super::notification_policy::notification_detail_text),
                request: None,
                repeats: None,
            })
            .collect()
    }

    /// One history row: `✓ Fix the login test · claude · herdr ×3`. The task is
    /// the pane's title when the notification fired; without one the row says
    /// what happened (`✓ claude finished · herdr`). The workspace is named by
    /// its label, never by its position, and the tab by nothing: the auto
    /// number says nothing. Other kinds keep their title and body.
    pub(super) fn notification_row_text(&self, entry: &NotificationRecord) -> String {
        let mark = match entry.kind.as_str() {
            "needs_attention" | "asking" => "?",
            "working" => "◐",
            "bookmark" => "★",
            "finished" => "✓",
            _ => {
                return match entry.body.as_deref() {
                    Some(body) => format!("{} · {body}", entry.title),
                    None => entry.title.clone(),
                }
            }
        };
        let workspace = entry.workspace_id.as_deref().and_then(|id| {
            self.snapshot
                .as_deref()?
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == id)
                .map(|workspace| workspace.label.clone())
        });
        // A tab the user renamed shows its name first, as the sidebar does, and
        // keeps the task after it so the name does not hide what the agent
        // does. It is looked up by tab id, so a rename also reaches old rows.
        let tab_name = entry.tab_id.as_deref().and_then(|tab_id| {
            self.snapshot
                .as_deref()?
                .tabs
                .iter()
                .find(|tab| tab.tab_id == tab_id && tab.custom_label)
                .map(|tab| tab.label.clone())
        });
        if entry.kind == "bookmark" {
            let mut text = format!("{mark} {}", entry.title);
            if let Some(workspace) = workspace {
                text.push_str(&format!(" · {workspace}"));
            }
            return text;
        }
        let mut parts = Vec::new();
        match (entry.task.as_deref(), entry.agent.as_deref()) {
            (Some(task), agent) => {
                match tab_name.as_deref().filter(|name| *name != task) {
                    Some(name) => parts.push(format!("{mark} {name} · {task}")),
                    None => parts.push(format!("{mark} {task}")),
                }
                parts.extend(agent.map(str::to_owned));
            }
            (None, _) => match tab_name {
                Some(name) => parts.push(format!("{mark} {name} · {}", entry.title)),
                None => parts.push(format!("{mark} {}", entry.title)),
            },
        }
        parts.extend(workspace);
        // A live row says what it waits for (an approval or a reply).
        if matches!(entry.kind.as_str(), "asking" | "working") {
            parts.extend(entry.body.clone());
        }
        let mut text = parts.join(" · ");
        if let Some(request) = entry.request.as_deref() {
            text.push_str(&format!(" — “{request}”"));
        }
        if let Some(repeats) = entry.repeats.filter(|repeats| *repeats > 1) {
            text.push_str(&format!(" ×{repeats}"));
        }
        text
    }

    /// Whether an entry's tab still has notifications not seen.
    pub(super) fn notification_is_unread(&self, entry: &NotificationRecord) -> bool {
        entry
            .tab_id
            .as_deref()
            .is_some_and(|tab_id| self.notification_log.unread_tabs.contains_key(tab_id))
    }

    /// Opens the dropdown and fetches the list, or closes it.
    pub(super) fn toggle_notification_log(&mut self, outcome: &mut ClientShellInput) {
        self.toggle_notification_view(NotificationLogView::History, outcome);
    }

    /// Opens the dropdown of `view`, or closes it. Only the history is
    /// fetched; the agent views come from the snapshot.
    pub(super) fn toggle_notification_view(
        &mut self,
        view: NotificationLogView,
        outcome: &mut ClientShellInput,
    ) {
        if matches!(self.overlay, Some(ClientShellOverlay::NotificationLog(_))) {
            self.overlay = None;
            return;
        }
        self.sync_notification_log_source();
        self.overlay = Some(ClientShellOverlay::NotificationLog(
            ClientNotificationLogOverlay {
                highlighted: 0,
                view,
            },
        ));
        if view != NotificationLogView::History {
            return;
        }
        let endpoint_id = self.active_endpoint_id.clone();
        let method = crate::api::schema::Method::NotificationList(Default::default());
        if self.endpoint_is_online(&endpoint_id) && self.supports_endpoint_method(&method) {
            self.push_endpoint_method_with_kind(
                method,
                PendingEndpointKind::NotificationList { endpoint_id },
                outcome,
            );
        }
    }

    pub(super) fn move_notification_log_selection(&mut self, delta: isize) {
        let rows = self.notification_log_rows().len();
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            log.highlighted = (log.highlighted as isize + delta)
                .clamp(0, rows.saturating_sub(1) as isize) as usize;
        }
    }

    /// Opens the entry's pane, else its tab, else its space, like its
    /// toast; says so when none of them is left.
    pub(super) fn activate_notification_log_row(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(entry) = self.notification_log_rows().into_iter().nth(index) else {
            return;
        };
        self.overlay = None;
        outcome.repaint = true;
        if let Some(tab_id) = entry.tab_id.as_deref() {
            self.notification_log.unread_tabs.remove(tab_id);
        }
        let method = self.snapshot.as_deref().and_then(|snapshot| {
            use crate::api::schema::{Method, PaneTarget, TabTarget, WorkspaceTarget};
            if let Some(pane_id) = entry
                .pane_id
                .clone()
                .filter(|id| snapshot.panes.iter().any(|pane| pane.pane_id == *id))
            {
                return Some(Method::PaneFocus(PaneTarget { pane_id }));
            }
            if let Some(tab_id) = entry
                .tab_id
                .clone()
                .filter(|id| snapshot.tabs.iter().any(|tab| tab.tab_id == *id))
            {
                return Some(Method::TabFocus(TabTarget { tab_id }));
            }
            entry
                .workspace_id
                .clone()
                .filter(|id| {
                    snapshot
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.workspace_id == *id)
                })
                .map(|workspace_id| Method::WorkspaceFocus(WorkspaceTarget { workspace_id }))
        });
        match method {
            Some(method) => self.push_endpoint_method(method, outcome),
            None if entry.pane_id.is_some()
                || entry.tab_id.is_some()
                || entry.workspace_id.is_some() =>
            {
                self.receive_endpoint_unavailable("target no longer exists".to_owned());
            }
            None => {}
        }
    }
}

/// `HH:MM` for today, `Mon DD HH:MM` for older days, in the local time
/// offset `utc_offset_secs`.
pub(super) fn notification_time(unix_ms: u64, now_unix: u64, utc_offset_secs: i64) -> String {
    let local = |secs: i64| {
        time::OffsetDateTime::from_unix_timestamp(secs + utc_offset_secs)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
    };
    let at = local((unix_ms / 1000) as i64);
    let now = local(now_unix as i64);
    if at.date() == now.date() {
        format!("{:02}:{:02}", at.hour(), at.minute())
    } else {
        format!(
            "{} {:>2} {:02}:{:02}",
            &at.month().to_string()[..3],
            at.day(),
            at.hour(),
            at.minute()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_show_the_date_only_for_other_days() {
        // 2026-09-28 22:05:00 UTC.
        let at = 1_790_633_100_000;
        assert_eq!(notification_time(at, 1_790_633_200, 0), "22:05");
        assert_eq!(notification_time(at, 1_790_633_200, 7200), "00:05");
        assert_eq!(notification_time(at, 1_790_720_000, 0), "Sep 28 22:05");
    }
}
