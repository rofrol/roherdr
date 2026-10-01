//! Sorting the spaces list from its header: `cust` keeps the manual order the
//! server stores (the only mode that allows dragging), `name` sorts by label
//! and `prio` by the most urgent agent. Clicking the active button flips its
//! direction. Presentation only: the choice is a client preference and never
//! changes the server's order, so another client keeps its own view.

use std::collections::HashSet;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use serde::{Deserialize, Serialize};

use super::*;
use crate::protocol::ClientShellSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SpaceSortKey {
    #[default]
    Custom,
    Name,
    Priority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct SpaceSort {
    pub(super) key: SpaceSortKey,
    /// Z to A instead of A to Z.
    pub(super) name_descending: bool,
    /// Most urgent first; false puts idle spaces first.
    pub(super) priority_descending: bool,
}

impl Default for SpaceSort {
    fn default() -> Self {
        Self {
            key: SpaceSortKey::Custom,
            name_descending: false,
            priority_descending: true,
        }
    }
}

impl SpaceSort {
    /// The sort after clicking `key`'s button: the active button flips its
    /// direction, another one becomes active with the direction it had.
    pub(super) fn clicked(self, key: SpaceSortKey) -> Self {
        let mut next = self;
        if self.key != key {
            next.key = key;
            return next;
        }
        match key {
            SpaceSortKey::Custom => {}
            SpaceSortKey::Name => next.name_descending = !next.name_descending,
            SpaceSortKey::Priority => next.priority_descending = !next.priority_descending,
        }
        next
    }

    pub(super) fn allows_drag(self) -> bool {
        self.key == SpaceSortKey::Custom
    }

    fn buttons(self) -> [(SpaceSortKey, String); 3] {
        let arrow = |descending: bool| if descending { "↓" } else { "↑" };
        [
            (SpaceSortKey::Custom, "manual".to_owned()),
            (
                SpaceSortKey::Name,
                format!("name {}", arrow(self.name_descending)),
            ),
            (
                SpaceSortKey::Priority,
                format!("prio {}", arrow(self.priority_descending)),
            ),
        ]
    }
}

/// Draws the buttons in place of the `spaces` title and returns their rects.
pub(super) fn render_sort_header(
    buffer: &mut Buffer,
    area: Rect,
    sort: SpaceSort,
    palette: &Palette,
) -> Vec<(Rect, SpaceSortKey)> {
    let mut hits = Vec::new();
    let mut x = area.x.saturating_add(1);
    for (key, label) in sort.buttons() {
        let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
        if x.saturating_add(width) > area.right() {
            break;
        }
        let style = if key == sort.key {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        };
        super::render::put_text(buffer, x, area.y, width, &label, style);
        hits.push((Rect::new(x, area.y, width, 1), key));
        // One column apart: `manual name ↑ prio ↓` leaves room for the
        // notification button at the right.
        x = x.saturating_add(width + 1);
    }
    hits
}

/// `entries` in the order `held` gives their spaces (root ids, as last
/// drawn), so a sorted list does not move while the pointer is over it;
/// spaces `held` does not know come last, in their own order. A space moves
/// with its indented worktrees.
pub(super) fn held_entries(
    snapshot: &ClientShellSnapshot,
    entries: Vec<WorkspaceEntry>,
    held: &[String],
) -> Vec<WorkspaceEntry> {
    let mut families: Vec<Vec<WorkspaceEntry>> = Vec::new();
    for entry in entries {
        match families.last_mut() {
            Some(family) if entry.indented => family.push(entry),
            _ => families.push(vec![entry]),
        }
    }
    let rank = |family: &[WorkspaceEntry]| {
        snapshot
            .workspaces
            .get(family[0].index)
            .and_then(|workspace| held.iter().position(|id| *id == workspace.workspace_id))
            .unwrap_or(usize::MAX)
    };
    // Stable, so spaces `held` does not know keep their sorted order.
    families.sort_by_key(|family| rank(family));
    families.into_iter().flatten().collect()
}

/// The root space ids of `entries`, in order: what [`held_entries`] holds.
pub(super) fn root_ids(snapshot: &ClientShellSnapshot, entries: &[WorkspaceEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| !entry.indented)
        .filter_map(|entry| snapshot.workspaces.get(entry.index))
        .map(|workspace| workspace.workspace_id.clone())
        .collect()
}

/// `entries` in `sort`'s order. A space moves together with its indented
/// worktrees; ties keep the manual order.
pub(super) fn sorted_entries(
    snapshot: &ClientShellSnapshot,
    entries: Vec<WorkspaceEntry>,
    collapsed_groups: &HashSet<String>,
    sort: SpaceSort,
) -> Vec<WorkspaceEntry> {
    if sort.key == SpaceSortKey::Custom {
        return entries;
    }
    let mut families: Vec<Vec<WorkspaceEntry>> = Vec::new();
    for entry in entries {
        match families.last_mut() {
            Some(family) if entry.indented => family.push(entry),
            _ => families.push(vec![entry]),
        }
    }
    let name = |family: &[WorkspaceEntry]| {
        snapshot
            .workspaces
            .get(family[0].index)
            .map(|workspace| workspace.label.to_lowercase())
            .unwrap_or_default()
    };
    let urgency = |family: &[WorkspaceEntry]| {
        family
            .iter()
            .filter_map(|entry| snapshot.workspaces.get(entry.index))
            .map(|workspace| {
                status_priority(super::sidebar::displayed_workspace_status(
                    snapshot,
                    workspace,
                    collapsed_groups,
                ))
            })
            .max()
            .unwrap_or(0)
    };
    let (mut keyed, descending): (Vec<_>, bool) = match sort.key {
        SpaceSortKey::Custom => return families.into_iter().flatten().collect(),
        SpaceSortKey::Name => (
            families
                .into_iter()
                .map(|family| ((name(&family), 0), family))
                .collect(),
            sort.name_descending,
        ),
        SpaceSortKey::Priority => (
            families
                .into_iter()
                .map(|family| ((String::new(), urgency(&family)), family))
                .collect(),
            sort.priority_descending,
        ),
    };
    // A stable sort, so equal keys keep the manual order either way.
    keyed.sort_by(|(left, _), (right, _)| {
        if descending {
            right.cmp(left)
        } else {
            left.cmp(right)
        }
    });
    keyed.into_iter().flat_map(|(_, family)| family).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;

    fn snapshot_with(spaces: &[(&str, AgentStatus)]) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        let template = snapshot.workspaces[0].clone();
        snapshot.workspaces = spaces
            .iter()
            .enumerate()
            .map(|(index, (label, status))| {
                let mut workspace = template.clone();
                workspace.workspace_id = format!("ws_{index}");
                workspace.label = (*label).to_owned();
                workspace.agent_status = *status;
                workspace.worktree = None;
                workspace
            })
            .collect();
        snapshot
    }

    fn labels(snapshot: &ClientShellSnapshot, sort: SpaceSort) -> Vec<String> {
        let entries = super::super::sidebar::workspace_entries(snapshot, &HashSet::new());
        sorted_entries(snapshot, entries, &HashSet::new(), sort)
            .iter()
            .map(|entry| snapshot.workspaces[entry.index].label.clone())
            .collect()
    }

    #[test]
    fn sorts_by_name_and_by_urgency_keeping_manual_order_on_ties() {
        let snapshot = snapshot_with(&[
            ("beta", AgentStatus::Idle),
            ("Alpha", AgentStatus::Working),
            ("gamma", AgentStatus::Blocked),
            ("delta", AgentStatus::Idle),
        ]);
        let custom = SpaceSort::default();
        assert_eq!(
            labels(&snapshot, custom),
            ["beta", "Alpha", "gamma", "delta"]
        );
        let name = custom.clicked(SpaceSortKey::Name);
        assert_eq!(labels(&snapshot, name), ["Alpha", "beta", "delta", "gamma"]);
        let name_down = name.clicked(SpaceSortKey::Name);
        assert_eq!(
            labels(&snapshot, name_down),
            ["gamma", "delta", "beta", "Alpha"]
        );
        let prio = name_down.clicked(SpaceSortKey::Priority);
        assert_eq!(labels(&snapshot, prio), ["gamma", "Alpha", "beta", "delta"]);
        let prio_up = prio.clicked(SpaceSortKey::Priority);
        assert_eq!(
            labels(&snapshot, prio_up),
            ["beta", "delta", "Alpha", "gamma"]
        );
    }

    #[test]
    fn switching_buttons_keeps_each_direction_and_only_custom_drags() {
        let sort = SpaceSort::default()
            .clicked(SpaceSortKey::Name)
            .clicked(SpaceSortKey::Name)
            .clicked(SpaceSortKey::Custom);
        assert!(sort.allows_drag());
        let back = sort.clicked(SpaceSortKey::Name);
        assert!(back.name_descending);
        assert!(!back.allows_drag());
        assert_eq!(sort.clicked(SpaceSortKey::Custom), sort);
    }
}
