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

/// Hands a repository's coordination to another pane: the tenure named,
/// else the one bound to `pane_id`, ends `handed_off`, and the next tenure
/// starts in `to_pane_id` with the next epoch, taking over the old one's
/// current item, workers and runs, in one transaction.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorHandoffParams {
    /// The pane the next tenure is bound to, with its agent session.
    pub to_pane_id: String,
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
    /// The TODO item it works on: the item of the `todo.run` it claimed,
    /// until that run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// Unix milliseconds.
    pub started_ms: u64,
    /// Absent while it is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// Why it ended: `ended` (`coordinator.end`), `role_cleared` (its tab's
    /// role was cleared), `orphaned` (its pane closed or its agent exited,
    /// also when found at server start), `handed_off`
    /// (`coordinator.handoff`), or the reason `coordinator.end` was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// Grows by one with each tenure of the repository.
    pub epoch: i64,
    /// Its latest binding's pane and agent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// A headless item coordinator's tenure (`todo.next`): no pane binds it,
    /// it runs as worker `worker_id`.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub headless: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// Records an exception to a coordinator tab's command allowlist: one tool
/// call that carried `# herdr-override: <reason>`. The server stores it
/// (append-only, in the worker store) and notifies the user; the hook lets
/// the call run only after this succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorRecordOverrideParams {
    /// The coordinator's pane (the hook sends its `HERDR_PANE_ID`).
    pub pane_id: String,
    /// The tool the call used, such as `Bash`.
    pub tool: String,
    /// The call's command, as the agent wrote it.
    pub command: String,
    /// The reason after `herdr-override:`; one line.
    pub reason: String,
    /// The agent's directory, which names the repository when the pane has
    /// no active tenure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The agent session that made the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Lists the recorded allowlist exceptions, of one repository when given.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HistoryOverridesParams {
    /// A directory in the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// One exception to a coordinator tab's command allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorOverride {
    /// The record's number, growing in the order of writing.
    pub id: i64,
    /// Unix milliseconds.
    pub ts_ms: u64,
    pub pane_id: String,
    pub tool: String,
    /// The command, cut to its first 4000 characters.
    pub command: String,
    pub reason: String,
    /// The repository: the pane's tenure's, else that of `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The pane's active tenure then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_id: Option<String>,
    /// The TODO item that tenure worked on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}
