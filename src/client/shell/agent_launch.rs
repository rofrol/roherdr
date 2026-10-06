//! The launch button left of a space's `+`: a new tab running the agent last
//! launched there, and a menu of the installed agents that teaches their
//! colours.

use super::*;

/// Two letters naming an agent, on the launch button and the tab lines.
pub(super) fn agent_badge(kind: &str) -> String {
    match kind {
        "claude" => "CL".into(),
        "codex" => "CX".into(),
        "gemini" => "GE".into(),
        "agy" => "AG".into(),
        "cursor" => "CU".into(),
        "copilot" => "CP".into(),
        "opencode" => "OC".into(),
        "qodercli" => "QO".into(),
        _ => kind
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(2)
            .collect::<String>()
            .to_ascii_uppercase(),
    }
}

/// The badge's colour: the vendor's hue where the theme has one close to it.
pub(super) fn agent_badge_color(kind: &str, palette: &Palette) -> ratatui::style::Color {
    match kind {
        "claude" => palette.peach,
        "codex" | "copilot" => palette.green,
        "gemini" | "agy" => palette.blue,
        "pi" | "omp" => palette.mauve,
        "cursor" | "opencode" => palette.yellow,
        _ => palette.overlay1,
    }
}

/// The canonical agent running in a tab, as detected.
pub(super) fn tab_agent<'a>(snapshot: &'a ClientShellSnapshot, tab_id: &str) -> Option<&'a str> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.tab_id == tab_id)
        .find_map(|agent| agent.agent.as_deref())
        .filter(|kind| crate::detect::parse_canonical_agent_label(kind).is_some())
}

/// The agent the launch button starts in a space: the one last launched
/// from this client, else the agent of the space's newest tab that has one,
/// else the most recently active agent anywhere.
pub(super) fn space_launch_agent(
    snapshot: &ClientShellSnapshot,
    launched: &HashMap<String, String>,
    workspace_id: &str,
) -> Option<String> {
    if let Some(kind) = launched.get(workspace_id) {
        return Some(kind.clone());
    }
    let newest_in_space = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace_id)
        .filter_map(|tab| Some((tab.number, tab_agent(snapshot, &tab.tab_id)?)))
        .max_by_key(|(number, _)| *number)
        .map(|(_, kind)| kind.to_owned());
    newest_in_space.or_else(|| {
        snapshot
            .agents
            .iter()
            .filter(|agent| {
                agent
                    .agent
                    .as_deref()
                    .is_some_and(|kind| crate::detect::parse_canonical_agent_label(kind).is_some())
            })
            .max_by_key(|agent| agent.state_change_seq)
            .and_then(|agent| agent.agent.clone())
    })
}

impl ClientShellState {
    /// A left click on the launch button: a new tab running the agent, or
    /// the picker when the space has no agent to repeat yet.
    pub(super) fn click_launch_button(
        &mut self,
        workspace_id: String,
        button: Rect,
        outcome: &mut ClientShellInput,
    ) {
        let kind = self.snapshot.as_deref().and_then(|snapshot| {
            space_launch_agent(snapshot, &self.launched_agents, &workspace_id)
        });
        match kind {
            Some(kind) => self.launch_agent(workspace_id, kind, outcome),
            None => self.open_agent_picker(workspace_id, button, outcome),
        }
    }

    pub(super) fn launch_agent(
        &mut self,
        workspace_id: String,
        kind: String,
        outcome: &mut ClientShellInput,
    ) {
        // A collapsed space shows the new tab.
        if self.unfold_space_tabs(&workspace_id) {
            self.persist_chrome_preferences(outcome);
        }
        self.launched_agents
            .insert(workspace_id.clone(), kind.clone());
        self.push_endpoint_method(
            crate::api::schema::Method::TabCreateAgent(crate::api::schema::TabCreateAgentParams {
                workspace_id,
                kind,
                focus: true,
            }),
            outcome,
        );
        outcome.repaint = true;
    }

    /// The menu of the installed agents, over the launch button: its first
    /// row's `A` covers the button's (the menu's border and padding sit one
    /// column left and one row up). The server says which are installed.
    pub(super) fn open_agent_picker(
        &mut self,
        workspace_id: String,
        button: Rect,
        outcome: &mut ClientShellInput,
    ) {
        let current = self.snapshot.as_deref().and_then(|snapshot| {
            space_launch_agent(snapshot, &self.launched_agents, &workspace_id)
        });
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::AgentPicker {
                workspace_id: workspace_id.clone(),
                current,
                kinds: None,
            },
            x: button.x.saturating_sub(1),
            y: button.y.saturating_sub(1),
            highlighted: 0,
        }));
        outcome.repaint = true;
        let sent = self.push_endpoint_method_with_kind(
            crate::api::schema::Method::AgentKindList(Default::default()),
            PendingEndpointKind::AgentKindList { workspace_id },
            outcome,
        );
        if !sent {
            if let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                target: ClientContextMenuTarget::AgentPicker { kinds, .. },
                ..
            })) = self.overlay.as_mut()
            {
                *kinds = Some(Err("this server cannot list agents".to_owned()));
            }
        }
    }

    /// Fills the agent picker, when it is still open for that space.
    pub(super) fn complete_agent_kind_list(
        &mut self,
        workspace_id: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target:
                ClientContextMenuTarget::AgentPicker {
                    workspace_id: open,
                    kinds,
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
        *kinds = Some(match result {
            Ok(crate::api::schema::ResponseResult::AgentKindList { kinds }) => Ok(kinds),
            Ok(_) => Err("unexpected response".to_owned()),
            Err(error) if error.code.as_deref() == Some("endpoint_busy") => {
                Err("server busy, click again".to_owned())
            }
            Err(error) => Err(error.message),
        });
        (true, Vec::new())
    }
}

