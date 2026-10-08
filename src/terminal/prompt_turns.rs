//! Prompts herdr typed into an agent, followed through the agent's own turn reports
//! (`pane.report_turn`), so a caller can wait for the turn its prompt started instead of
//! whichever turn ends next. Only an agent whose integration reports turns is followed; the
//! screen never decides that a turn started or ended. A turn ends with the agent's report
//! (finished or failed), with the agent's process (exited), or with the next turn starting before
//! that report. That last case is `interrupted` only on an explicit signal: herdr sent the agent
//! an interrupt key (Esc, Ctrl-C) while the turn worked, or the agent reported the end as
//! interrupted. Claude reports no `Stop` for a turn the user ends with Esc, but it also starts a
//! turn of its own for a queued message, so without a signal the outcome is `unknown`. A turn
//! the integration marks as a continuation (a background task's notification inside the
//! prompt's work) ends nothing: the prompt's turn ends at the end report that follows.

use std::collections::VecDeque;

/// Requests kept per terminal; the oldest finished ones go first.
const MAX_REQUESTS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTurnState {
    /// Typed into the agent; its turn has not started yet.
    Accepted,
    /// The agent reported a turn started by this prompt.
    Working,
    /// That turn ended.
    Finished,
    /// That turn ended on an error the agent reported (Claude's `StopFailure`).
    Failed,
    /// The turn was interrupted: herdr sent an interrupt key while it worked and another turn
    /// started, or the agent reported its end as interrupted.
    Interrupted,
    /// Another turn started before this one reported its end, with no signal saying why: the
    /// user may have interrupted it, or the agent took a queued message into its work.
    Unknown,
    /// The agent's process ended, or another agent took over its reports, before the turn ended.
    Exited,
}

impl PromptTurnState {
    /// The request's turn is over; nothing changes it again.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Accepted | Self::Working)
    }
}

#[derive(Debug, Clone)]
struct PromptRequest {
    id: String,
    text: String,
    state: PromptTurnState,
    /// The error a failed turn ended on.
    error: Option<String>,
    /// Herdr sent the agent an interrupt key while this request's turn worked.
    interrupt_sent: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PromptTurns {
    /// The agent label (`claude`, `pi`) whose integration reported turns in this terminal.
    reporting_agent: Option<String>,
    requests: VecDeque<PromptRequest>,
}

impl PromptTurns {
    /// Records that `agent`'s integration reports turns here. Another agent's reports end the
    /// open requests of the agent before it.
    pub fn mark_reporting(&mut self, agent: &str) {
        if self.reporting_agent.as_deref() != Some(agent) {
            self.reporting_agent = Some(agent.to_string());
            self.agent_exited();
        }
    }

    /// Whether the agent running now (`agent`, its effective label) reports its turns.
    pub fn supported_for(&self, agent: Option<&str>) -> bool {
        agent.is_some() && self.reporting_agent.as_deref() == agent
    }

    /// Starts following a prompt herdr is about to type.
    pub fn accept(&mut self, id: String, text: &str) {
        if self.requests.len() >= MAX_REQUESTS {
            let oldest_finished = self
                .requests
                .iter()
                .position(|request| request.state.is_terminal())
                .unwrap_or(0);
            self.requests.remove(oldest_finished);
        }
        self.requests.push_back(PromptRequest {
            id,
            text: normalize(text),
            state: PromptTurnState::Accepted,
            error: None,
            interrupt_sent: false,
        });
    }

    /// A turn started with `prompt`. A `continuation` (a background task's notification) goes on
    /// with the working request's work and changes nothing. Otherwise a request still working
    /// ended without its report: interrupted when herdr sent it an interrupt key, else unknown.
    /// The oldest accepted request whose text `prompt` carries is now working; a turn started
    /// by anything else (the user typing) starts no request. Returns whether a request changed.
    pub fn turn_started(&mut self, prompt: Option<&str>, continuation: bool) -> bool {
        if continuation {
            return false;
        }
        let mut interrupted = false;
        for request in &mut self.requests {
            if request.state == PromptTurnState::Working {
                request.state = if request.interrupt_sent {
                    PromptTurnState::Interrupted
                } else {
                    PromptTurnState::Unknown
                };
                interrupted = true;
            }
        }
        let Some(prompt) = prompt.map(normalize).filter(|prompt| !prompt.is_empty()) else {
            return interrupted;
        };
        let Some(request) = self.requests.iter_mut().find(|request| {
            request.state == PromptTurnState::Accepted
                && !request.text.is_empty()
                && (prompt == request.text || prompt.contains(&request.text))
        }) else {
            return interrupted;
        };
        request.state = PromptTurnState::Working;
        true
    }

