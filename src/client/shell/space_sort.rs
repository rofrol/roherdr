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

    /// The current key and direction: `manual`, `name ↑`, `prio ↓`.
    pub(super) fn current_label(self) -> String {
        self.buttons()
            .into_iter()
            .find(|(key, _)| *key == self.key)
            .map(|(_, label)| label)
            .unwrap_or_default()
    }

    /// The menu rows: every key with its direction; the active one is marked
    /// and clicking it flips its direction.
    pub(super) fn menu_items(self) -> Vec<(SpaceSortKey, String)> {
        self.buttons()
            .into_iter()
            .map(|(key, label)| {
                let mark = if key == self.key { "● " } else { "  " };
                (key, format!("{mark}{label}"))
            })
            .collect()
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

/// Draws the one sort button that opens the choice (see
/// [`SpaceSort::menu_items`]): `⇅ name ↑`, the current key and direction. It
/// stays short so the indicators fit beside it at the default width.
pub(super) fn render_sort_header(
    buffer: &mut Buffer,
    area: Rect,
    sort: SpaceSort,
    palette: &Palette,
) -> Vec<(Rect, SpaceSortKey)> {
    let label = format!("⇅ {}", sort.current_label());
    let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
    let x = area.x.saturating_add(1);
    if x.saturating_add(width) > area.right() {
        return Vec::new();
    }
    let style = Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    super::render::put_text(buffer, x, area.y, width, &label, style);
    vec![(Rect::new(x, area.y, width, 1), sort.key)]
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

/// What the last `⤒` click moved: the space, the server's space order before
/// it and the order it asked for. While the order is still the one asked
/// for, that space's button offers to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpaceToTopUndo {
    pub(super) workspace_id: String,
    pub(super) before: Vec<String>,
    pub(super) after: Vec<String>,
}

/// The `workspace.move_block` that puts a space first in the manual order,
/// with its worktree group: the ids to move, the space they go before, and
/// the order that leaves. None when it is first already.
pub(super) fn move_space_to_top(
    snapshot: &ClientShellSnapshot,
    workspace_id: &str,
) -> Option<(Vec<String>, Option<String>, Vec<String>)> {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)?;
    let key = workspace.worktree.as_ref().map(|worktree| &worktree.key);
    let in_family = |other: &ClientShellWorkspace| {
        other.workspace_id == workspace_id
            || key.is_some() && other.worktree.as_ref().map(|worktree| &worktree.key) == key
    };
    let order = snapshot
        .workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.clone())
        .collect::<Vec<_>>();
    let moved = snapshot
        .workspaces
        .iter()
        .filter(|other| in_family(other))
        .map(|other| other.workspace_id.clone())
        .collect::<Vec<_>>();
    let rest = snapshot
        .workspaces
        .iter()
        .filter(|other| !in_family(other))
        .map(|other| other.workspace_id.clone())
        .collect::<Vec<_>>();
    let after = moved.iter().chain(&rest).cloned().collect::<Vec<_>>();
    if after == order {
        return None;
    }
    Some((moved, rest.first().cloned(), after))
}

impl ClientShellState {
    /// The space whose last move to the top can still be put back: nothing
    /// reordered the spaces since.
    pub(super) fn space_to_top_undo(&self) -> Option<&str> {
        let undo = self.space_to_top_undo.as_ref()?;
        (self.active_endpoint_id.is_local()
            && self.snapshot.as_deref().is_some_and(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.workspace_id.as_str())
                    .eq(undo.after.iter().map(String::as_str))
            }))
        .then_some(undo.workspace_id.as_str())
    }

    /// A space's `⤒`: moves it to the top of the manual order, for every
    /// client, or puts the last such move back.
    pub(super) fn click_space_to_top(
        &mut self,
        workspace_id: String,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{Method, WorkspaceMoveBlockParams};
        outcome.repaint = true;
        if self.space_to_top_undo() == Some(workspace_id.as_str()) {
            if let Some(undo) = self.space_to_top_undo.take() {
                self.push_endpoint_method(
                    Method::WorkspaceMoveBlock(WorkspaceMoveBlockParams {
                        workspace_ids: undo.before,
                        before_workspace_id: None,
                    }),
                    outcome,
                );
            }
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let before = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect::<Vec<_>>();
        let Some((workspace_ids, before_workspace_id, after)) =
            move_space_to_top(snapshot, &workspace_id)
        else {
            return;
        };
        if self.push_endpoint_method_with_kind(
            Method::WorkspaceMoveBlock(WorkspaceMoveBlockParams {
                workspace_ids,
                before_workspace_id,
            }),
            PendingEndpointKind::Generic,
            outcome,
        ) {
            self.space_to_top_undo = Some(SpaceToTopUndo {
                workspace_id,
                before,
                after,
            });
        }
        self.workspace_scroll = 0;
    }
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

    #[test]
    fn a_space_moves_to_the_top_with_its_worktree_group() {
        use crate::protocol::ClientShellWorktree;
        let mut snapshot = snapshot_with(&[
            ("a", AgentStatus::Idle),
            ("parent", AgentStatus::Idle),
            ("b", AgentStatus::Idle),
            ("child", AgentStatus::Idle),
        ]);
        let worktree = |linked| ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: linked,
        };
        snapshot.workspaces[1].worktree = Some(worktree(false));
        snapshot.workspaces[3].worktree = Some(worktree(true));
        let (moved, before, after) = move_space_to_top(&snapshot, "ws_1").expect("a move");
        assert_eq!(moved, ["ws_1", "ws_3"]);
        assert_eq!(before.as_deref(), Some("ws_0"));
        assert_eq!(after, ["ws_1", "ws_3", "ws_0", "ws_2"]);
        assert_eq!(move_space_to_top(&snapshot, "ws_0"), None, "first already");
    }

    #[test]
    fn the_to_top_button_moves_a_space_up_and_can_put_it_back() {
        use crate::api::schema::Method;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        let snapshot = snapshot_with(&[("a", AgentStatus::Idle), ("b", AgentStatus::Idle)]);
        state.set_snapshot(Box::new(snapshot.clone()));
        let moves = |outcome: &ClientShellInput| {
            outcome
                .actions
                .iter()
                .filter_map(|action| match action {
                    ClientShellAction::Endpoint { request, .. } => match &request.method {
                        Method::WorkspaceMoveBlock(params) => Some(params.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut outcome = ClientShellInput::default();
        state.click_space_to_top("ws_1".into(), &mut outcome);
        let sent = moves(&outcome);
        assert_eq!(sent[0].workspace_ids, ["ws_1"]);
        assert_eq!(sent[0].before_workspace_id.as_deref(), Some("ws_0"));
        assert_eq!(state.space_to_top_undo(), None, "not moved yet");

        // The server moved it: its button puts the old order back.
        let mut moved = snapshot;
        moved.workspaces.swap(0, 1);
        state.set_snapshot(Box::new(moved));
        assert_eq!(state.space_to_top_undo(), Some("ws_1"));
        let mut outcome = ClientShellInput::default();
        state.click_space_to_top("ws_1".into(), &mut outcome);
        let sent = moves(&outcome);
        assert_eq!(sent[0].workspace_ids, ["ws_0", "ws_1"]);
        assert_eq!(sent[0].before_workspace_id, None);
        assert!(state.space_to_top_undo.is_none());
    }
}
