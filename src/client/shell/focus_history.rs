//! Back and forward over focus jumps, like a browser's: the header's `‹ ›`
//! buttons. Each client keeps its own history per machine, in memory only,
//! since each client has its own focus. An entry is a tab, with the pane
//! focused in it last; moving between panes of one tab updates that entry.

use crate::protocol::ClientShellSnapshot;

/// Entries kept; the oldest go first.
const DEPTH: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FocusEntry {
    pub(super) tab_id: String,
    pub(super) pane_id: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct FocusHistory {
    entries: Vec<FocusEntry>,
    /// The entry the focus is on.
    cursor: usize,
    /// Back or forward steps sent to the server and not yet seen in a
    /// snapshot: the tabs passed on the way (the one left first, then each
    /// earlier step's target) and the last step's target. Snapshots that
    /// show a tab on the way are not a jump.
    pending: Option<(Vec<String>, String)>,
    /// The tab the last snapshot showed, where a refused step goes back to.
    shown: Option<String>,
}

impl FocusHistory {
    pub(super) fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub(super) fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Takes in the focus a snapshot shows. A new tab is a jump: it drops the
    /// entries ahead and becomes the last one. The focus that replaces a tab
    /// that closed takes its entry instead, since nobody jumped there.
    pub(super) fn observe(&mut self, snapshot: &ClientShellSnapshot) {
        // A snapshot without tabs (a server still starting) says nothing
        // about which tabs closed.
        let Some(tab_id) = snapshot
            .focused_tab_id
            .clone()
            .filter(|_| !snapshot.tabs.is_empty())
        else {
            return;
        };
        let live = |id: &str| snapshot.tabs.iter().any(|tab| tab.tab_id == id);
        self.shown = Some(tab_id.clone());
        if let Some((passed, to)) = self.pending.as_ref() {
            if *to != tab_id && passed.contains(&tab_id) {
                self.compact(live);
                return;
            }
            // Arrived, or something else moved the focus meanwhile.
            self.pending = None;
        }
        let entry = FocusEntry {
            tab_id: tab_id.clone(),
            pane_id: snapshot.focused_pane_id.clone(),
        };
        match self.entries.get_mut(self.cursor) {
            None => {
                self.entries = vec![entry];
                self.cursor = 0;
            }
            Some(current) if current.tab_id == tab_id || !live(&current.tab_id) => {
                *current = entry;
            }
            Some(_) => {
                self.entries.truncate(self.cursor + 1);
                self.entries.push(entry);
                if self.entries.len() > DEPTH {
                    self.entries.remove(0);
                }
                self.cursor = self.entries.len() - 1;
            }
        }
        self.compact(live);
    }

    /// Drops the entries of closed tabs and merges neighbours that name the
    /// same tab, keeping the cursor on its entry (always live: the focus).
    fn compact(&mut self, live: impl Fn(&str) -> bool) {
        let mut kept: Vec<FocusEntry> = Vec::with_capacity(self.entries.len());
        let mut cursor = 0;
        for (index, entry) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            let is_cursor = index == self.cursor;
            if !is_cursor && !live(&entry.tab_id) {
                continue;
            }
            match kept.last_mut() {
                Some(last) if last.tab_id == entry.tab_id => {
                    if is_cursor {
                        *last = entry;
                    }
                }
                _ => kept.push(entry),
            }
            if is_cursor {
                cursor = kept.len() - 1;
            }
        }
        self.entries = kept;
        self.cursor = cursor;
    }

    /// The server refused a step (its tab closed meanwhile, say): the cursor
    /// goes back to the tab the snapshots show.
    pub(super) fn step_refused(&mut self) {
        if self.pending.take().is_none() {
            return;
        }
        let Some(shown) = self.shown.as_ref() else {
            return;
        };
        if let Some(index) = (0..self.entries.len())
            .filter(|index| self.entries[*index].tab_id == *shown)
            .min_by_key(|index| index.abs_diff(self.cursor))
        {
            self.cursor = index;
        }
    }

    /// Moves one entry back (`-1`) or forward (`1`) and returns where the
    /// focus should go, or none at either end.
    pub(super) fn step(&mut self, direction: isize) -> Option<FocusEntry> {
        let target = self.cursor.checked_add_signed(direction)?;
        let entry = self.entries.get(target)?.clone();
        // A step before the last one arrived: snapshots may still show any
        // tab passed since the first.
        let passed = match self.pending.take() {
            Some((mut passed, to)) => {
                passed.push(to);
                passed
            }
            None => vec![self.entries.get(self.cursor)?.tab_id.clone()],
        };
        self.cursor = target;
        self.pending = Some((passed, entry.tab_id.clone()));
        Some(entry)
    }
}

