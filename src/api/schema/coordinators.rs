use serde::{Deserialize, Serialize};

/// Claims a repository's coordination: starts a tenure bound to the pane
/// (one active tenure per repository). Setting a tab's role to
/// `coordinator` starts one for the tab's focused pane.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorStartParams {
    /// A directory in the repository; the repository of the pane's
    /// directory when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The coordinator's pane (the CLI sends its `HERDR_PANE_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// Ends a tenure: the one named, else the one bound to the pane.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorEndParams {
    /// Why it ends, stored as its `end_reason`; `ended` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_id: Option<String>,
    /// The CLI sends its `HERDR_PANE_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// Lists the active tenures, of one repository when given.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorStatusParams {
    /// A directory in the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// One coordination tenure: who coordinated a repository's TODO, from when
/// to when. Its id is its own, never a pane's or an agent session's: those
/// are its bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorInfo {
    /// `c-` and 8 lowercase base32 characters.
    pub coordinator_id: String,
    /// The repository: the parent of its git common directory.
    pub repo: String,
    /// The TODO item it works on; not recorded yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// Unix milliseconds.
    pub started_ms: u64,
    /// Absent while it is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// Why it ended: `ended` (`coordinator.end`), `role_cleared` (its tab's
    /// role was cleared), `orphaned` (its pane closed or its agent exited,
    /// also when found at server start), or the reason `coordinator.end`
    /// was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// Grows by one with each tenure of the repository.
    pub epoch: i64,
    /// Its latest binding's pane and agent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}
