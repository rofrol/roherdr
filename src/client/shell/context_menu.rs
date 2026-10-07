use super::*;

/// Branch names longer than this are cut in the branch menu.
const BRANCH_NAME_MAX_WIDTH: usize = 32;

/// A branch's push state in the branch menu: `↑1 ↓4`, `synced`, `gone`,
/// `local`.
fn branch_state(branch: &crate::api::schema::GitBranchInfo) -> String {
    if branch.upstream_gone {
        "gone".to_owned()
    } else if branch.upstream.is_none() {
        "local".to_owned()
    } else if branch.ahead == 0 && branch.behind == 0 {
        "synced".to_owned()
    } else {
        crate::ui::push_status_text(None, Some((branch.ahead, branch.behind)))
    }
}

/// The agents a session can be handed over to, as `agent.handoff` names
/// them, with their menu labels.
pub(super) const HANDOFF_AGENTS: [(&str, &str); 3] =
    [("claude", "Claude"), ("pi", "Pi"), ("codex", "Codex")];

/// A push status chip as a menu title: the shown branch and the counts.
pub(super) type ChipTitle<'a> = (Option<&'a str>, Option<(usize, usize)>);

impl ClientContextMenuOverlay {
    /// The title drawn into the menu's top border: the branch menu repeats
    /// the chip it opened from.
    pub(super) fn title(&self) -> Option<ChipTitle<'_>> {
        match &self.target {
            ClientContextMenuTarget::Branches {
                branch,
                ahead_behind,
                ..
            } => Some((branch.as_deref(), *ahead_behind)),
            _ => None,
        }
    }

    /// The agent of the picker row at this index, whose `A` takes its colour.
    pub(super) fn picker_agent(&self, index: usize) -> Option<&str> {
        let ClientContextMenuTarget::AgentPicker {
            current,
            kinds: Some(Ok(kinds)),
            ..
        } = &self.target
        else {
            return None;
        };
        super::agent_launch::picker_agents(kinds, current.as_deref())
            .get(index)
            .copied()
    }

    pub(super) fn items(&self) -> Vec<ClientContextMenuItem> {
        let mut items = self.target_items();
        // A space's bookmark toggle, last, so the other items keep their
        // places.
        if let ClientContextMenuTarget::Workspace {
            bookmarked: Some(bookmarked),
            ..
        } = &self.target
        {
            items.push(ClientContextMenuItem {
                label: if *bookmarked {
                    "Remove from bookmarks"
                } else {
                    "Add to bookmarks"
                }
                .to_owned(),
                action: ClientContextMenuAction::ToggleBookmark,
            });
        }
        items
    }

    fn target_items(&self) -> Vec<ClientContextMenuItem> {
        use ClientContextMenuAction as Action;

        let item = |label: &str, action| ClientContextMenuItem {
            label: label.to_owned(),
            action,
        };
        match &self.target {
            ClientContextMenuTarget::Workspace { is_git: false, .. } => {
                vec![item("Rename", Action::Rename), item("Close", Action::Close)]
            }
            ClientContextMenuTarget::Workspace {
                is_linked_worktree: false,
                has_worktree_children: false,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close", Action::Close),
                item("New worktree", Action::NewWorktree),
                item("Open worktree...", Action::OpenWorktree),
            ],
            ClientContextMenuTarget::Workspace {
                is_linked_worktree: true,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close", Action::Close),
                item("Delete worktree checkout...", Action::RemoveWorktree),
            ],
            ClientContextMenuTarget::Workspace {
                has_worktree_children: true,
                close_group,
                collapsed,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item(
                    if *close_group { "Close group" } else { "Close" },
                    Action::Close,
                ),
                item("New worktree", Action::NewWorktree),
                item("Open worktree...", Action::OpenWorktree),
                item(
                    if *collapsed { "Expand" } else { "Collapse" },
                    Action::ToggleGroup,
                ),
            ],
            ClientContextMenuTarget::Tab {
                running_jobs,
                succeeded_jobs,
                failed_jobs,
                bookmarked,
                in_list,
                awaiting_panes,
                can_hand_over,
                ..
            } => {
                // The job actions are chips on one `Close jobs:` row, as the
                // tab line counts them: `◑ 2` (asks first), `!1`, `✓3`.
                let mut items = Vec::new();
                if !*in_list {
                    items.push(item("New tab", Action::NewTab));
                }
                items.push(item("Rename", Action::Rename));
                match awaiting_panes {
                    0 => {}
                    1 => items.push(item("Dismiss question", Action::DismissQuestions)),
                    count => items.push(item(
                        &format!("Dismiss {count} questions"),
                        Action::DismissQuestions,
                    )),
                }
                if let Some(bookmarked) = bookmarked {
                    items.push(item(
                        if *bookmarked {
                            "Remove from bookmarks"
                        } else {
                            "Add to bookmarks"
                        },
                        Action::ToggleBookmark,
                    ));
                }
                if *can_hand_over && !*in_list {
                    items.push(item("Hand over to…", Action::HandOver));
                }
                if *running_jobs > 0 {
                    items.push(item(
                        &format!("{} {running_jobs}", crate::ui::motion::job_glyph()),
                        Action::StopRunningJobs,
                    ));
                }
                if *failed_jobs > 0 {
                    items.push(item(&format!("!{failed_jobs}"), Action::CloseFailedJobs));
                }
                if *succeeded_jobs > 0 {
                    items.push(item(
                        &format!("✓{succeeded_jobs}"),
                        Action::CloseSucceededJobs,
                    ));
                }
                items.push(item("Close", Action::Close));
                items
            }
            ClientContextMenuTarget::Handoff { .. } => HANDOFF_AGENTS
                .iter()
                .enumerate()
                .map(|(index, (_, label))| item(label, Action::HandOverTo(index)))
                .collect(),
            ClientContextMenuTarget::AgentPicker { current, kinds, .. } => match kinds {
                None => vec![item("loading…", Action::Dismiss)],
                Some(Err(message)) => vec![item(message, Action::Dismiss)],
                Some(Ok(kinds)) => {
                    let agents = super::agent_launch::picker_agents(kinds, current.as_deref());
                    if agents.is_empty() {
                        return vec![item("no agents installed", Action::Dismiss)];
                    }
                    agents
                        .into_iter()
                        .enumerate()
                        .map(|(index, kind)| {
                            item(
                                &super::agent_launch::picker_label(kind, current.as_deref()),
                                Action::LaunchAgent(index),
                            )
                        })
                        .collect()
                }
            },
            ClientContextMenuTarget::Branches { branches, .. } => match branches {
                None => vec![item("loading…", Action::Dismiss)],
                Some(Err(message)) => vec![item(message, Action::Dismiss)],
                Some(Ok(branches)) => {
                    let others = branches
                        .iter()
                        .filter(|branch| !branch.current)
                        .collect::<Vec<_>>();
                    if others.is_empty() {
                        return vec![item("no other branches", Action::Dismiss)];
                    }
                    let name_width = others
                        .iter()
                        .map(|branch| usize::from(super::render::display_width(&branch.name)))
                        .max()
                        .unwrap_or(0)
                        .min(BRANCH_NAME_MAX_WIDTH);
                    others
                        .into_iter()
                        .map(|branch| {
                            let name = crate::ui::truncate_end(&branch.name, name_width);
                            let pad = name_width
                                .saturating_sub(usize::from(super::render::display_width(&name)));
                            item(
                                &format!("{name}{}  {}", " ".repeat(pad), branch_state(branch)),
                                Action::Dismiss,
                            )
                        })
                        .collect()
                }
            },
            ClientContextMenuTarget::SortSpaces(sort) => sort
                .menu_items()
                .into_iter()
                .map(|(key, label)| item(&label, Action::SortSpaces(key)))
                .collect(),
            ClientContextMenuTarget::Worker { can_take_over, .. } => {
                let mut items = vec![item("Open log", Action::OpenWorkerLog)];
                if *can_take_over {
                    items.push(item("Take over", Action::TakeOverWorker));
                }
                items
            }
            ClientContextMenuTarget::Pane {
                source_pane_id,
                has_manual_label,
                right_click_passthrough,
                awaiting_reply,
                ..
            } => {
                let mut items = vec![
                    item("Attach image…", Action::AttachImage),
                    item("Rename pane", Action::RenamePane),
                ];
                if *awaiting_reply {
                    items.push(item("Dismiss question", Action::DismissQuestions));
                }
                if *has_manual_label {
                    items.push(item("Clear pane name", Action::ClearPaneName));
                }
                if source_pane_id.is_some() {
                    items.push(item("Swap with focused pane", Action::SwapWithFocusedPane));
                }
                items.extend([
                    item("Split right", Action::SplitRight),
                    item("Split down", Action::SplitDown),
                    item("Zoom", Action::Zoom),
                    item(
                        if *right_click_passthrough {
                            "Use Herdr right-click menu"
                        } else {
                            "Send right-clicks to pane"
                        },
                        Action::ToggleRightClickPassthrough,
                    ),
                    item("Close pane", Action::ClosePane),
                ]);
                items
            }
        }
    }
}

