//! The launch button left of a space's `+`: a new tab running the agent last
//! launched there, and a menu of the other installed agents.

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
        if self
            .collapsed_groups
            .remove(&super::space_tabs::tabs_collapse_key(&workspace_id))
        {
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

    /// The menu of the other installed agents, under the launch button; the
    /// server says which are installed.
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
            x: button.x,
            y: button.y.saturating_add(1),
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

/// The agents the picker offers: the installed ones but the button's own.
pub(super) fn picker_agents<'a>(kinds: &'a [String], current: Option<&str>) -> Vec<&'a str> {
    kinds
        .iter()
        .map(String::as_str)
        .filter(|kind| Some(*kind) != current)
        .collect()
}
