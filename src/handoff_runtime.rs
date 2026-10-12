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
    /// Every open question, oldest first; the `awaiting_reply_*` fields above carry the
    /// newest one for older receivers. Absent from older senders, which carry one at most.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<HandoffOpenQuestion>,
    /// The prompts herdr typed into the agent, followed through its turn reports, and which
    /// agent reports turns here, so `agent.prompt_status` and the waits on a request typed
    /// before the handoff go on. Absent from older senders, whose requests the new server does
    /// not know (`prompt_request_not_found`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_turns: Option<crate::terminal::prompt_turns::PromptTurns>,
}

/// An open question as a live handoff carries it.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HandoffOpenQuestion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default)]
    pub since_ms: u64,
}

#[cfg(unix)]
impl HandoffRuntimeState {
    /// The open questions this state carries: every one from a current sender, the single
    /// report from an older one.
    pub(crate) fn carried_open_questions(&self) -> Vec<(Option<String>, u64)> {
        if !self.open_questions.is_empty() {
            return self
                .open_questions
                .iter()
                .map(|open| (open.question.clone(), open.since_ms))
                .collect();
        }
        if self.awaiting_reply_reported {
            return vec![(
                self.awaiting_reply_question.clone(),
                self.awaiting_reply_since_ms.unwrap_or(0),
            )];
        }
        Vec::new()
    }
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_followed_prompts_are_optional_in_a_handoff_state() {
        let mut state: HandoffRuntimeState = serde_json::from_value(serde_json::json!({
            "pane_id": 1, "child_pid": 2, "rows": 24, "cols": 80,
            "cell_width_px": 0, "cell_height_px": 0
        }))
        .unwrap();
        // An older sender's state carries none.
        assert_eq!(state.prompt_turns, None);
        let mut turns = crate::terminal::prompt_turns::PromptTurns::default();
        turns.mark_reporting("claude");
        turns.accept("p1".into(), "do the item");
        state.prompt_turns = Some(turns.clone());
        let state: HandoffRuntimeState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(state.prompt_turns, Some(turns));
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: std::os::fd::RawFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}
