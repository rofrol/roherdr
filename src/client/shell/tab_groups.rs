//! Child tabs: a tab with `parent_tab_id` is shown in a second row under the
//! main tab bar while its parent (or a sibling) is the active tab. The server
//! keeps each parent's children right after it, so the flat tab order is the
//! order tabs are shown in.

use crate::api::schema::TabStatus;
use crate::protocol::{ClientShellSnapshot, ClientShellTab};

/// Tabs of the focused workspace shown in the main row.
pub(super) fn main_row_tabs(snapshot: &ClientShellSnapshot) -> Vec<&ClientShellTab> {
    focused_workspace_tabs(snapshot)
        .filter(|tab| tab.parent_tab_id.is_none())
        .collect()
}

/// The main-row tab of the active group: the focused tab, or its parent.
pub(super) fn active_main_tab_id(snapshot: &ClientShellSnapshot) -> Option<&str> {
    let focused = focused_workspace_tabs(snapshot).find(|tab| tab.focused)?;
    Some(
        focused
            .parent_tab_id
            .as_deref()
            .unwrap_or(focused.tab_id.as_str()),
    )
}

pub(super) fn child_tabs<'a>(
    snapshot: &'a ClientShellSnapshot,
    parent_tab_id: &str,
) -> Vec<&'a ClientShellTab> {
    snapshot
        .tabs
        .iter()
        .filter(|tab| tab.parent_tab_id.as_deref() == Some(parent_tab_id))
        .collect()
}

/// Whether the second row is shown: while any tab of the focused workspace
/// has children, whichever tab is active, so switching tabs does not resize
/// every pane by a row.
pub(super) fn workspace_has_child_tabs(snapshot: &ClientShellSnapshot) -> bool {
    focused_workspace_tabs(snapshot).any(|tab| tab.parent_tab_id.is_some())
}

/// Children of the active group, shown in the second row.
pub(super) fn active_child_tabs(snapshot: &ClientShellSnapshot) -> Vec<&ClientShellTab> {
    active_main_tab_id(snapshot)
        .map(|parent| child_tabs(snapshot, parent))
        .unwrap_or_default()
}

/// Maps an insert position among main-row tabs to one in the workspace's flat
/// tab list, which `tab.move` takes.
pub(super) fn flat_insert_index(snapshot: &ClientShellSnapshot, main_row_index: usize) -> usize {
    let tabs = focused_workspace_tabs(snapshot).collect::<Vec<_>>();
    tabs.iter()
        .enumerate()
        .filter(|(_, tab)| tab.parent_tab_id.is_none())
        .nth(main_row_index)
        .map_or(tabs.len(), |(index, _)| index)
}

/// Name of a parent's own entry in the child row: the agent running in it
/// (e.g. `claude`), else the tab's label.
pub(super) fn parent_entry_label(
    snapshot: &ClientShellSnapshot,
    parent: &ClientShellTab,
) -> String {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.tab_id == parent.tab_id)
        .find_map(|agent| {
            agent
                .display_agent
                .clone()
                .or_else(|| agent.agent.clone())
                .or_else(|| agent.name.clone())
        })
        .unwrap_or_else(|| parent.label.clone())
}

pub(super) fn status_icon(status: Option<TabStatus>) -> Option<&'static str> {
    match status? {
        TabStatus::Running => Some("⏳"),
        TabStatus::Succeeded => Some("✓"),
        // Not `✗`: next to a tab label it reads as a close button.
        TabStatus::Failed => Some("!"),
        TabStatus::Unknown => None,
    }
}

/// Counts of the children's statuses, e.g. `⏳1 !2 ✓3`; empty without children.
/// Children without a status are counted as `•N`.
pub(super) fn children_summary(children: &[&ClientShellTab]) -> String {
    let count = |wanted: Option<TabStatus>| {
        children
            .iter()
            .filter(|tab| status_icon(tab.status) == status_icon(wanted))
            .count()
    };
    [
        ("⏳", count(Some(TabStatus::Running))),
        ("!", count(Some(TabStatus::Failed))),
        ("✓", count(Some(TabStatus::Succeeded))),
        ("•", count(None)),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .map(|(icon, count)| format!("{icon}{count}"))
    .collect::<Vec<_>>()
    .join(" ")
}

fn focused_workspace_tabs(snapshot: &ClientShellSnapshot) -> impl Iterator<Item = &ClientShellTab> {
    snapshot
        .tabs
        .iter()
        .filter(|tab| Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(
        id: &str,
        parent: Option<&str>,
        status: Option<TabStatus>,
        focused: bool,
    ) -> ClientShellTab {
        ClientShellTab {
            tab_id: id.into(),
            workspace_id: "w".into(),
            number: 1,
            label: id.into(),
            custom_label: false,
            zoomed: false,
            focused,
            agent_status: crate::api::schema::AgentStatus::Unknown,
            parent_tab_id: parent.map(str::to_string),
            status,
        }
    }

    fn snapshot(tabs: Vec<ClientShellTab>) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.focused_workspace_id = Some("w".into());
        snapshot.tabs = tabs;
        snapshot
    }

    #[test]
    fn a_focused_child_keeps_its_parent_active_and_its_row_visible() {
        let snapshot = snapshot(vec![
            tab("a", None, None, false),
            tab("a1", Some("a"), Some(TabStatus::Running), true),
            tab("b", None, None, false),
        ]);

        assert_eq!(active_main_tab_id(&snapshot), Some("a"));
        let ids = |tabs: Vec<&ClientShellTab>| {
            tabs.into_iter()
                .map(|tab| tab.tab_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(main_row_tabs(&snapshot)), ["a", "b"]);
        assert_eq!(ids(active_child_tabs(&snapshot)), ["a1"]);
    }

    #[test]
    fn summary_counts_statuses_and_skips_zeroes() {
        let snapshot = snapshot(vec![
            tab("a", None, None, true),
            tab("a1", Some("a"), Some(TabStatus::Failed), false),
            tab("a2", Some("a"), Some(TabStatus::Succeeded), false),
            tab("a3", Some("a"), Some(TabStatus::Failed), false),
            tab("a4", Some("a"), Some(TabStatus::Unknown), false),
        ]);

        assert_eq!(children_summary(&child_tabs(&snapshot, "a")), "!2 ✓1 •1");
        assert_eq!(children_summary(&[]), "");
    }

    #[test]
    fn main_row_insert_positions_map_past_children() {
        let snapshot = snapshot(vec![
            tab("a", None, None, true),
            tab("a1", Some("a"), None, false),
            tab("b", None, None, false),
            tab("b1", Some("b"), None, false),
        ]);

        assert_eq!(flat_insert_index(&snapshot, 0), 0);
        assert_eq!(flat_insert_index(&snapshot, 1), 2);
        assert_eq!(flat_insert_index(&snapshot, 2), 4);
    }
}