    /// The turn ended, on `error` when the agent reported one: every working request is
    /// finished, failed, or interrupted when the agent `interrupted` says so. A turn that was
    /// already running when a prompt was typed has no working request of that prompt, so it
    /// finishes nothing new.
    pub fn turn_finished(&mut self, error: Option<&str>, interrupted: bool) -> bool {
        let error = error.map(str::trim).filter(|error| !error.is_empty());
        let mut changed = false;
        for request in &mut self.requests {
            if request.state == PromptTurnState::Working {
                request.state = if interrupted {
                    PromptTurnState::Interrupted
                } else if error.is_some() {
                    PromptTurnState::Failed
                } else {
                    PromptTurnState::Finished
                };
                request.error = error.map(str::to_string);
                changed = true;
            }
        }
        changed
    }

    /// Herdr sent the agent an interrupt key: a working request whose turn the next turn's
    /// start cuts off was interrupted. Its state stays `working` until then.
    pub fn interrupt_sent(&mut self) {
        for request in &mut self.requests {
            if request.state == PromptTurnState::Working {
                request.interrupt_sent = true;
            }
        }
    }

    /// The agent's process ended: its accepted and working requests will never finish.
    pub fn agent_exited(&mut self) -> bool {
        self.end_open(
            &[PromptTurnState::Accepted, PromptTurnState::Working],
            PromptTurnState::Exited,
        )
    }

    fn end_open(&mut self, from: &[PromptTurnState], to: PromptTurnState) -> bool {
        let mut changed = false;
        for request in &mut self.requests {
            if from.contains(&request.state) {
                request.state = to;
                changed = true;
            }
        }
        changed
    }

    pub fn state_of(&self, id: &str) -> Option<PromptTurnState> {
        self.requests
            .iter()
            .find(|request| request.id == id)
            .map(|request| request.state)
    }

