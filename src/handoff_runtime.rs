#[cfg(unix)]
use serde::{Deserialize, Serialize};

/// Long-lived pane runtime transferred during server replacement.
///
/// Handoff preserves server-owned session state such as PTYs, processes, agent
/// identity, and durable plugin/session metadata. It intentionally does not
/// preserve transient coordination such as in-flight requests, waits,
/// subscriptions, client sockets, or pane-to-pane messages; clients reconnect
/// and retry those operations after replacement.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
    /// Contents and cursor state of the alternate screen when it is active, so
    /// the new server shows a full-screen program without waiting for it to
    /// redraw. Absent from older senders and from oversized screens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_screen_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<crate::terminal::state::HandoffAgentState>,
    /// The agent's report that its turn ended with a question for the user
    /// (the `?` mark). Separate from `agent_state`, which only agents with full
    /// lifecycle hooks have; Claude and Pi report just this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting_reply_reported: bool,
    /// The agent's reported task (`pane.report_task`); absent from older senders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_task: Option<String>,
    /// The question reported with `awaiting_reply_reported`; absent from older senders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_reply_question: Option<String>,
    /// Unix milliseconds of the awaiting-reply report; absent from older senders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_reply_since_ms: Option<u64>,
    /// The limit the agent's last turn ended on (`pane.report_limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_report: Option<crate::terminal::state::LimitReport>,
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: std::os::fd::RawFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}
