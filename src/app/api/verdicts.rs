//! Assignments and their verdicts (`crate::assignments`): registered when herdr accepts a
//! prompt, recorded when the pane's agent reports a turn end, read with `agent.verdict`.

use std::path::PathBuf;

use crate::app::App;
use crate::terminal::prompt_turns::PromptTurnState;

use super::responses::{encode_error, encode_success};

impl App {
    /// The transcript of the pane's agent now: the path its integration reported, else the
    /// one its session names.
    fn pane_transcript(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> Option<PathBuf> {
        let pane = self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        crate::assignments::transcript_of(
            terminal.prompt_turns.transcript(),
            crate::app::creation::terminal_agent_session_info(terminal).as_ref(),
        )
    }

    /// Registers a prompt herdr accepted for the pane's agent, anchored at the end of the
    /// agent's transcript now, owned by `caller_pane` (the coordinator that sent it). A note,
    /// or a prompt that is not from a coordinator to its worker, is registered as a message
    /// ([`Self::prompt_message_reason`]): closed at once, awaiting no verdict.
    pub(super) fn register_assignment(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        request_id: &str,
        text: &str,
        caller_pane: Option<&str>,
        note: bool,
    ) {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        let terminal = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id));
        let agent = terminal.and_then(|terminal| terminal.effective_agent_label());
        let worker = terminal.and_then(|terminal| terminal.agent_name.as_deref());
        let transcript = self.pane_transcript(ws_idx, pane_id);
        let owner = caller_pane
            .filter(|pane| !pane.trim().is_empty())
            .map(|pane| self.canonical_pane_id(pane));
        let message = self.prompt_message_reason(ws_idx, pane_id, &public, owner.as_deref(), note);
        crate::assignments::accepted_as(
            request_id,
            &public,
            agent,
            worker,
            owner.as_deref(),
            text,
            transcript.as_deref(),
            message,
        );
    }

    /// Why a prompt into `target` (the public id of `pane_id`) from `owner` is a message rather
    /// than a worker's assignment, or `None` when it is one. Only a prompt from a coordination
    /// tenure's pane to its worker is an assignment: a worker it started
    /// ([`Self::register_started_worker`]), or one registered to it, which a worker-role tab
    /// nobody owns becomes when its first coordinator prompts it. A note, a prompt into a
    /// coordinator's pane and a coordinator's prompt to a pane that is not its worker are
    /// messages. A prompt sent from a pane without a tenure stays followed for its verdict, as
    /// before; no coordinator owns it, so supervision ignores it.
    fn prompt_message_reason(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        target: &str,
        owner: Option<&str>,
        note: bool,
    ) -> Option<String> {
        if note {
            return Some("a note (`agent prompt --note`)".into());
        }
        let coordinators = crate::workers::coordinators()?;
        if let Some(tenure) = coordinators.coordinator_of_pane(target) {
            return Some(format!(
                "a message to coordinator {} (its pane {target})",
                tenure.coordinator_id
            ));
        }
        let sender = coordinators.coordinator_of_pane(owner?)?;
        match crate::assignments::worker_pane_owner(target) {
            Some(worker_owner) if worker_owner.repo == sender.repo => None,
            Some(worker_owner) => Some(format!(
                "a message from coordinator {} to a worker of coordinator {}",
                sender.coordinator_id, worker_owner.coordinator_id
            )),
            None if self.pane_tab_role(ws_idx, pane_id)
                == Some(crate::api::schema::TabRole::Worker) =>
            {
                crate::assignments::register_worker_pane(
                    target,
                    Some(crate::assignments::WorkerPaneOwner {
                        coordinator_id: sender.coordinator_id,
                        repo: sender.repo,
                    }),
                );
                None
            }
            None => Some(format!(
                "a message from coordinator {} to pane {target}, which is not its worker",
                sender.coordinator_id
            )),
        }
    }

    /// The role of the tab that holds `pane_id`.
    fn pane_tab_role(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::TabRole> {
        self.state
            .workspaces
            .get(ws_idx)?
            .tabs
            .iter()
            .find(|tab| tab.panes.contains_key(&pane_id))?
            .role
    }

    /// An agent was started in `pane_id` by `caller_pane`: it is a worker of the coordination
    /// tenure that pane holds, else of nobody (a stale owner of the pane is forgotten).
    pub(super) fn register_started_worker(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        caller_pane: Option<&str>,
    ) {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        let Some(coordinators) = crate::workers::coordinators() else {
            return;
        };
        let owner = caller_pane
            .filter(|pane| !pane.trim().is_empty())
            .map(|pane| self.canonical_pane_id(pane))
            .and_then(|pane| coordinators.coordinator_of_pane(&pane))
            .map(|tenure| crate::assignments::WorkerPaneOwner {
                coordinator_id: tenure.coordinator_id,
                repo: tenure.repo,
            });
        crate::assignments::register_worker_pane(&public, owner);
    }

    /// The pane's agent reported a turn end: reads its transcript once for the pane's open
    /// assignments, records each verdict found (once), closes those a newer prompt superseded,
    /// and marks a prompt the agent took in mid-turn as worked on by this turn, so its end ends
    /// it. `final_text` is the turn's last message the agent reported with the end, which the
    /// transcript may not show yet; it is also the verdict of an assignment whose prompt the
    /// agent reported starting this turn when the transcript does not show that prompt yet.
    /// When `turn_closed` (no background task left to resume the agent), a started open
    /// assignment the turn brought no verdict is marked stopped without one, and every other
    /// one still open is marked left by a closed turn ([`Self::account_closed_turn`]), with
    /// what the transcript showed: a missing or unreadable transcript is evidence too, never
    /// silence. A verdict that no `agent.wait_verdict` waits for, and a stop without a verdict,
    /// ask for a supervision sweep. Returns whether a followed request changed.
    pub(super) fn record_turn_end_verdicts(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        turn_closed: bool,
        final_text: Option<&str>,
    ) -> bool {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return false;
        };
        let (changed, transcript) =
            self.read_turn_end_verdicts(ws_idx, pane_id, &public, turn_closed, final_text);
        if turn_closed {
            self.account_closed_turn(
                ws_idx,
                pane_id,
                &format!("a turn end with no background task left; transcript: {transcript}"),
            );
        }
        changed
    }

    /// The transcript read of [`Self::record_turn_end_verdicts`]: whether a followed request
    /// changed, and what the read showed of the assignments it left open.
    fn read_turn_end_verdicts(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        public: &str,
        turn_closed: bool,
        final_text: Option<&str>,
    ) -> (bool, String) {
        let open = crate::assignments::open_for_pane(public);
        if open.is_empty() {
            return (false, "not read, no open assignment".into());
        }
        let Some(path) = self.pane_transcript(ws_idx, pane_id).or_else(|| {
            open.iter()
                .rev()
                .find_map(|assignment| assignment.transcript.clone().map(PathBuf::from))
        }) else {
            return (
                false,
                "none known (the agent reported no session or transcript)".into(),
            );
        };
        for assignment in &open {
            crate::assignments::note_transcript(&assignment.request_id, &path);
        }
        let now_ms = crate::assignments::now_ms();
        let mut scan = match crate::assignments::scan_open(&path, &open, true, final_text, now_ms) {
            Ok(scan) => scan,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot read the agent's transcript");
                return (false, format!("cannot read {}: {error}", path.display()));
            }
        };
        let mut changed = false;
        if let Some(terminal) = self.pane_terminal_mut(ws_idx, pane_id) {
            for (request_id, absorbed) in &scan.started {
                if *absorbed
                    && terminal.prompt_turns.state_of(request_id) == Some(PromptTurnState::Accepted)
                {
                    changed |= terminal.prompt_turns.absorbed(request_id);
                }
            }
            // The agent reported starting this turn with the prompt (`UserPromptSubmit`), but
            // the transcript does not show it yet: the turn's last message is its verdict.
            let verdict = final_text.and_then(|text| crate::assignments::worker_line(text, now_ms));
            for assignment in &open {
                let id = &assignment.request_id;
                if scan.started.contains_key(id)
                    || scan.superseded.contains_key(id)
                    || terminal.prompt_turns.state_of(id) != Some(PromptTurnState::Working)
                {
                    continue;
                }
                scan.started.insert(id.clone(), false);
                if let Some(verdict) = &verdict {
                    scan.verdicts.insert(id.clone(), verdict.clone());
                }
            }
        }
        for request_id in crate::assignments::apply(&path, &scan) {
            if !crate::assignments::has_waiter(&request_id) {
                crate::workers::supervision::request_sweep(&format!(
                    "verdict of {request_id} in pane {public}"
                ));
            }
        }
        if turn_closed {
            // Started now or at an earlier turn end, still open after this one.
            let stopped = crate::assignments::open_for_pane(public)
                .into_iter()
                .filter(|assignment| {
                    assignment.started_ms.is_some()
                        || scan.started.contains_key(&assignment.request_id)
                })
                .filter(|assignment| {
                    crate::assignments::stopped_without_verdict(&assignment.request_id)
                })
                .count();
            if stopped > 0 {
                crate::workers::supervision::request_sweep(&format!(
                    "a turn end in pane {public} without a verdict"
                ));
            }
        }
        (
            changed,
            format!(
                "{} read; no start of the prompt found after its anchor",
                path.display()
            ),
        )
    }

    /// A turn of the pane's agent closed (no background task left to resume it, or an error
    /// ended it) after herdr accepted each of its open assignments, which herdr processed in
    /// that order: each one neither a verdict, a supersession nor a stop closed is marked with
    /// `what` happened and the prompt's state ([`crate::assignments::turn_closed_after`]). A
    /// prompt still queued (typed, its turn not started) is marked queued: the closed turn was
    /// the one before it. A new mark asks for a supervision sweep, where it is the
    /// `unaccounted_assignment` finding unless the pane asks a question.
    pub(super) fn account_closed_turn(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        what: &str,
    ) {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        let terminal = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id));
        let mut marked = false;
        for assignment in crate::assignments::open_for_pane(&public) {
            if assignment.stopped_ms.is_some() {
                continue;
            }
            let state = terminal
                .and_then(|terminal| terminal.prompt_turns.state_of(&assignment.request_id));
            let queued = state == Some(PromptTurnState::Accepted);
            let state = state.map_or_else(
                || "not followed by this server".to_string(),
                |state| format!("{state:?}").to_lowercase(),
            );
            marked |= crate::assignments::turn_closed_after(
                &assignment.request_id,
                format!("{what}; prompt state: {state}"),
                queued,
            );
        }
        if marked {
            crate::workers::supervision::request_sweep(&format!(
                "a closed turn in pane {public} left an assignment open"
            ));
        }
    }

    /// A turn of the pane's agent started: its open assignments stopped without a verdict are
    /// worked on again, and a finding about them resolves at the sweep this asks for.
    pub(super) fn assignments_turn_started(&self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        if crate::assignments::turn_started(&public) {
            crate::workers::supervision::request_sweep(&format!("a turn start in pane {public}"));
        }
    }

    pub(super) fn handle_agent_ack_verdict(
        &mut self,
        id: String,
        params: crate::api::schema::AgentAckVerdictParams,
    ) -> String {
        crate::assignments::ack_response(id, &params.request_id)
    }

    pub(super) fn handle_agent_verdict(
        &mut self,
        id: String,
        params: crate::api::schema::AgentVerdictParams,
    ) -> String {
        match crate::assignments::get(&params.request_id) {
            Some(assignment) => encode_success(
                id,
                crate::api::schema::ResponseResult::AgentAssignment {
                    assignment: crate::assignments::info(&assignment),
                },
            ),
            None => encode_error(
                id,
                "prompt_request_not_found",
                format!(
                    "herdr keeps no assignment {}; it never typed that prompt, or the record \
                     is gone",
                    params.request_id
                ),
            ),
        }
    }
}
