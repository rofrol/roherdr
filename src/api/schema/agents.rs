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

/// Makes `agent.prompt` wait after typing. A prompt for an agent that is `working` is typed only
/// once its state changes to anything else, as with `agent.prompt_confirmed`; a caller timeout
/// before that answers `timeout` with nothing typed. After typing it first waits for the
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
    /// The caller's own bound on the launch, in milliseconds (greater than 3000, at most
    /// 300000). Herdr adds none: without it the name stays registered until the launch ends.
    /// With it, a launch whose agent never appeared by then gives its name up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// The agent's first prompt, passed as its launch argument (claude, codex and pi) and
    /// followed like one `agent.prompt` typed: the response's `prompt_request` names it for
    /// `agent.wait_turn`, `agent.prompt_status` and `agent.wait_verdict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The pane that started the agent, recorded with the first prompt's assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
}

/// Waits until the launch `agent.start` began in a pane ends: the agent's integration
/// reported its session (Claude's `SessionStart`) or screen detection saw it ready, it stopped
/// at a dialog, it exited or another program took the pane, or its integration reported a
/// limit. Every outcome is an event; `timeout_ms` is only the caller's own bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentWaitStartParams {
    /// The name the launch registered.
    pub name: String,
    /// The terminal `agent.start` answered with: a name that moved to another terminal is
    /// not this launch's.
    pub terminal_id: String,
    /// The pane `agent.start` answered with, read when the name no longer resolves.
    pub pane_id: String,
    /// The kind `agent.start` launched.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_workspace_id: Option<String>,
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
    /// The pane that sends the prompt (the caller's `HERDR_PANE_ID`): the coordinator that
    /// owns the assignment herdr registers, whose inbox its supervision findings go to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// The prompt is a note, a message rather than a worker's task (`agent prompt --note`):
    /// herdr types and confirms it but awaits no verdict and tracks no assignment for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub note: bool,
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
    /// The pane that sends the prompt (the caller's `HERDR_PANE_ID`): the coordinator that
    /// owns the assignment herdr registers, whose inbox its supervision findings go to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// The prompt is a note, a message rather than a worker's task (`agent prompt --note`):
    /// herdr types and confirms it but awaits no verdict and tracks no assignment for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub note: bool,
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
    /// The pane that sends the prompt (the caller's `HERDR_PANE_ID`): the coordinator that
    /// owns the assignment herdr registers, whose inbox its supervision findings go to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// The prompt is a note, a message rather than a worker's task (`agent prompt --note`):
    /// herdr types and confirms it but awaits no verdict and tracks no assignment for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub note: bool,
}

/// Types a prompt like `agent.prompt` and answers only once the agent shows it accepted it: the
/// turn report of the prompt (Claude's `UserPromptSubmit`, Pi's turn start) for an agent that
/// reports turns, else the agent's state turning `working` after the typing; a state the agent
/// already had never counts. A prompt for an agent that is `working` waits, untyped, for its
/// state to change to anything else (its `idle` event) and is typed then. A dialog (`blocked`)
/// or an agent not ready is refused before typing, naming the dialog when screen detection
/// recognizes it; a dialog that appears after typing, before the acknowledgement, answers
/// `agent_prompt_blocked` without typing again. It has no timeout:
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
    /// The pane that sends the prompt (the caller's `HERDR_PANE_ID`): the coordinator that
    /// owns the assignment herdr registers, whose inbox its supervision findings go to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// The prompt is a note, a message rather than a worker's task (`agent prompt --note`):
    /// herdr types and confirms it but awaits no verdict and tracks no assignment for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub note: bool,
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
    /// The error a `failed` turn ended on; for `unsupported`, why herdr cannot follow it yet.
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
    /// The agent has not reported its turns (see `error`), so herdr cannot follow this prompt
    /// yet; it follows it once the agent's integration reports the turn.
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
    /// The agent asked the user something (reported through `pane.report_awaiting_reply`)
    /// that is still open, and it is idle: the pane's `?` mark.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub awaiting_reply: bool,
    /// The agent's open questions, oldest first, whatever its state: each stays until the
    /// user submits a reply, the agent withdraws it or its session ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<AgentOpenQuestion>,
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
    /// The agent's CLI was updated on disk since it started: it runs an older version than
    /// its launcher now has installed, until it is restarted. Claude and pi only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_pending: Option<AgentRestartPending>,
    /// The input context of the agent's last completed request, read from its transcript.
    /// Absent while unknown: an agent whose transcripts herdr cannot read (Claude only for
    /// now), and a new session (`/clear`, a resume, a compaction) or a rewritten transcript
    /// until that session's first completed request. Unknown is never 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentContext>,
    /// What the agent reported it works on now (`pane.report_task`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// How many background tasks (shells, dev servers, waits) the agent had running when
    /// its last turn ended, from the decimal `bg_count` token its integration reports
    /// through `pane.report_metadata`. Absent when unknown (no report seen, or an agent
    /// that reports none); `0` when the agent was seen running none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tasks: Option<u32>,
    /// The current idle transition completed work, independently of who has viewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    pub revision: u64,
}

/// A question an agent asked the user that is still open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentOpenQuestion {
    /// Unique within the agent's terminal.
    pub id: u64,
    /// The question in a few words; absent when the agent gave none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Unix milliseconds when it was asked; 0 when unknown.
    pub since_ms: u64,
}