impl super::ClientShellState {
    /// Records the focus of the snapshot just taken in.
    pub(super) fn observe_focus_history(&mut self) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        self.focus_history
            .entry(self.active_endpoint_id.clone())
            .or_default()
            .observe(snapshot);
    }

    pub(super) fn focus_history_state(&self) -> (bool, bool) {
        self.focus_history
            .get(&self.active_endpoint_id)
            .map_or((false, false), |history| {
                (history.can_go_back(), history.can_go_forward())
            })
    }

    /// The header's `‹` (`-1`) and `›` (`1`): focuses the tab one jump back
    /// or forward, on the pane last focused there while it is open.
    pub(super) fn step_focus_history(
        &mut self,
        direction: isize,
        outcome: &mut super::ClientShellInput,
    ) {
        let Some(entry) = self
            .focus_history
            .get_mut(&self.active_endpoint_id)
            .and_then(|history| history.step(direction))
        else {
            return;
        };
        let pane_open = self.snapshot.as_deref().is_some_and(|snapshot| {
            snapshot.panes.iter().any(|pane| {
                Some(&pane.pane_id) == entry.pane_id.as_ref() && pane.tab_id == entry.tab_id
            })
        });
        let method = match entry.pane_id.filter(|_| pane_open) {
            Some(pane_id) => {
                crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id })
            }
            None => crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                tab_id: entry.tab_id,
            }),
        };
        self.push_endpoint_method_with_kind(
            method,
            super::state::PendingEndpointKind::FocusStep {
                endpoint_id: self.active_endpoint_id.clone(),
            },
            outcome,
        );
    }

    pub(super) fn focus_step_refused(&mut self, endpoint_id: &super::ClientEndpointId) {
        if let Some(history) = self.focus_history.get_mut(endpoint_id) {
            history.step_refused();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(tabs: &[&str], focused: &str) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.tabs = tabs
            .iter()
            .map(|id| crate::protocol::ClientShellTab {
                activity: None,
                bookmarked: false,
                tab_id: (*id).into(),
                workspace_id: "w".into(),
                number: 1,
                label: (*id).into(),
                custom_label: false,
                zoomed: false,
                focused: *id == focused,
                agent_status: crate::api::schema::AgentStatus::Unknown,
                parent_tab_id: None,
                status: None,
                program: None,
                role: None,
            })
            .collect();
        snapshot.focused_tab_id = Some(focused.into());
        snapshot.focused_pane_id = Some(format!("{focused}-pane"));
        snapshot
    }

    fn visit(history: &mut FocusHistory, tabs: &[&str], focused: &str) {
        history.observe(&snapshot(tabs, focused));
    }

    fn back(history: &mut FocusHistory) -> Option<String> {
        history.step(-1).map(|entry| entry.tab_id)
    }

    #[test]
    fn back_and_forward_walk_the_jumps_and_a_new_jump_drops_the_ones_ahead() {
        let tabs = ["a", "b", "c", "d"];
        let mut history = FocusHistory::default();
        visit(&mut history, &tabs, "a");
        visit(&mut history, &tabs, "b");
        visit(&mut history, &tabs, "c");
        assert!(!history.can_go_forward());

        assert_eq!(back(&mut history).as_deref(), Some("b"));
        // A snapshot from before the step still shows `c`: not a jump.
        visit(&mut history, &tabs, "c");
        visit(&mut history, &tabs, "b");
        assert_eq!(back(&mut history).as_deref(), Some("a"));
        visit(&mut history, &tabs, "a");
        assert!(!history.can_go_back());
        assert_eq!(
            history.step(1).map(|entry| entry.tab_id).as_deref(),
            Some("b")
        );
        visit(&mut history, &tabs, "b");

        visit(&mut history, &tabs, "d");
        assert!(!history.can_go_forward());
        assert_eq!(back(&mut history).as_deref(), Some("b"));
    }

    #[test]
    fn two_quick_steps_back_land_two_entries_back() {
        let tabs = ["a", "b", "c"];
        let mut history = FocusHistory::default();
        visit(&mut history, &tabs, "a");
        visit(&mut history, &tabs, "b");
        visit(&mut history, &tabs, "c");
        assert_eq!(back(&mut history).as_deref(), Some("b"));
        assert_eq!(back(&mut history).as_deref(), Some("a"));
        // Snapshots still show `c`, then the first step, then the second.
        visit(&mut history, &tabs, "c");
        visit(&mut history, &tabs, "b");
        visit(&mut history, &tabs, "a");
        assert!(!history.can_go_back());
        assert!(history.can_go_forward());
    }

    #[test]
    fn a_refused_step_puts_the_cursor_back_on_the_tab_shown() {
        let tabs = ["a", "b", "c"];
        let mut history = FocusHistory::default();
        visit(&mut history, &tabs, "a");
        visit(&mut history, &tabs, "b");
        visit(&mut history, &tabs, "c");
        assert_eq!(back(&mut history).as_deref(), Some("b"));
        history.step_refused();
        assert!(!history.can_go_forward());
        visit(&mut history, &tabs, "c");
        assert_eq!(back(&mut history).as_deref(), Some("b"));
    }

    #[test]
    fn closed_tabs_leave_the_history_and_their_fallback_is_no_jump() {
        let mut history = FocusHistory::default();
        visit(&mut history, &["a", "b", "c"], "a");
        visit(&mut history, &["a", "b", "c"], "b");
        visit(&mut history, &["a", "b", "c"], "c");
        // `c` closes and the server focuses `a`: that takes `c`'s place.
        visit(&mut history, &["a", "b"], "a");
        assert_eq!(back(&mut history).as_deref(), Some("b"));
        visit(&mut history, &["a", "b"], "b");
        // `a` was entered twice in a row once `c` went: one entry.
        assert!(history.can_go_back());
        assert_eq!(back(&mut history).as_deref(), Some("a"));
        visit(&mut history, &["a", "b"], "a");
        assert!(!history.can_go_back());
    }

    #[test]
    fn a_pane_move_inside_a_tab_updates_its_entry() {
        let mut history = FocusHistory::default();
        visit(&mut history, &["a", "b"], "a");
        visit(&mut history, &["a", "b"], "b");
        let mut moved = snapshot(&["a", "b"], "b");
        moved.focused_pane_id = Some("b-other".into());
        history.observe(&moved);
        visit(&mut history, &["a", "b"], "a");
        let entry = history.step(-1).expect("one jump back");
        assert_eq!(entry.pane_id.as_deref(), Some("b-other"));
    }

    #[test]
    fn the_history_keeps_its_depth() {
        let tabs = (0..DEPTH + 5).map(|n| n.to_string()).collect::<Vec<_>>();
        let tabs = tabs.iter().map(String::as_str).collect::<Vec<_>>();
        let mut history = FocusHistory::default();
        for tab in &tabs {
            visit(&mut history, &tabs, tab);
        }
        let mut steps = 0;
        while back(&mut history).is_some() {
            steps += 1;
        }
        assert_eq!(steps, DEPTH - 1);
    }
}
