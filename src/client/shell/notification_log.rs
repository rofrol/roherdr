//! Notification history: a button at the right end of the spaces header
//! lists the active machine's past notifications (`notification.list`,
//! newest first, with the time each arrived); an entry navigates like
//! clicking its toast. The button counts the tabs this client received a
//! notification for and has not shown since; showing the tab clears it. Kept
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
    /// Notices this client raised itself (a launcher's failure), oldest
    /// first: the server never sees them, so they are kept here, apart from
    /// the fetched entries and across machines.
    local_entries: Vec<NotificationRecord>,
    /// The local entries not opened yet.
    unread_local: std::collections::HashSet<u64>,
}

/// Local entries count down from the top of the id range, so they never
/// meet the server's ids, which count up from zero.
const FIRST_LOCAL_ID: u64 = u64::MAX;

/// Local entries kept at most; the oldest go first.
const MAX_LOCAL_ENTRIES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientNotificationLogOverlay {
    /// The row Enter, `x` and Delete act on, kept by what it shows so it
    /// stays on the same agent, tab or notification when the rows change
    /// under it. None until the pointer or an arrow key picks a row: the list
    /// opens on a click, and a row lit at once looked current and made `x`
    /// remove the first bookmark nobody chose.
    pub(super) highlighted: Option<RowKey>,
    /// What the rows list.
    pub(super) view: NotificationLogView,
    /// A row's tab menu, open over the list: it closes alone, and an
    /// action that needs no dialog (a bookmark, finished jobs) leaves the
    /// list open.
    pub(super) menu: Option<ListRowMenu>,
}

/// What a dropdown row shows: a past notification, an agent's pane, or a
/// bookmarked tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RowKey {
    Notification(u64),
    Pane(String),
    Tab(String),
    Space(String),
}

impl RowKey {
    fn of(view: NotificationLogView, row: &NotificationRecord) -> Option<Self> {
        match view {
            NotificationLogView::History => Some(Self::Notification(row.id)),
            NotificationLogView::Working | NotificationLogView::Asking => {
                row.pane_id.clone().map(Self::Pane)
            }
            NotificationLogView::Bookmarks => row
                .tab_id
                .clone()
                .map(Self::Tab)
                .or_else(|| row.workspace_id.clone().map(Self::Space)),
        }
    }
}

/// What a header list's row menu acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ListRowTarget {
    Tab(String),
    /// A bookmarked space's row.
    Space(String),
}