impl ClientShellState {
    pub(super) fn open_workspace_context_menu(&mut self, workspace_id: String, x: u16, y: u16) {
        if let Some(menu) = self.workspace_context_menu(workspace_id, x, y) {
            self.overlay = Some(ClientShellOverlay::ContextMenu(menu));
        }
    }

    /// The space's menu at `(x, y)`; none when the space is gone.
    pub(super) fn workspace_context_menu(
        &self,
        workspace_id: String,
        x: u16,
        y: u16,
    ) -> Option<ClientContextMenuOverlay> {
        let snapshot = self.snapshot.as_deref()?;
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)?;
        let worktree = workspace.worktree.as_ref();
        let has_worktree_children = worktree.is_some_and(|worktree| {
            !worktree.is_linked_worktree
                && snapshot.workspaces.iter().any(|candidate| {
                    candidate.worktree.as_ref().is_some_and(|candidate| {
                        candidate.key == worktree.key && candidate.is_linked_worktree
                    })
                })
        });
        let close_group = super::sidebar::workspace_close_is_group(snapshot, workspace);
        let collapsed = worktree.is_some_and(|worktree| {
            self.group_is_collapsed(&self.active_endpoint_id, &worktree.key)
        });
        let bookmarked = self
            .supports_endpoint_method(&crate::api::schema::Method::WorkspaceBookmark(
                crate::api::schema::WorkspaceBookmarkParams {
                    workspace_id: String::new(),
                    bookmarked: true,
                },
            ))
            .then_some(workspace.bookmarked);
        Some(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                workspace_id,
                is_git: worktree.is_some() || workspace.branch.is_some(),
                is_linked_worktree: worktree.is_some_and(|worktree| worktree.is_linked_worktree),
                has_worktree_children,
                close_group,
                collapsed,
                bookmarked,
            },
            x,
            y,
            highlighted: 0,
        })
    }

    /// Opens the sort choice under the header button at `(x, y)`.
    pub(super) fn open_space_sort_menu(&mut self, x: u16, y: u16) {
        let sort = self.space_sort;
        let highlighted = sort
            .menu_items()
            .iter()
            .position(|(key, _)| *key == sort.key)
            .unwrap_or(0);
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::SortSpaces(sort),
            x,
            y,
            highlighted,
        }));
    }

    pub(super) fn open_tab_context_menu(&mut self, tab_id: String, x: u16, y: u16) {
        if let Some(menu) = self.tab_context_menu(tab_id, x, y, false) {
            self.overlay = Some(ClientShellOverlay::ContextMenu(menu));
        }
    }

    /// The tab's menu at `(x, y)`; none when the tab is gone.
    pub(super) fn tab_context_menu(
        &self,
        tab_id: String,
        x: u16,
        y: u16,
        in_list: bool,
    ) -> Option<ClientContextMenuOverlay> {
        let tab = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))?;
        let jobs = |status| {
            self.snapshot.as_deref().map_or(0, |snapshot| {
                super::tab_groups::child_tabs(snapshot, &tab_id)
                    .iter()
                    .filter(|child| child.status == Some(status))
                    .count()
            })
        };
        let running_jobs = jobs(crate::api::schema::TabStatus::Running);
        let succeeded_jobs = jobs(crate::api::schema::TabStatus::Succeeded);
        let failed_jobs = jobs(crate::api::schema::TabStatus::Failed);
        let bookmarked = self
            .supports_endpoint_method(&crate::api::schema::Method::TabBookmark(
                crate::api::schema::TabBookmarkParams {
                    tab_id: String::new(),
                    bookmarked: true,
                },
            ))
            .then_some(tab.bookmarked);
        let awaiting_panes = self
            .awaiting_reply_panes(|agent| agent.tab_id == tab_id)
            .len();
        let can_hand_over = self.supports_endpoint_method(
            &crate::api::schema::Method::AgentHandoff(crate::api::schema::AgentHandoffParams {
                pane_id: String::new(),
                to: String::new(),
                focus: false,
            }),
        ) && self.handoff_pane(&tab_id).is_some();
        Some(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id: tab.workspace_id.clone(),
                running_jobs,
                succeeded_jobs,
                failed_jobs,
                bookmarked,
                in_list,
                awaiting_panes,
                can_hand_over,
            },
            x,
            y,
            highlighted: 0,
        })
    }

    /// The pane whose session a tab hands over: its agent's, else (an agent
    /// that exited, for example at its usage limit) its focused pane. The
    /// server reports a pane without a known session.
    pub(super) fn handoff_pane(&self, tab_id: &str) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        snapshot
            .agents
            .iter()
            .find(|agent| agent.tab_id == tab_id)
            .map(|agent| agent.pane_id.clone())
            .or_else(|| {
                let panes = || snapshot.panes.iter().filter(|pane| pane.tab_id == tab_id);
                panes()
                    .find(|pane| pane.focused)
                    .or_else(|| panes().next())
                    .map(|pane| pane.pane_id.clone())
            })
    }

    /// Panes whose agent awaits a reply among those `select` picks, when
    /// the server can dismiss their questions (else none).
    pub(super) fn awaiting_reply_panes(
        &self,
        select: impl Fn(&crate::protocol::ClientShellAgent) -> bool,
    ) -> Vec<String> {
        let supported =
            self.supports_endpoint_method(&crate::api::schema::Method::PaneClearAwaitingReply(
                crate::api::schema::PaneClearAwaitingReplyParams {
                    pane_ids: Vec::new(),
                },
            ));
        if !supported {
            return Vec::new();
        }
        self.snapshot.as_deref().map_or_else(Vec::new, |snapshot| {
            snapshot
                .agents
                .iter()
                .filter(|agent| agent.awaiting_reply && select(agent))
                .map(|agent| agent.pane_id.clone())
                .collect()
        })
    }

    /// Dismisses these agents' questions with one request.
    fn dismiss_questions(&mut self, pane_ids: Vec<String>, outcome: &mut ClientShellInput) {
        if pane_ids.is_empty() {
            return;
        }
        self.push_endpoint_method(
            crate::api::schema::Method::PaneClearAwaitingReply(
                crate::api::schema::PaneClearAwaitingReplyParams { pane_ids },
            ),
            outcome,
        );
        outcome.repaint = true;
    }

    pub(super) fn open_pane_context_menu(&mut self, pane_id: String, x: u16, y: u16) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        let source_pane_id = snapshot
            .focused_pane_id
            .clone()
            .filter(|focused| focused != &pane_id);
        let awaiting_reply = !self
            .awaiting_reply_panes(|agent| agent.pane_id == pane_id)
            .is_empty();
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Pane {
                pane_id,
                workspace_id: pane.workspace_id.clone(),
                source_pane_id,
                has_manual_label: pane.label.is_some(),
                right_click_passthrough: pane.right_click_passthrough,
                awaiting_reply,
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    /// Opens the branch menu of a space over its push status chip, so the
    /// chip becomes the menu's title, and asks the server for the branches.
    pub(super) fn open_branch_menu(
        &mut self,
        workspace_id: String,
        chip: Rect,
        outcome: &mut ClientShellInput,
    ) {
        let Some(workspace) = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
        }) else {
            return;
        };
        let target = ClientContextMenuTarget::Branches {
            workspace_id: workspace_id.clone(),
            branch: workspace.branch.clone(),
            ahead_behind: workspace.git_ahead_behind,
            branches: None,
        };
        // The chip's text starts after its padding column; the title starts
        // two columns into the menu, after the corner and a space: there it
        // covers the chip's text.
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target,
            x: chip.x.saturating_sub(1),
            y: chip.y,
            highlighted: usize::MAX,
        }));
        outcome.repaint = true;
        let sent = self.push_endpoint_method_with_kind(
            crate::api::schema::Method::GitBranchList(crate::api::schema::GitBranchListParams {
                workspace_id: workspace_id.clone(),
            }),
            PendingEndpointKind::GitBranchList { workspace_id },
            outcome,
        );
        if !sent {
            if let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                target: ClientContextMenuTarget::Branches { branches, .. },
                ..
            })) = self.overlay.as_mut()
            {
                *branches = Some(Err("this server cannot list branches".to_owned()));
            }
        }
    }

    /// Fills the branch menu, when it is still open for that space.
    pub(super) fn complete_git_branch_list(
        &mut self,
        workspace_id: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target:
                ClientContextMenuTarget::Branches {
                    workspace_id: open,
                    branches,
                    ..
                },
            ..
        })) = self.overlay.as_mut()
        else {
            return (false, Vec::new());
        };
        if *open != workspace_id {
            return (false, Vec::new());
        }
        *branches = Some(match result {
            Ok(crate::api::schema::ResponseResult::GitBranchList { branches }) => Ok(branches),
            Ok(_) => Err("unexpected response".to_owned()),
            Err(error) if error.code.as_deref() == Some("endpoint_busy") => {
                Err("server busy, click again".to_owned())
            }
            Err(error) => Err(error.message),
        });
        (true, Vec::new())
    }

    pub(super) fn move_context_menu_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        let item_count = menu.items().len();
        if item_count == 0 {
            return;
        }
        menu.highlighted = (menu.highlighted as isize + delta)
            .clamp(0, item_count.saturating_sub(1) as isize) as usize;
    }

    pub(super) fn activate_context_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.take() else {
            return;
        };
        let Some(action) = menu.items().get(index).map(|item| item.action) else {
            outcome.repaint = true;
            return;
        };
        let (x, y) = (menu.x, menu.y);
        match menu.target {
            ClientContextMenuTarget::Tab { tab_id, .. }
                if action == ClientContextMenuAction::HandOver =>
            {
                // The pane now, not when the menu opened.
                if let Some(pane_id) = self.handoff_pane(&tab_id) {
                    self.overlay =
                        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                            target: ClientContextMenuTarget::Handoff { pane_id },
                            x,
                            y,
                            highlighted: 0,
                        }));
                }
            }
            ClientContextMenuTarget::Workspace {
                workspace_id,
                close_group,
                ..
            } => self.activate_workspace_context_action(workspace_id, close_group, action, outcome),
            ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id,
                ..
            } => self.activate_tab_context_action(tab_id, workspace_id, action, outcome),
            ClientContextMenuTarget::Branches { .. } => {}
            ClientContextMenuTarget::Worker { worker_id, .. } => match action {
                ClientContextMenuAction::OpenWorkerLog => self.open_worker_log(worker_id, outcome),
                ClientContextMenuAction::TakeOverWorker => {
                    self.take_over_worker(worker_id, outcome);
                }
                _ => {}
            },
            ClientContextMenuTarget::Handoff { pane_id } => {
                if let ClientContextMenuAction::HandOverTo(index) = action {
                    if let Some((to, _)) = HANDOFF_AGENTS.get(index) {
                        self.push_endpoint_method(
                            crate::api::schema::Method::AgentHandoff(
                                crate::api::schema::AgentHandoffParams {
                                    pane_id,
                                    to: (*to).to_owned(),
                                    focus: true,
                                },
                            ),
                            outcome,
                        );
                    }
                }
            }
            ClientContextMenuTarget::AgentPicker {
                workspace_id,
                current,
                kinds: Some(Ok(kinds)),
            } => {
                if let ClientContextMenuAction::LaunchAgent(index) = action {
                    let picked = super::agent_launch::picker_agents(&kinds, current.as_deref())
                        .get(index)
                        .map(|kind| kind.to_string());
                    if let Some(kind) = picked {
                        self.launch_agent(workspace_id, kind, outcome);
                    }
                }
            }
            ClientContextMenuTarget::AgentPicker { .. } => {}
            ClientContextMenuTarget::SortSpaces(_) => {
                if let ClientContextMenuAction::SortSpaces(key) = action {
                    self.space_sort = self.space_sort.clicked(key);
                    self.workspace_scroll = 0;
                    self.reveal_focused_workspace = true;
                    self.persist_chrome_preferences(outcome);
                }
            }
            ClientContextMenuTarget::Pane {
                pane_id,
                workspace_id,
                source_pane_id,
                right_click_passthrough,
                ..
            } => self.activate_pane_context_action(
                pane_id,
                workspace_id,
                source_pane_id,
                right_click_passthrough,
                action,
                outcome,
            ),
        }
        outcome.repaint = true;
    }

    pub(super) fn activate_workspace_context_action(
        &mut self,
        workspace_id: String,
        close_group: bool,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::KeybindAction;

        match action {
            ClientContextMenuAction::Rename => {
                let label = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.workspace_id == workspace_id)
                    })
                    .map(|workspace| workspace.label.clone());
                if let Some(label) = label {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename workspace",
                        input: TextEditor::new(&label, false),
                        target: ClientRenameTarget::Workspace { workspace_id },
                    }));
                }
            }
            ClientContextMenuAction::Close => {
                self.request_workspace_close(workspace_id, Some(close_group), outcome);
            }
            ClientContextMenuAction::ToggleBookmark => {
                // The flag now, not when the menu opened.
                let bookmarked = self.snapshot.as_deref().is_some_and(|snapshot| {
                    snapshot.workspaces.iter().any(|workspace| {
                        workspace.workspace_id == workspace_id && workspace.bookmarked
                    })
                });
                self.push_endpoint_method(
                    crate::api::schema::Method::WorkspaceBookmark(
                        crate::api::schema::WorkspaceBookmarkParams {
                            workspace_id,
                            bookmarked: !bookmarked,
                        },
                    ),
                    outcome,
                );
            }
            ClientContextMenuAction::NewWorktree => {
                self.begin_worktree_action_for(KeybindAction::NewWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::OpenWorktree => {
                self.begin_worktree_action_for(KeybindAction::OpenWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::RemoveWorktree => {
                self.begin_worktree_action_for(KeybindAction::RemoveWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::ToggleGroup => {
                let key = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.workspace_id == workspace_id)
                        .and_then(|workspace| workspace.worktree.as_ref())
                        .map(|worktree| worktree.key.clone())
                });
                if let Some(key) = key {
                    let endpoint_id = self.active_endpoint_id.clone();
                    self.toggle_collapsed_group(&endpoint_id, key);
                    self.persist_chrome_preferences(outcome);
                }
            }
            _ => {}
        }
    }

    pub(super) fn activate_tab_context_action(
        &mut self,
        tab_id: String,
        workspace_id: String,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{Method, TabTarget};

        if action == ClientContextMenuAction::StopRunningJobs {
            self.confirm_stop_running_jobs(&tab_id, outcome);
            return;
        }
        if action == ClientContextMenuAction::DismissQuestions {
            // The agents that await a reply now, not when the menu opened.
            let panes = self.awaiting_reply_panes(|agent| agent.tab_id == tab_id);
            self.dismiss_questions(panes, outcome);
            return;
        }
        if action == ClientContextMenuAction::ToggleBookmark {
            // The flag now, not when the menu opened; the focus stays put.
            let bookmarked = self
                .snapshot
                .as_deref()
                .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))
                .is_some_and(|tab| tab.bookmarked);
            self.push_endpoint_method(
                Method::TabBookmark(crate::api::schema::TabBookmarkParams {
                    tab_id,
                    bookmarked: !bookmarked,
                }),
                outcome,
            );
            outcome.repaint = true;
            return;
        }
        // Closing a tab's finished jobs keeps the focus where it is.
        let job_status = match action {
            ClientContextMenuAction::CloseSucceededJobs => {
                Some(crate::api::schema::TabStatus::Succeeded)
            }
            ClientContextMenuAction::CloseFailedJobs => Some(crate::api::schema::TabStatus::Failed),
            _ => None,
        };
        if let Some(status) = job_status {
            // The statuses now, not when the menu opened: a job may have
            // finished or closed since.
            let jobs = self
                .snapshot
                .as_deref()
                .map(|snapshot| {
                    super::tab_groups::child_tabs(snapshot, &tab_id)
                        .into_iter()
                        .filter(|child| child.status == Some(status))
                        .map(|child| child.tab_id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for job in jobs {
                self.push_endpoint_method(Method::TabClose(TabTarget { tab_id: job }), outcome);
            }
            outcome.repaint = true;
            return;
        }
        self.push_endpoint_method(
            Method::TabFocus(TabTarget {
                tab_id: tab_id.clone(),
            }),
            outcome,
        );
        match action {
            ClientContextMenuAction::NewTab => {
                if self.config.prompt_new_tab_name {
                    let default_name = (self
                        .snapshot
                        .as_deref()
                        .map(|snapshot| {
                            snapshot
                                .tabs
                                .iter()
                                .filter(|tab| tab.workspace_id == workspace_id)
                                .count()
                        })
                        .unwrap_or(0)
                        + 1)
                    .to_string();
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "new tab",
                        input: TextEditor::new(&default_name, true),
                        target: ClientRenameTarget::NewTab {
                            workspace_id,
                            default_name,
                        },
                    }));
                } else {
                    let method = self.new_tab_method(workspace_id, None);
                    self.push_endpoint_method(method, outcome);
                }
            }
            ClientContextMenuAction::Rename => {
                let tab = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id));
                if let Some(tab) = tab {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename tab",
                        input: TextEditor::new(&tab.label, false),
                        target: ClientRenameTarget::Tab {
                            tab_id,
                            auto_name: !tab.custom_label,
                            original_name: tab.label.clone(),
                        },
                    }));
                }
            }
            ClientContextMenuAction::Close => {
                self.request_tab_close(tab_id, outcome);
            }
            _ => {}
        }
    }

    fn activate_pane_context_action(
        &mut self,
        pane_id: String,
        workspace_id: String,
        source_pane_id: Option<String>,
        right_click_passthrough: bool,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{
            Method, PaneInputSetParams, PaneRenameParams, PaneRightClickTarget, PaneSplitParams,
            PaneSwapParams, PaneTarget, PaneZoomMode, PaneZoomParams, SplitDirection,
        };

        match action {
            ClientContextMenuAction::DismissQuestions => {
                let panes = self.awaiting_reply_panes(|agent| agent.pane_id == pane_id);
                self.dismiss_questions(panes, outcome);
            }
            ClientContextMenuAction::AttachImage => {
                self.open_image_picker(pane_id);
                outcome.repaint = true;
            }
            ClientContextMenuAction::RenamePane => {
                let label = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id == pane_id)
                        .and_then(|pane| pane.label.clone())
                });
                self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                    title: "rename pane",
                    input: TextEditor::new(label.as_deref().unwrap_or_default(), label.is_none()),
                    target: ClientRenameTarget::Pane { pane_id },
                }));
            }
            ClientContextMenuAction::ClearPaneName => self.push_endpoint_method(
                Method::PaneRename(PaneRenameParams {
                    pane_id,
                    label: None,
                }),
                outcome,
            ),
            ClientContextMenuAction::SwapWithFocusedPane => {
                if let Some(source_pane_id) = source_pane_id {
                    self.push_endpoint_method(
                        Method::PaneSwap(PaneSwapParams {
                            pane_id: None,
                            direction: None,
                            source_pane_id: Some(source_pane_id.clone()),
                            target_pane_id: Some(pane_id),
                        }),
                        outcome,
                    );
                    self.push_endpoint_method(
                        Method::PaneFocus(PaneTarget {
                            pane_id: source_pane_id,
                        }),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::SplitRight | ClientContextMenuAction::SplitDown => {
                self.push_endpoint_method(
                    Method::PaneSplit(PaneSplitParams {
                        workspace_id: Some(workspace_id),
                        target_pane_id: Some(pane_id),
                        direction: if action == ClientContextMenuAction::SplitRight {
                            SplitDirection::Right
                        } else {
                            SplitDirection::Down
                        },
                        ratio: None,
                        cwd: None,
                        focus: true,
                        right_click: Default::default(),
                        env: Default::default(),
                    }),
                    outcome,
                );
            }
            ClientContextMenuAction::Zoom => self.push_endpoint_method(
                Method::PaneZoom(PaneZoomParams {
                    pane_id: Some(pane_id),
                    mode: PaneZoomMode::Toggle,
                }),
                outcome,
            ),
            ClientContextMenuAction::ToggleRightClickPassthrough => self.push_endpoint_method(
                Method::PaneInputSet(PaneInputSetParams {
                    pane_id,
                    right_click: if right_click_passthrough {
                        PaneRightClickTarget::Herdr
                    } else {
                        PaneRightClickTarget::Pane
                    },
                }),
                outcome,
            ),
            ClientContextMenuAction::ClosePane => self.request_pane_close(pane_id, outcome),
            _ => {}
        }
    }
}
