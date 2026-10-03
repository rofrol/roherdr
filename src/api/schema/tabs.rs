use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

/// Creates a tab already nested under a top-level tab, in that tab's
/// workspace, so it never shows as a top-level tab first. A separate method
/// from `tab.create`, whose shape is frozen: an older server rejects this one
/// instead of ignoring the parent and creating a top-level tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabCreateChildParams {
    pub parent_tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

/// Creates a tab running an interactive agent, typed into its new shell as
/// if the user had launched it there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabCreateAgentParams {
    pub workspace_id: String,
    /// A canonical agent id, as `agent.kind_list` returns.
    pub kind: String,
    #[serde(default)]
    pub focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TabListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabRenameParams {
    pub tab_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabMoveParams {
    pub tab_id: String,
    pub insert_index: usize,
}

/// Outcome of the work a tab runs, set by whoever runs it (e.g. a job runner).
/// Clients choose how to show it and summarise a parent's children with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TabStatus {
    Running,
    Succeeded,
    Failed,
    /// A value from a newer server that this build does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetParentParams {
    pub tab_id: String,
    /// The top-level tab to nest this tab under; omit or null to make it top-level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tab_id: Option<String>,
}

/// What a running tab's job is doing, as far as its runner can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TabActivity {
    /// No output for a while and (almost) no CPU: the job runs but does nothing.
    Idle,
    /// A value from a newer server that this build does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetStatusParams {
    pub tab_id: String,
    /// Omit or null to clear the status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TabStatus>,
    /// What the running job does; omit or null to clear it (a status change
    /// without an activity always clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<TabActivity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabBookmarkParams {
    pub tab_id: String,
    /// True to bookmark the tab, false to remove the bookmark; repeating it is
    /// harmless.
    pub bookmarked: bool,
}

/// Runtime facts supplied by a job runner; presentation belongs to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabJobMetadata {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub why: Option<String>,
    pub origin: String,
    #[serde(default)]
    pub owner_pane: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabSetJobMetadataParams {
    pub tab_id: String,
    /// Null clears a previous job registration.
    pub job: Option<TabJobMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TabInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<TabJobMetadata>,
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TabStatus>,
    /// Whether the user bookmarked the tab (see `tab.bookmark`). Absent
    /// means false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bookmarked: bool,
    /// What the running job does (see `tab.set_status`); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<TabActivity>,
}