/// The menu of a header list's row: its tab or space, where it opened and
/// its highlighted item. The items come from the snapshot each time, so job
/// counts are never stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListRowMenu {
    pub(super) target: ListRowTarget,
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) highlighted: usize,
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

    /// Keeps a notice this client raised as an unread history entry, so it
    /// can leave the screen without being lost.
    pub(super) fn record_local_notification(&mut self, title: String, body: Option<String>) {
        let log = &mut self.notification_log;
        let id = log
            .local_entries
            .last()
            .map_or(FIRST_LOCAL_ID, |last| last.id.saturating_sub(1));
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        log.local_entries.push(NotificationRecord {
            id,
            unix_ms,
            kind: "custom".into(),
            title,
            body,
            agent: None,
            workspace_id: None,
            tab_id: None,
            pane_id: None,
            task: None,
            request: None,
            repeats: None,
        });
        log.unread_local.insert(id);
        if log.local_entries.len() > MAX_LOCAL_ENTRIES {
            let dropped = log.local_entries.remove(0);
            log.unread_local.remove(&dropped.id);
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
        // Unread tabs, not arrivals: the list marks one row per unread tab and
        // collapses repeats, so the count matches the marks it shows.
        let unread_tabs = if current { log.unread_tabs.len() } else { 0 };
        Some(unread_tabs + log.unread_local.len())
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
            NotificationLogView::History => {
                let log = &self.notification_log;
                let mut rows = log
                    .entries
                    .iter()
                    .rev()
                    .chain(log.local_entries.iter().rev())
                    .cloned()
                    .collect::<Vec<_>>();
                // Stable, so entries of the same millisecond stay newest first.
                rows.sort_by_key(|row| std::cmp::Reverse(row.unix_ms));
                rows.truncate(MAX_ROWS);
                rows
            }
            NotificationLogView::Bookmarks => self.bookmark_rows(),
            view => self.agent_rows(view),
        }
    }

    pub(super) fn bookmark_count(&self) -> usize {
        self.snapshot.as_deref().map_or(0, |snapshot| {
            snapshot.tabs.iter().filter(|tab| tab.bookmarked).count()
                + snapshot
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.bookmarked)
                    .count()
        })
    }

    /// Bookmarked spaces, then bookmarked tabs, as records, in the order of
    /// the spaces and then the tabs inside each space: computed from the
    /// snapshot, so a moved, renamed or reordered tab or space shows right
    /// away.
    fn bookmark_rows(&self) -> Vec<NotificationRecord> {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return Vec::new();
        };
        let spaces = snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.bookmarked)
            .map(|workspace| NotificationRecord {
                id: 0,
                unix_ms: 0,
                kind: "bookmark-space".into(),
                title: workspace.label.clone(),
                body: None,
                agent: None,
                workspace_id: Some(workspace.workspace_id.clone()),
                tab_id: None,
                pane_id: None,
                task: None,
                request: None,
                repeats: None,
            });
        let tabs = snapshot
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
                // The label of the tab's sidebar line (its task, or the name it
                // was given), not the tab number.
                title: super::render::tabs::sidebar_tab_label(tab, snapshot, &self.config),
                body: None,
                agent: None,
                workspace_id: Some(tab.workspace_id.clone()),
                tab_id: Some(tab.tab_id.clone()),
                pane_id: None,
                task: None,
                request: None,
                repeats: None,
            });
        spaces.chain(tabs).collect()
    }

    /// The icon and colour for each row of the open list. A history row shows
    /// what happened then (finished, or asked), drawn like the sidebar draws
    /// that state, not the tab's state now: a tab that resumed work is the
    /// working list's. Other views show the tab's state now, for the rows
    /// whose tab still exists. Rows without one keep their mark in the text.
    pub(super) fn notification_row_icons(
        &self,
        rows: &[NotificationRecord],
    ) -> Vec<Option<(&'static str, ratatui::style::Color)>> {
        if self.notification_log_view() == NotificationLogView::History {
            use crate::api::schema::AgentStatus;
            let style = self.config.status_indicators;
            let palette = &self.config.palette;
            return rows
                .iter()
                .map(|row| {
                    let mark = match row.kind.as_str() {
                        "finished" => super::AgentMark::None,
                        "needs_attention" | "asking" => super::AgentMark::AwaitsReply,
                        _ => return None,
                    };
                    Some((
                        super::agent_icon(AgentStatus::Done, mark, style),
                        super::agent_color(AgentStatus::Done, mark, palette),
                    ))
                })
                .collect();
        }
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
        let Some(highlighted) = self.notification_log_highlighted(&self.notification_log_rows())
        else {
            return;
        };
        self.remove_bookmark_row(highlighted, outcome);
        // The row stays until the snapshot drops it; a second `x` must not
        // remove it again or move on to the next bookmark.
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            log.highlighted = None;
        }
    }

    /// The highlighted row's index in `rows`, or none when no row is
    /// highlighted or the highlighted one is gone.
    pub(super) fn notification_log_highlighted(
        &self,
        rows: &[NotificationRecord],
    ) -> Option<usize> {
        let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_ref() else {
            return None;
        };
        let key = log.highlighted.as_ref()?;
        rows.iter()
            .position(|row| RowKey::of(log.view, row).as_ref() == Some(key))
    }

    /// Highlights the row at `index` of the open list.
    pub(super) fn highlight_notification_log_row(&mut self, index: usize) {
        let key = self
            .notification_log_rows()
            .get(index)
            .and_then(|row| RowKey::of(self.notification_log_view(), row));
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            log.highlighted = key;
        }
    }

    pub(super) fn remove_bookmark_row(&mut self, index: usize, outcome: &mut ClientShellInput) {
        let Some(row) = self.bookmark_rows().into_iter().nth(index) else {
            return;
        };
        match (row.tab_id, row.workspace_id) {
            (Some(tab_id), _) => self.remove_bookmark(tab_id, outcome),
            (None, Some(workspace_id)) => {
                self.push_endpoint_method(
                    crate::api::schema::Method::WorkspaceBookmark(
                        crate::api::schema::WorkspaceBookmarkParams {
                            workspace_id,
                            bookmarked: false,
                        },
                    ),
                    outcome,
                );
                outcome.repaint = true;
            }
            (None, None) => {}
        }
    }

    /// Opens the row's tab menu over the list, at the pointer; nothing for
    /// a row whose tab is gone (a past notification).
    pub(super) fn open_list_row_menu(&mut self, index: usize, x: u16, y: u16) -> bool {
        let Some(row) = self.notification_log_rows().into_iter().nth(index) else {
            return false;
        };
        // A bookmarked space's row (no tab) gets the space's menu.
        let target = match (row.tab_id, row.workspace_id, row.kind.as_str()) {
            (Some(tab_id), _, _) => ListRowTarget::Tab(tab_id),
            (None, Some(workspace_id), "bookmark-space") => ListRowTarget::Space(workspace_id),
            _ => return false,
        };
        let menu = ListRowMenu {
            target,
            x,
            y,
            highlighted: 0,
        };
        if self.list_row_menu_overlay(&menu).is_none() {
            return false;
        }
        self.highlight_notification_log_row(index);
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            log.menu = Some(menu);
        }
        true
    }

    fn list_row_menu_overlay(&self, menu: &ListRowMenu) -> Option<ClientContextMenuOverlay> {
        let mut overlay = match &menu.target {
            ListRowTarget::Tab(tab_id) => {
                self.tab_context_menu(tab_id.clone(), menu.x, menu.y, true)?
            }
            ListRowTarget::Space(workspace_id) => {
                self.workspace_context_menu(workspace_id.clone(), menu.x, menu.y)?
            }
        };
        overlay.highlighted = menu.highlighted;
        Some(overlay)
    }

    /// The open row menu as a tab menu; none when its tab is gone.
    pub(super) fn list_row_menu(&self) -> Option<ClientContextMenuOverlay> {
        let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_ref() else {
            return None;
        };
        self.list_row_menu_overlay(log.menu.as_ref()?)
    }

    /// The open row menu, closing it.
    pub(super) fn take_list_row_menu(&mut self) -> Option<ClientContextMenuOverlay> {
        let menu = self.list_row_menu();
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            log.menu = None;
        }
        menu
    }

    /// Highlights an item of the open row menu.
    pub(super) fn highlight_list_row_menu_item(&mut self, index: usize) {
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            if let Some(menu) = log.menu.as_mut() {
                menu.highlighted = index;
            }
        }
    }

    /// Moves the open row menu's highlight by `delta` items.
    pub(super) fn move_list_row_menu_selection(&mut self, delta: isize) {
        let count = self.list_row_menu().map_or(0, |menu| menu.items().len());
        if let Some(ClientShellOverlay::NotificationLog(log)) = self.overlay.as_mut() {
            if let Some(menu) = log.menu.as_mut().filter(|_| count > 0) {
                menu.highlighted =
                    (menu.highlighted as isize + delta).clamp(0, count as isize - 1) as usize;
            }
        }
    }

    /// Runs an item of a row menu taken from the list. A dialog it opens
    /// (rename, a close confirmation) takes the list's place.
    pub(super) fn activate_list_row_menu_item(
        &mut self,
        menu: ClientContextMenuOverlay,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(action) = menu.items().get(index).map(|item| item.action) else {
            return;
        };
        match menu.target {
            ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id,
                ..
            } => self.activate_tab_context_action(tab_id, workspace_id, action, outcome),
            ClientContextMenuTarget::Workspace {
                workspace_id,
                close_group,
                ..
            } => self.activate_workspace_context_action(workspace_id, close_group, action, outcome),
            _ => {}
        }
        outcome.repaint = true;
    }

    pub(super) fn remove_bookmark(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
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

    /// Agents that wait on the user (blocked on an approval, awaiting a
    /// reply, or stopped by a limit) and agents working, one pane in at most
    /// one of them: attention wins. Counted from the snapshot the client
    /// already has.
    pub(super) fn agent_is_asking(agent: &crate::protocol::ClientShellAgent) -> bool {
        agent.agent_status == crate::api::schema::AgentStatus::Blocked
            || agent.awaiting_reply
            || agent.limited.is_some()
    }

    /// Working as the sidebar shows it: the agent works, or it waits on a
    /// job it started (the purple icon), unless it asks for the user.
    fn agent_is_working(
        snapshot: &crate::protocol::ClientShellSnapshot,
        agent: &crate::protocol::ClientShellAgent,
    ) -> bool {
        !Self::agent_is_asking(agent)
            && (agent.agent_status == crate::api::schema::AgentStatus::Working
                || super::agent_mark(snapshot, agent).waits_on_a_job())
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
        let mut agents = snapshot
            .agents
            .iter()
            .filter(|agent| match view {
                NotificationLogView::Asking => Self::agent_is_asking(agent),
                _ => Self::agent_is_working(snapshot, agent),
            })
            .collect::<Vec<_>>();
        // The agent that has waited longest first; one with no known start
        // (an older server) after them, in the sidebar's order.
        if view == NotificationLogView::Asking {
            agents.sort_by_key(|agent| agent.waiting_since_ms.unwrap_or(u64::MAX));
        }
        agents
            .into_iter()
            .take(MAX_ROWS)
            .map(|agent| NotificationRecord {
                id: 0,
                // An asking row's time is when the agent started waiting.
                unix_ms: if view == NotificationLogView::Asking {
                    agent.waiting_since_ms.unwrap_or(0)
                } else {
                    0
                },
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
                    Some(Self::asking_detail(agent))
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
                    .task
                    .as_deref()
                    .or(agent.terminal_title_stripped.as_deref())
                    .or(agent.terminal_title.as_deref())
                    .and_then(super::notification_policy::notification_detail_text),
                request: None,
                repeats: None,
            })
            .collect()
    }

    /// What an agent waiting on the user asks, for the second line of its
    /// row: its question, the approval it waits for, or the limit that
    /// stopped it and when that resets (`limited · resets 14:32`).
    pub(super) fn asking_detail(agent: &crate::protocol::ClientShellAgent) -> String {
        use crate::api::schema::AgentStatus;
        if agent.agent_status == AgentStatus::Blocked {
            return agent
                .question
                .clone()
                .unwrap_or_else(|| "approval".to_owned());
        }
        if agent.awaiting_reply {
            return agent.question.clone().unwrap_or_else(|| "reply".to_owned());
        }
        let Some(limit) = agent.limited.as_ref() else {
            return "reply".to_owned();
        };
        limit_detail(
            limit,
            crate::usage::now_unix(),
            super::usage::local_utc_offset_secs(),
        )
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
            "bookmark-space" => return format!("▤ {}", entry.title),
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
            // The space only when it says something the label does not.
            if let Some(workspace) = workspace.filter(|workspace| *workspace != entry.title) {
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
        // A working row says what it waits for (a job); an asking row says
        // it on a second line of its own (`notification_row_detail`).
        if entry.kind == "working" {
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

    /// The second line of an asking row: what the agent asks.
    pub(super) fn notification_row_detail(entry: &NotificationRecord) -> Option<String> {
        (entry.kind == "asking")
            .then(|| entry.body.clone())
            .flatten()
    }

    /// Which rows are unread: the first (newest) row of each tab that still
    /// has notifications not seen. Older rows of that tab were there before,
    /// so marking them too would show more marks than the button counts.
    pub(super) fn notification_unread_rows(&self, rows: &[NotificationRecord]) -> Vec<bool> {
        let mut marked = std::collections::HashSet::new();
        rows.iter()
            .map(|entry| {
                self.notification_log.unread_local.contains(&entry.id)
                    || entry.tab_id.as_deref().is_some_and(|tab_id| {
                        self.notification_log.unread_tabs.contains_key(tab_id)
                            && marked.insert(tab_id)
                    })
            })
            .collect()
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
                highlighted: None,
                view,
                menu: None,
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

    /// Moves the highlight by `delta` rows; with none, down starts at the
    /// first row and up at the last.
    pub(super) fn move_notification_log_selection(&mut self, delta: isize) {
        let rows = self.notification_log_rows();
        if rows.is_empty() {
            return;
        }
        let last = rows.len() - 1;
        let index = match self.notification_log_highlighted(&rows) {
            Some(index) => (index as isize + delta).clamp(0, last as isize) as usize,
            None if delta < 0 => last,
            None => 0,
        };
        self.highlight_notification_log_row(index);
    }

    /// A jump from a list, or a new tab, lands in a collapsed space (or one
    /// under a collapsed worktree group): open them, so the target shows, and
    /// scroll to it. The choice is kept, like a manual toggle.
    pub(super) fn expand_for_jump(
        &mut self,
        workspace_id: Option<&str>,
        outcome: &mut ClientShellInput,
    ) {
        let Some(workspace_id) = workspace_id else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        let mut keys = vec![super::space_tabs::tabs_collapse_key(workspace_id)];
        let starts_folded = super::space_tabs::starts_folded(workspace);
        if let Some(worktree) = workspace.worktree.as_ref() {
            // The group's parent space holds the group's collapse key.
            for (index, candidate) in snapshot.workspaces.iter().enumerate() {
                let is_parent = candidate
                    .worktree
                    .as_ref()
                    .is_some_and(|other| other.key == worktree.key && !other.is_linked_worktree);
                if is_parent {
                    keys.extend(super::sidebar::parent_group_key(snapshot, index));
                }
            }
        }
        let endpoint = self.active_endpoint_id.clone();
        let mut changed = starts_folded && self.unfold_space_tabs(workspace_id);
        for key in keys {
            changed |= self.expand_collapsed_group(&endpoint, &key);
        }
        if changed {
            self.reveal_focused_workspace = true;
            self.persist_chrome_preferences(outcome);
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
        self.notification_log.unread_local.remove(&entry.id);
        self.expand_for_jump(entry.workspace_id.as_deref(), outcome);
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

/// `HH:MM` in the local time offset `utc_offset_secs`; the day is said once
/// per group by `notification_day`.
pub(super) fn notification_time(unix_ms: u64, utc_offset_secs: i64) -> String {
    let at = local_datetime((unix_ms / 1000) as i64, utc_offset_secs);
    format!("{:02}:{:02}", at.hour(), at.minute())
}

/// The day group of a notification: `Today`, `Oct 2` for other days of this
/// year, `Oct 2 2025` for other years, in the local time offset
/// `utc_offset_secs`.
pub(super) fn notification_day(unix_ms: u64, now_unix: u64, utc_offset_secs: i64) -> String {
    let at = local_datetime((unix_ms / 1000) as i64, utc_offset_secs);
    let now = local_datetime(now_unix as i64, utc_offset_secs);
    let month = at.month().to_string();
    let month = month.get(..3).unwrap_or(&month);
    if at.date() == now.date() {
        "Today".to_owned()
    } else if at.year() == now.year() {
        format!("{month} {}", at.day())
    } else {
        format!("{month} {} {}", at.day(), at.year())
    }
}

/// What stopped an agent and when it can go on: `limited · resets 14:32`,
/// `limited · resets Oct 7 09:00` on another day, `limited · reset 14:32,
/// resume it` once passed, `out of credits`. Without a known reset time the
/// agent's own message follows instead.
pub(super) fn limit_detail(
    limit: &crate::api::schema::AgentLimit,
    now_unix: u64,
    utc_offset_secs: i64,
) -> String {
    use crate::api::schema::AgentLimitKind;
    let mut text = match limit.kind {
        AgentLimitKind::Credits => "out of credits".to_owned(),
        AgentLimitKind::Usage | AgentLimitKind::Unknown => "limited".to_owned(),
    };
    if let Some(resets_at) = limit.resets_at {
        let millis = resets_at.saturating_mul(1000);
        let time = notification_time(millis, utc_offset_secs);
        let day = notification_day(millis, now_unix, utc_offset_secs);
        if resets_at <= now_unix {
            text.push_str(&format!(" · reset {time}, resume it"));
        } else if day == "Today" {
            text.push_str(&format!(" · resets {time}"));
        } else {
            text.push_str(&format!(" · resets {day} {time}"));
        }
    } else if let Some(message) = limit.message.as_deref() {
        text.push_str(&format!(" · {message}"));
    }
    text
}

/// How long an agent has waited, for the time column of its row: `now`,
/// `4m`, `2h`, `3d`.
pub(super) fn wait_duration(since_ms: u64, now_unix: u64) -> String {
    let secs = now_unix.saturating_sub(since_ms / 1000);
    match secs {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn local_datetime(secs: i64, utc_offset_secs: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(secs.saturating_add(utc_offset_secs))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_the_clock_and_days_group_them() {
        // 2026-09-28 22:05:00 UTC.
        let at = 1_790_633_100_000;
        assert_eq!(notification_time(at, 0), "22:05");
        assert_eq!(notification_time(at, 7200), "00:05");
        assert_eq!(notification_day(at, 1_790_633_200, 0), "Today");
        // Past local midnight the same instant is yesterday's group.
        assert_eq!(notification_day(at, 1_790_640_000, 0), "Sep 28");
        assert_eq!(notification_day(at, 1_790_633_200, 7200), "Today");
        // A clock ahead of `now` still gets its own day, never "Today".
        assert_eq!(notification_day(at, 1_790_550_000, 0), "Sep 28");
        // 2027-01-01 00:10 UTC: another year says the year.
        assert_eq!(notification_day(at, 1_798_762_200, 0), "Sep 28 2026");
    }
}