/// `ui.sidebar.spaces.todo_command` for one space: `{space}` becomes its
/// label, quoted for the shell that runs it.
pub(super) fn space_command(template: &str, label: &str) -> String {
    template.replace("{space}", &crate::platform::custom_command_argument(label))
}

/// The toast for a finished space command: its first output line, or on a
/// failure the first line of its errors.
pub(super) fn space_command_notice(
    result: &Result<SpaceCommandOutput, String>,
) -> (ClientEndpointNoticeKind, String) {
    let first_line = |text: &str| {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    };
    match result {
        Ok(output) if output.success => (
            ClientEndpointNoticeKind::Info,
            first_line(&output.stdout).unwrap_or_else(|| "done".to_owned()),
        ),
        Ok(output) => (
            ClientEndpointNoticeKind::Rejected,
            first_line(&output.stderr)
                .or_else(|| first_line(&output.stdout))
                .unwrap_or_else(|| "failed".to_owned()),
        ),
        Err(error) => (
            ClientEndpointNoticeKind::Rejected,
            format!("could not run todo_command: {error}"),
        ),
    }
}

impl ClientShellState {
    /// A click on a space's `T`: runs `todo_command` for it on this machine.
    pub(super) fn click_todo_button(&mut self, workspace_id: &str, outcome: &mut ClientShellInput) {
        let Some(template) = self.config.spaces.todo_command.clone() else {
            return;
        };
        let Some(label) = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .map(|workspace| workspace.label.clone())
        }) else {
            return;
        };
        self.show_space_command_notice(
            &label,
            ClientEndpointNoticeKind::Info,
            "starting…".to_owned(),
        );
        outcome.actions.push(ClientShellAction::RunSpaceCommand {
            command: space_command(&template, &label),
            space: label,
        });
        outcome.repaint = true;
    }

    /// The command started by `click_todo_button` ended: its output as a
    /// toast, which stays until clicked when the command failed.
    pub(crate) fn space_command_finished(
        &mut self,
        space: &str,
        result: Result<SpaceCommandOutput, String>,
    ) -> bool {
        let (kind, body) = space_command_notice(&result);
        if kind != ClientEndpointNoticeKind::Info {
            tracing::warn!(%space, ?result, "space todo_command failed");
        }
        self.show_space_command_notice(space, kind, body);
        true
    }

    fn show_space_command_notice(
        &mut self,
        space: &str,
        kind: ClientEndpointNoticeKind,
        body: String,
    ) {
        // A failure waits for the user; a day stands in for "until clicked".
        let duration = if kind == ClientEndpointNoticeKind::Info {
            std::time::Duration::from_secs(8)
        } else {
            std::time::Duration::from_secs(24 * 60 * 60)
        };
        self.visible_endpoint_notice = Some(ClientVisibleEndpointNotice {
            key: ClientEndpointNoticeKey {
                boot_id: "local".into(),
                kind,
                code: format!("space-command:{space}"),
            },
            title: format!("TODO {space}"),
            body,
            deadline: std::time::Instant::now() + duration,
        });
    }
}

/// The agents the picker offers: the installed ones, the button's own first.
pub(super) fn picker_agents<'a>(kinds: &'a [String], current: Option<&str>) -> Vec<&'a str> {
    let mut agents = kinds.iter().map(String::as_str).collect::<Vec<_>>();
    // Stable: the others keep the server's order.
    agents.sort_by_key(|kind| Some(*kind) != current);
    agents
}

/// A picker row: the button's `A` (coloured when drawn), a gap the
/// highlight starts in, and the agent; the button's own agent has a check.
pub(super) fn picker_label(kind: &str, current: Option<&str>) -> String {
    if Some(kind) == current {
        format!("{LAUNCH_GLYPH}  {kind} ✓")
    } else {
        format!("{LAUNCH_GLYPH}  {kind}")
    }
}

/// The letter on the launch button and before each picker row.
pub(super) const LAUNCH_GLYPH: char = 'A';