    /// The error a failed request's turn ended on.
    pub fn error_of(&self, id: &str) -> Option<&str> {
        self.requests
            .iter()
            .find(|request| request.id == id)
            .and_then(|request| request.error.as_deref())
    }
}

/// Whitespace runs collapse to one space: the agent may hand the prompt back with other line
/// endings or trimmed.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn following(agent: &str) -> PromptTurns {
        let mut turns = PromptTurns::default();
        turns.mark_reporting(agent);
        turns
    }

    #[test]
    fn supported_only_for_the_agent_that_reported_turns() {
        let turns = following("claude");
        assert!(turns.supported_for(Some("claude")));
        assert!(!turns.supported_for(Some("codex")));
        assert!(!turns.supported_for(None));
        assert!(!PromptTurns::default().supported_for(Some("claude")));
    }

    #[test]
    fn a_request_follows_accepted_working_finished() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "fix the test\r\n");
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Accepted));
        assert!(turns.turn_started(Some("  fix the\ntest"), false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Working));
        assert!(turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn a_turn_running_before_the_prompt_does_not_finish_it() {
        let mut turns = following("claude");
        // The user's own turn is running when herdr types the prompt.
        assert!(!turns.turn_started(Some("something the user typed"), false));
        turns.accept("p1".into(), "review the diff");
        assert!(!turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Accepted));
        // The queued prompt runs next.
        assert!(turns.turn_started(Some("review the diff"), false));
        assert!(turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn a_turn_started_by_other_text_leaves_the_request_accepted() {
        let mut turns = following("pi");
        turns.accept("p1".into(), "run the tests");
        assert!(!turns.turn_started(Some("hello"), false));
        assert!(!turns.turn_started(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Accepted));
    }

    #[test]
    fn requests_start_in_the_order_they_were_typed() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "same");
        turns.accept("p2".into(), "same");
        assert!(turns.turn_started(Some("same"), false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Working));
        assert_eq!(turns.state_of("p2"), Some(PromptTurnState::Accepted));
    }

    #[test]
    fn another_reporting_agent_ends_the_old_requests_as_exited() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.mark_reporting("claude");
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Accepted));
        turns.mark_reporting("pi");
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Exited));
    }

    #[test]
    fn a_turn_ending_on_an_error_fails_the_request_with_it() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.turn_started(Some("x"), false);
        assert!(turns.turn_finished(Some(" server_error: overloaded "), false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Failed));
        assert_eq!(turns.error_of("p1"), Some("server_error: overloaded"));
    }

    #[test]
    fn a_turn_starting_before_the_last_one_ended_without_a_signal_is_unknown() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.turn_started(Some("x"), false);
        // Either the user pressed Esc (no `Stop`) and typed something else, or Claude took a
        // queued message into its work: nothing tells which.
        assert!(turns.turn_started(Some("never mind"), false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Unknown));
        // The late end of another turn does not change it.
        assert!(!turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Unknown));
    }

    #[test]
    fn a_turn_starting_after_an_interrupt_key_interrupts_the_last_one() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.turn_started(Some("x"), false);
        // Herdr sent Esc: the turn ends without its `Stop`, and the next prompt starts a turn.
        turns.interrupt_sent();
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Working));
        assert!(turns.turn_started(Some("never mind"), false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Interrupted));
    }

    #[test]
    fn an_interrupt_key_the_agent_ignored_still_finishes_at_its_end_report() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.turn_started(Some("x"), false);
        turns.interrupt_sent();
        assert!(turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn an_interrupt_key_before_the_turn_started_does_not_mark_it() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.interrupt_sent();
        turns.turn_started(Some("x"), false);
        assert!(turns.turn_started(Some("next"), false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Unknown));
    }

    #[test]
    fn the_agent_reporting_an_interrupted_end_interrupts_the_request() {
        let mut turns = following("pi");
        turns.accept("p1".into(), "x");
        turns.turn_started(Some("x"), false);
        assert!(turns.turn_finished(None, true));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Interrupted));
    }

    #[test]
    fn a_background_task_notification_turn_continues_the_prompts_work() {
        // The reported bug: Claude starts a turn for a background task's notification while
        // the prompt's turn works, and the prompt's work ends at the `Stop` that follows.
        let mut turns = following("claude");
        turns.accept("p1".into(), "run the build in the background and report");
        assert!(turns.turn_started(Some("run the build in the background and report"), false));
        let notification = "<task-notification><tool-use-id>toolu_1</tool-use-id>\
                            <status>completed</status></task-notification>";
        assert!(!turns.turn_started(Some(notification), true));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Working));
        assert!(turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn a_notification_turn_after_the_end_changes_nothing() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.turn_started(Some("x"), false);
        turns.turn_finished(None, false);
        assert!(!turns.turn_started(Some("<task-notification>"), true));
        assert!(!turns.turn_finished(None, false));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn the_agent_exiting_ends_accepted_and_working_requests() {
        let mut turns = following("claude");
        turns.accept("done".into(), "a");
        turns.turn_started(Some("a"), false);
        turns.turn_finished(None, false);
        turns.accept("working".into(), "b");
        turns.turn_started(Some("b"), false);
        turns.accept("accepted".into(), "c");
        assert!(turns.agent_exited());
        assert_eq!(turns.state_of("done"), Some(PromptTurnState::Finished));
        assert_eq!(turns.state_of("working"), Some(PromptTurnState::Exited));
        assert_eq!(turns.state_of("accepted"), Some(PromptTurnState::Exited));
        assert!(!turns.agent_exited());
    }

    #[test]
    fn the_oldest_finished_request_goes_first_when_full() {
        let mut turns = following("claude");
        turns.accept("first".into(), "a");
        turns.turn_started(Some("a"), false);
        turns.turn_finished(None, false);
        for index in 0..MAX_REQUESTS {
            turns.accept(format!("p{index}"), "b");
        }
        assert_eq!(turns.state_of("first"), None);
        assert_eq!(turns.state_of("p0"), Some(PromptTurnState::Accepted));
    }
}
