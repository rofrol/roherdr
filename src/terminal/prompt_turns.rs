//! Prompts herdr typed into an agent, followed through the agent's own turn reports
//! (`pane.report_turn`), so a caller can wait for the turn its prompt started instead of
//! whichever turn ends next. Only an agent whose integration reports turns is followed; the
//! screen never decides that a turn started or ended.

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
}

#[derive(Debug, Clone)]
struct PromptRequest {
    id: String,
    text: String,
    state: PromptTurnState,
}

#[derive(Debug, Clone, Default)]
pub struct PromptTurns {
    /// The agent label (`claude`, `pi`) whose integration reported turns in this terminal.
    reporting_agent: Option<String>,
    requests: VecDeque<PromptRequest>,
}

impl PromptTurns {
    /// Records that `agent`'s integration reports turns here.
    pub fn mark_reporting(&mut self, agent: &str) {
        if self.reporting_agent.as_deref() != Some(agent) {
            self.reporting_agent = Some(agent.to_string());
            self.requests.clear();
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
                .position(|request| request.state == PromptTurnState::Finished)
                .unwrap_or(0);
            self.requests.remove(oldest_finished);
        }
        self.requests.push_back(PromptRequest {
            id,
            text: normalize(text),
            state: PromptTurnState::Accepted,
        });
    }

    /// A turn started with `prompt`: the oldest accepted request whose text it carries is now
    /// working. A turn started by anything else (the user typing) changes nothing. Returns
    /// whether a request changed.
    pub fn turn_started(&mut self, prompt: Option<&str>) -> bool {
        let Some(prompt) = prompt.map(normalize).filter(|prompt| !prompt.is_empty()) else {
            return false;
        };
        let Some(request) = self.requests.iter_mut().find(|request| {
            request.state == PromptTurnState::Accepted
                && !request.text.is_empty()
                && (prompt == request.text || prompt.contains(&request.text))
        }) else {
            return false;
        };
        request.state = PromptTurnState::Working;
        true
    }

    /// The turn ended: every working request is finished. A turn that was already running when
    /// a prompt was typed has no working request of that prompt, so it finishes nothing new.
    pub fn turn_finished(&mut self) -> bool {
        let mut changed = false;
        for request in &mut self.requests {
            if request.state == PromptTurnState::Working {
                request.state = PromptTurnState::Finished;
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
        assert!(turns.turn_started(Some("  fix the\ntest")));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Working));
        assert!(turns.turn_finished());
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn a_turn_running_before_the_prompt_does_not_finish_it() {
        let mut turns = following("claude");
        // The user's own turn is running when herdr types the prompt.
        assert!(!turns.turn_started(Some("something the user typed")));
        turns.accept("p1".into(), "review the diff");
        assert!(!turns.turn_finished());
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Accepted));
        // The queued prompt runs next.
        assert!(turns.turn_started(Some("review the diff")));
        assert!(turns.turn_finished());
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Finished));
    }

    #[test]
    fn a_turn_started_by_other_text_leaves_the_request_accepted() {
        let mut turns = following("pi");
        turns.accept("p1".into(), "run the tests");
        assert!(!turns.turn_started(Some("hello")));
        assert!(!turns.turn_started(None));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Accepted));
    }

    #[test]
    fn requests_start_in_the_order_they_were_typed() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "same");
        turns.accept("p2".into(), "same");
        assert!(turns.turn_started(Some("same")));
        assert_eq!(turns.state_of("p1"), Some(PromptTurnState::Working));
        assert_eq!(turns.state_of("p2"), Some(PromptTurnState::Accepted));
    }

    #[test]
    fn another_reporting_agent_drops_the_old_requests() {
        let mut turns = following("claude");
        turns.accept("p1".into(), "x");
        turns.mark_reporting("claude");
        assert!(turns.state_of("p1").is_some());
        turns.mark_reporting("pi");
        assert_eq!(turns.state_of("p1"), None);
    }

    #[test]
    fn the_oldest_finished_request_goes_first_when_full() {
        let mut turns = following("claude");
        turns.accept("first".into(), "a");
        turns.turn_started(Some("a"));
        turns.turn_finished();
        for index in 0..MAX_REQUESTS {
            turns.accept(format!("p{index}"), "b");
        }
        assert_eq!(turns.state_of("first"), None);
        assert_eq!(turns.state_of("p0"), Some(PromptTurnState::Accepted));
    }
}