/// The pane metadata token in which an agent's integration reports how many background
/// tasks it had running when its turn ended (Claude Code's Stop hook `herdr-bg-badge`
/// reports it from the hook payload, `0` included); its value is a decimal count.
pub const BACKGROUND_TASKS_TOKEN: &str = "bg_count";

/// The pane metadata token in which an agent's integration reports how many of the
/// background tasks its turn ended with are herdr-job waits with a live record (`herdr-job
/// wait`, `wait-agent`; `herdr-bg-badge` reports it from `herdr-job _waits`, `0` included):
/// waits herdr knows of, which the supervision sweep does not count as untracked shells.
pub const BACKGROUND_WAITS_TOKEN: &str = "bg_waits";

/// The background task count a `bg_count` token value states; None (unknown) when it
/// is not a decimal count.
pub fn background_task_count(value: &str) -> Option<u32> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
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

/// An agent that runs an older version of its CLI than the one installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentRestartPending {
    /// What its launcher's `--version` printed when the agent started.
    pub running_version: String,
    /// What it prints now.
    pub installed_version: String,
}

/// The input context of an agent's last completed request: how full its context window was
/// as of that request. Not cost, and not cumulative over the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentContext {
    /// The request's input tokens: for Claude its `input_tokens` plus
    /// `cache_creation_input_tokens` plus `cache_read_input_tokens`; output excluded.
    pub last_request_input_tokens: u64,
    /// The agent session the request belongs to.
    pub session_id: String,
    /// The transcript message the reading came from (Claude's `message.id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// When the transcript recorded that message (RFC 3339, as the agent wrote it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}

/// Reads the assignment herdr registered when it typed a prompt (`agent.prompt` and its
/// variants) and the verdict it recorded for it: the `WORKER-DONE`/`WORKER-BLOCKED` line the
/// agent's work ended on, read from its transcript at its turn end. Assignments are kept on
/// disk, so this answers after a restart or a live handoff too; `prompt_request_not_found`
/// for a request herdr never registered or no longer keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentVerdictParams {
    pub request_id: String,
}

/// Waits until herdr records the verdict of a prompt's assignment (`agent.verdict`), a newer
/// prompt into the same pane supersedes it, the agent asks something (`blocked`, or an open
/// question) or its pane goes away, and answers which. A verdict recorded before the wait, also
/// by the server a live handoff replaced, answers at once. It has no timeout: every outcome is
/// an event, and a request herdr does not know answers `unknown_request` at once. An answer
/// with a verdict is not its delivery: the waiter acknowledges it with `agent.ack_verdict`,
/// sent as the next request on the same connection (the server reads it there until the
/// client closes) or on its own; until then the verdict counts as undelivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentWaitVerdictParams {
    pub request_id: String,
}

/// Acknowledges the verdict of a prompt's assignment that an `agent.wait_verdict` answered
/// with: from then on it counts as delivered (`delivered_ms`). Answers the assignment; a
/// repeated acknowledgement answers it as it is. `prompt_request_not_found` for a request herdr
/// does not keep, `verdict_not_recorded` for one without a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentAckVerdictParams {
    pub request_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentVerdictKind {
    /// `WORKER-DONE <sha> ...`.
    Done,
    /// `WORKER-BLOCKED <reason>`.
    Blocked,
    /// Any kind this client does not know.
    #[serde(other)]
    Unknown,
}

/// The `WORKER` line an assignment's work ended on, as herdr recorded it once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentVerdictInfo {
    pub kind: AgentVerdictKind,
    /// The line, from `WORKER-DONE` or `WORKER-BLOCKED` on.
    pub line: String,
    /// The commit a `WORKER-DONE` line names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// When herdr recorded it, Unix milliseconds.
    pub recorded_ms: u64,
}

/// A prompt herdr typed into a pane's agent, with the verdict its work ended on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentAssignmentInfo {
    pub request_id: String,
    /// The pane the prompt was typed into.
    pub pane_id: String,
    /// The agent's name in herdr when the prompt was typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// When herdr accepted the prompt, Unix milliseconds.
    pub accepted_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<AgentVerdictInfo>,
    /// The newer prompt into the same pane that started before this assignment's verdict and
    /// closed it: this assignment never takes that work's verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// When an `agent.wait_verdict` first answered with the verdict, Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_ms: Option<u64>,
    /// When the waiter acknowledged the verdict (`agent.ack_verdict`), Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_ms: Option<u64>,
    /// Why the prompt is a message, not a worker's assignment (a note, a message to a
    /// coordinator, not the sender's worker): no verdict is awaited for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Why `agent.wait_verdict` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentVerdictWaitReason {
    /// Herdr recorded the assignment's verdict; see `assignment.verdict`.
    Verdict,
    /// A newer prompt into the same pane started before the verdict and closed this
    /// assignment; see `assignment.superseded_by`, whose verdict to wait for instead.
    Superseded,
    /// The agent asks something: a dialog (`blocked`) or an open question; see `question`.
    Question,
    /// The agent exited or its pane closed without a verdict.
    Exited,
    /// The prompt is a message, not a worker's assignment: no verdict is awaited; see
    /// `assignment.message`.
    Message,
    /// Herdr never registered this request or no longer keeps it.
    UnknownRequest,
    /// Any reason this client does not know.
    #[serde(other)]
    Unknown,
}
