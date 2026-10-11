use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::{AgentStatus, ReadFormat, ReadSource};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentReadParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub source: ReadSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default)]
    pub format: ReadFormat,
    #[serde(default = "super::common::default_true")]
    pub strip_ansi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSendKeysParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentWaitParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Waits until the agent's state changes after the caller looked at it: answers with the agent
/// once its `state_change_seq` differs from `state_change_seq` (level-triggered, so a change
/// made before the wait began answers at once), and `agent_not_running` when the agent exits,
/// is released or its pane closes. A caller reads the agent and its own sources (a
/// transcript), then waits with the `state_change_seq` it read, so no change between the read
/// and the wait is missed. The sequence is per server: after a restart it differs, which
/// answers at once and makes the caller look again. It has no timeout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentWaitChangeParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub state_change_seq: u64,
}

/// Makes `agent.prompt` wait after typing. From a non-working state it first waits for the
/// agent's acknowledgement, the same as `agent.prompt_confirmed` (the prompt's turn report, else
/// the agent turning `working`), with no time limit of its own; then it waits for one of `until`
/// (default `idle`, `done` or `blocked`). A dialog before the acknowledgement answers with the
/// `blocked` agent when `until` includes `blocked`, else `agent_prompt_blocked` naming the
/// dialog. `timeout_ms` is the caller's: when it passes before the acknowledgement the answer is
/// `agent_prompt_stalled`, after it `timeout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptWaitOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) submission_deadline: Option<std::time::Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentRenameParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentViewSetParams {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AgentViewFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<AgentViewSort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentViewClearParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AgentViewFilter {
    All {
        filters: Vec<AgentViewFilter>,
    },
    Any {
        filters: Vec<AgentViewFilter>,
    },
    Not {
        filter: Box<AgentViewFilter>,
    },
    Eq {
        field: AgentViewField,
        value: AgentViewValue,
    },
    In {
        field: AgentViewField,
        values: Vec<AgentViewValue>,
    },
    Exists {
        field: AgentViewField,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewField {
    Builtin(AgentViewBuiltinField),
    Token { token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewBuiltinField {
    Status,
    WorkspaceId,
    TabId,
    PaneId,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewValue {
    String(String),
    Bool(bool),
    Number(u64),
    Context { context: AgentViewContext },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewContext {
    CurrentWorkspaceId,
    CurrentTabId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentViewSort {
    pub field: AgentViewSortField,
    #[serde(default)]
    pub order: AgentViewSortOrder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum AgentViewSortField {
    Builtin(AgentViewBuiltinSortField),
    Token { token: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewBuiltinSortField {
    WorkspaceOrder,
    TabOrder,
    PaneOrder,
    Attention,
    Status,
    Agent,
    Seen,
    StateChangeSeq,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentViewSortOrder {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentStartParams {
    pub name: String,
    pub kind: String,
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Startup timeout in milliseconds. Values must be greater than 3000 and at most 300000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Hands the session of the agent in `pane_id` over to another agent: a new
/// tab after the source tab, in the source pane's directory, starts `to` with
/// a first prompt that points at the source session's transcript. The source
/// tab stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentHandoffParams {
    pub pane_id: String,
    /// The agent to continue the work: `claude`, `pi` or `codex`.
    pub to: String,
    #[serde(default)]
    pub focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<AgentPromptWaitOptions>,
    /// Set by `agent.prompt_turn`: refuse before typing when the agent does not report turns.
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) follow_turn: bool,
}

/// Sends a prompt like `agent.prompt` and waits until the turn that prompt started ends, as the
/// agent's integration reports it. Refused with `turn_tracking_unsupported`, before anything is
/// typed, when the agent does not report its turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptTurnParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptStatusParams {
    pub request_id: String,
}

/// Types a prompt like `agent.prompt` and returns at once with the request herdr follows
/// (`prompt_request`), for `agent.wait_turn`. Refused with `turn_tracking_unsupported`, before
/// anything is typed, when the agent does not report its turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptTrackedParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub text: String,
}

/// Types a prompt like `agent.prompt` and answers only once the agent shows it accepted it: the
/// turn report of the prompt (Claude's `UserPromptSubmit`, Pi's turn start) for an agent that
/// reports turns, else the agent's state turning `working`. An agent already `working` when the
/// prompt is typed has its input open and queues it, so the written submission is the
/// acknowledgement. A dialog (`blocked`) or an agent not ready is refused before typing, naming
/// the dialog when screen detection recognizes it; a dialog that appears after typing, before
/// the acknowledgement, answers `agent_prompt_blocked` without typing again. It has no timeout:
/// it waits for one of these events, the agent's exit, or the caller closing the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptConfirmedParams {
    pub target: String,
    /// Workspace whose agent names take precedence when `target` is an agent
    /// name: a name found there resolves even if other workspaces use it too;
    /// a name absent there resolves across all workspaces as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
    pub text: String,
}

/// Waits until the turn a followed prompt (`agent.prompt_tracked`, `agent.prompt`) started
/// ends, and says how it ended. It checks the request and subscribes to its changes in one
/// step, so a turn that ended before the wait began still answers `finished`. It has no
/// timeout: every outcome is an event, and a request herdr no longer knows (its server
/// restarted) answers `unknown_request` at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentWaitTurnParams {
    pub request_id: String,
}

/// How the turn of a followed prompt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentTurnEndReason {
    /// The agent reported the turn's end (Claude's `Stop`).
    Finished,
    /// The agent reported the turn ended on an error (Claude's `StopFailure`); see `error`.
    Failed,
    /// The turn was interrupted: herdr sent the agent an interrupt key (Esc, Ctrl-C) while it
    /// worked and another turn started before its end report, or the agent reported its end
    /// as interrupted.
    Interrupted,
    /// The agent's own process ended, or its pane closed, before the turn ended.
    Exited,
    /// Herdr does not follow this request: it never did, it was dropped as the oldest, or the
    /// server restarted and lost it. Its turn's outcome is not known.
    UnknownRequest,
    /// Another turn started before this one reported its end, with no signal saying why: the
    /// user may have interrupted it (Claude reports no `Stop` for a turn ended with Esc), or
    /// the agent took a queued message into its work. Also any reason this client does not
    /// know.
    #[serde(other)]
    Unknown,
}

/// A prompt herdr typed into an agent and how far the turn it started got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentPromptRequest {
    pub request_id: String,
    pub state: AgentPromptRequestState,
    /// The error a `failed` turn ended on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromptRequestState {
    /// Typed into the agent; the turn it starts has not been reported yet.
    Accepted,
    /// The agent reported that a turn started with this prompt.
    Working,
    /// That turn ended.
    Finished,
    /// The agent does not report its turns, so herdr cannot follow this prompt.
    Unsupported,
    /// That turn ended on an error the agent reported; see `error`.
    Failed,
    /// The turn was interrupted: herdr sent an interrupt key while it worked, or the agent
    /// reported its end as interrupted.
    Interrupted,
    /// The agent's process ended before the turn did.
    Exited,
    /// Another turn started before this one reported its end, with no signal saying why; also
    /// any state this client does not know.
    #[serde(other)]
    Unknown,
}

impl AgentPromptRequestState {
    /// The prompt's turn is over, however it ended.
    pub fn is_turn_end(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::Failed | Self::Interrupted | Self::Exited | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentInfo {
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub screen_detection_skipped: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionInfo>,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub launch_pending: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub interactive_ready: bool,
    #[serde(default)]
    pub state_change_seq: u64,
    /// The agent's last turn ended by asking the user something (reported through
    /// `pane.report_awaiting_reply`), nobody has typed into the pane since, and it is idle.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub awaiting_reply: bool,
    /// What the agent asks while it waits on the user: the question it reported with
    /// `pane.report_awaiting_reply`, or while blocked its hook's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Unix milliseconds since when the agent waits on the user: blocked, awaiting a reply,
    /// or stopped by a limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since_ms: Option<u64>,
    /// The limit the agent's last turn ended on (`pane.report_limit`), while it idles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited: Option<AgentLimit>,
    /// What the agent reported it works on now (`pane.report_task`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The current idle transition completed work, independently of who has viewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    pub revision: u64,
}

/// Which limit ended an agent's turn; the remedies differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentLimitKind {
    /// A usage or rate limit that resets on its own.
    Usage,
    /// Out of credits or a billing problem: waiting does not help.
    Credits,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentLimit {
    pub kind: AgentLimitKind,
    /// Unix seconds when the limit resets, from herdr's usage report of the agent's provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    /// The agent's own error text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}
