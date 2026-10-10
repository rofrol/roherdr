//! A headless worker's question opened as a dialog: a click on its row in
//! the `?` list, or on its worker's sidebar line, opens it, bound to the
//! question's request id, not to the worker.
//!
//! The dialog asks the server for the question with the tool's whole input
//! (`worker.question`) when it opens, so an approval never rests on the
//! one-line preview the snapshot carries. What happens to the question
//! while it is open (answered elsewhere, the worker exited) comes from the
//! snapshot the client receives anyway; nothing is fetched in the
//! background. Its actions send `worker.answer` or `worker.deny_and_stop`,
//! one request per click, and no key acts without a button being chosen:
//! Enter presses only the button Tab moved to.

use crate::api::schema::{
    Method, PaneTarget, ResponseResult, WorkerAnswerParams, WorkerDecision,
    WorkerDenyAndStopParams, WorkerQuestionDetail, WorkerQuestionKind, WorkerQuestionTarget,
    WorkerTarget,
};

use crossterm::event::{MouseButton, MouseEventKind};

use super::*;

/// A worker question's dialog.
#[derive(Debug)]
pub(super) struct WorkerQuestionDialog {
    /// The machine the question belongs to.
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) worker_id: String,
    pub(super) request_id: String,
    /// The question as `worker.question` returned it; none until it
    /// answers.
    pub(super) detail: Option<WorkerQuestionDetail>,
    /// `worker.question` is in flight.
    pub(super) loading: bool,
    /// The server cannot send the whole input (an older server).
    pub(super) unsupported: bool,
    /// Why the server would not show the question: it is no longer pending,
    /// as `worker_question_gone` says it.
    pub(super) refused: Option<String>,
    /// The action sent and not answered yet.
    pub(super) sending: Option<WorkerQuestionAction>,
    /// The last action's refusal.
    pub(super) failure: Option<String>,
    /// Rows of the input scrolled past.
    pub(super) scroll: usize,
    /// Deny's message field, open after Deny until it is sent or cancelled.
    pub(super) deny_message: Option<TextEditor>,
    /// Stop worker was pressed: its confirmation shows.
    pub(super) confirm_stop: bool,
    /// The options picked for each `AskUserQuestion` question.
    pub(super) picks: Vec<Vec<bool>>,
    /// The button Tab moved to, which Enter presses; none when it opens.
    pub(super) focused: Option<WorkerQuestionButton>,
    /// Requests this dialog answered or whose worker it stopped: moving on
    /// skips them while the snapshot still lists them.
    pub(super) done: Vec<String>,
}

/// What the dialog sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerQuestionAction {
    Allow,
    Deny,
    Answer,
    Stop,
}

/// A button of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkerQuestionButton {
    Allow,
    Deny,
    /// Sends the picked options of an `AskUserQuestion`.
    SendAnswer,
    /// An option of an `AskUserQuestion`'s question: (question, option).
    Pick(usize, usize),
    ViewLog,
    Stop,
    OpenOwner,
    Previous,
    Next,
    Close,
    SendDeny,
    CancelDeny,
    ConfirmStop,
    CancelStop,
}

/// Where the question stands, as the dialog says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkerQuestionStatus {
    /// `worker.question` has not answered yet.
    Loading,
    /// Pending; with the whole input once loaded.
    Open,
    /// An action is on its way.
    Sending(WorkerQuestionAction),
    /// It is no longer pending: answered elsewhere, withdrawn or ended.
    Gone(String),
    /// Its worker ended.
    WorkerEnded,
}

/// One row of the dialog's scrolled part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkerQuestionLine {
    /// A line of the input, as written.
    Text(String),
    /// An `AskUserQuestion` question's text.
    Heading(String),
    /// An option: its button, label and whether it is picked.
    Option {
        button: WorkerQuestionButton,
        label: String,
        picked: bool,
        multi: bool,
    },
}

/// Everything the dialog draws, computed from its state and the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerQuestionView {
    /// `worker w12 · fix the login test`.
    pub(super) title: String,
    /// The question's place among the pending ones: (its number, how many).
    pub(super) position: Option<(usize, usize)>,
    /// `herdr · Bash`.
    pub(super) context: String,
    /// Who else can answer it.
    pub(super) owner: String,
    /// Why it asks the user, or that it still waits for its coordinator.
    pub(super) escalation: Option<String>,
    pub(super) lines: Vec<WorkerQuestionLine>,
    pub(super) status: WorkerQuestionStatus,
    /// What the status line says, with whether it warns.
    pub(super) notice: Option<(String, bool)>,
    /// The buttons in order, with whether each can be pressed.
    pub(super) buttons: Vec<(WorkerQuestionButton, &'static str, bool)>,
    pub(super) deny_message: Option<TextEditor>,
    pub(super) confirm_stop: bool,
    pub(super) scroll: usize,
    pub(super) focused: Option<WorkerQuestionButton>,
}

impl WorkerQuestionDialog {
    fn new(endpoint_id: ClientEndpointId, worker_id: String, request_id: String) -> Self {
        Self {
            endpoint_id,
            worker_id,
            request_id,
            detail: None,
            loading: false,
            unsupported: false,
            refused: None,
            sending: None,
            failure: None,
            scroll: 0,
            deny_message: None,
            confirm_stop: false,
            picks: Vec::new(),
            focused: None,
            done: Vec::new(),
        }
    }
}

/// The label of an action's button and of its progress.
fn action_progress(action: WorkerQuestionAction) -> &'static str {
    match action {
        WorkerQuestionAction::Allow => "allowing…",
        WorkerQuestionAction::Deny => "denying…",
        WorkerQuestionAction::Answer => "sending the answer…",
        WorkerQuestionAction::Stop => "denying and stopping the worker…",
    }
}

impl ClientShellState {
    pub(super) fn worker_question_dialog(&self) -> Option<&WorkerQuestionDialog> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::WorkerQuestion(dialog)) => Some(dialog),
            _ => None,
        }
    }

    fn worker_question_dialog_mut(&mut self) -> Option<&mut WorkerQuestionDialog> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::WorkerQuestion(dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// The pending worker questions, the longest waiting first: the order
    /// "1 of 3" counts in.
    fn pending_worker_questions(&self) -> Vec<&crate::protocol::ClientShellWorkerQuestion> {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return Vec::new();
        };
        let mut questions: Vec<_> = snapshot
            .worker_questions
            .iter()
            .filter(|question| !question.request_id.is_empty())
            .collect();
        questions.sort_by_key(|question| question.since_ms);
        questions
    }

    /// Opens the dialog of the worker's question `request_id` and asks the
    /// server for it.
    pub(super) fn open_worker_question(
        &mut self,
        worker_id: String,
        request_id: String,
        outcome: &mut ClientShellInput,
    ) {
        let done = self
            .worker_question_dialog()
            .map(|dialog| dialog.done.clone())
            .unwrap_or_default();
        let mut dialog =
            WorkerQuestionDialog::new(self.active_endpoint_id.clone(), worker_id, request_id);
        dialog.done = done;
        let method = Method::WorkerQuestion(WorkerQuestionTarget {
            worker_id: dialog.worker_id.clone(),
            request_id: dialog.request_id.clone(),
        });
        let kind = PendingEndpointKind::WorkerQuestion {
            endpoint_id: dialog.endpoint_id.clone(),
            worker_id: dialog.worker_id.clone(),
            request_id: dialog.request_id.clone(),
        };
        let supported = self.supports_endpoint_method(&method);
        dialog.unsupported = !supported;
        self.overlay = Some(ClientShellOverlay::WorkerQuestion(Box::new(dialog)));
        outcome.repaint = true;
        if supported && self.push_endpoint_method_with_kind(method, kind, outcome) {
            if let Some(dialog) = self.worker_question_dialog_mut() {
                dialog.loading = true;
            }
        }
    }

    /// The oldest pending question of the worker, for a click on its line.
    pub(super) fn worker_pending_question(&self, worker_id: &str) -> Option<String> {
        self.pending_worker_questions()
            .into_iter()
            .find(|question| question.worker_id == worker_id)
            .map(|question| question.request_id.clone())
    }

    /// `worker.question`'s reply.
    pub(super) fn complete_worker_question(
        &mut self,
        endpoint_id: ClientEndpointId,
        worker_id: String,
        request_id: String,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(dialog) = self.worker_question_dialog_mut().filter(|dialog| {
            dialog.endpoint_id == endpoint_id
                && dialog.worker_id == worker_id
                && dialog.request_id == request_id
        }) else {
            return (false, Vec::new());
        };
        dialog.loading = false;
        match result {
            Ok(ResponseResult::WorkerQuestionDetail { detail }) => {
                dialog.picks = detail
                    .question
                    .questions
                    .iter()
                    .map(|question| vec![false; question.options.len()])
                    .collect();
                dialog.detail = Some(detail);
            }
            Ok(_) => dialog.refused = Some("the server sent something else".to_owned()),
            Err(error) if error.code.as_deref() == Some("unsupported_method") => {
                dialog.unsupported = true;
            }
            Err(error) => dialog.refused = Some(error.message),
        }
        (true, Vec::new())
    }

    /// The reply to an answer or a stop the dialog sent: the next pending
    /// question opens, or the dialog closes when none is left; a refusal
    /// stays in the dialog.
    pub(super) fn complete_worker_question_action(
        &mut self,
        endpoint_id: ClientEndpointId,
        request_id: String,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Some(dialog) = self
            .worker_question_dialog_mut()
            .filter(|dialog| dialog.endpoint_id == endpoint_id && dialog.request_id == request_id)
        else {
            return (false, Vec::new());
        };
        let action = dialog.sending.take();
        if let Err(error) = result {
            dialog.failure = Some(error.message);
            return (true, Vec::new());
        }
        dialog.done.push(request_id);
        if action == Some(WorkerQuestionAction::Stop) {
            // Its other questions end with it.
            let worker_id = dialog.worker_id.clone();
            let others: Vec<String> = self
                .pending_worker_questions()
                .into_iter()
                .filter(|question| question.worker_id == worker_id)
                .map(|question| question.request_id.clone())
                .collect();
            if let Some(dialog) = self.worker_question_dialog_mut() {
                dialog.done.extend(others);
            }
        }
        let mut outcome = ClientShellInput::default();
        match self.next_worker_question(1) {
            Some((worker_id, request_id)) => {
                self.open_worker_question(worker_id, request_id, &mut outcome)
            }
            None => self.overlay = None,
        }
        (true, outcome.actions)
    }

    /// The pending question `step` places after (or before) the open one,
    /// skipping those the dialog handled; none when no other one waits.
    fn next_worker_question(&self, step: isize) -> Option<(String, String)> {
        let dialog = self.worker_question_dialog()?;
        let pending: Vec<_> = self
            .pending_worker_questions()
            .into_iter()
            .filter(|question| {
                question.request_id == dialog.request_id
                    || !dialog.done.contains(&question.request_id)
            })
            .collect();
        let current = pending.iter().position(|question| {
            question.worker_id == dialog.worker_id && question.request_id == dialog.request_id
        });
        let others = || {
            pending
                .iter()
                .filter(|question| question.request_id != dialog.request_id)
        };
        let next = match current {
            Some(index) if pending.len() > 1 => {
                let len = pending.len() as isize;
                let at = (index as isize + step).rem_euclid(len) as usize;
                pending.get(at).copied()
            }
            Some(_) => None,
            // The open one is gone: the first of the others.
            None if step < 0 => others().next_back().copied(),
            None => others().next().copied(),
        }?;
        Some((next.worker_id.clone(), next.request_id.clone()))
    }

    /// Where the open question stands, from the dialog and the snapshot.
    pub(super) fn worker_question_status(&self) -> Option<WorkerQuestionStatus> {
        let dialog = self.worker_question_dialog()?;
        if let Some(action) = dialog.sending {
            return Some(WorkerQuestionStatus::Sending(action));
        }
        let snapshot = self.snapshot.as_deref()?;
        let worker_ended = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == dialog.worker_id)
            .is_none_or(|worker| matches!(worker.state.as_str(), "exited" | "lost"));
        let pending = snapshot.worker_questions.iter().any(|question| {
            question.worker_id == dialog.worker_id && question.request_id == dialog.request_id
        });
        Some(if worker_ended && !pending {
            WorkerQuestionStatus::WorkerEnded
        } else if let Some(refused) = dialog.refused.as_ref() {
            WorkerQuestionStatus::Gone(refused.clone())
        } else if !pending {
            WorkerQuestionStatus::Gone("it was answered elsewhere or withdrawn".to_owned())
        } else if dialog.loading {
            WorkerQuestionStatus::Loading
        } else {
            WorkerQuestionStatus::Open
        })
    }

    /// The owner's pane when it still lives.
    fn worker_question_owner_pane(&self) -> Option<&crate::protocol::ClientShellPane> {
        let owner = self
            .worker_question_dialog()?
            .detail
            .as_ref()?
            .owner_pane_id
            .as_deref()?;
        self.snapshot
            .as_deref()?
            .panes
            .iter()
            .find(|pane| pane.pane_id == owner)
    }

    /// Which buttons the dialog has, each with whether it can be pressed.
    fn worker_question_buttons(
        &self,
        status: &WorkerQuestionStatus,
    ) -> Vec<(WorkerQuestionButton, &'static str, bool)> {
        let Some(dialog) = self.worker_question_dialog() else {
            return Vec::new();
        };
        if dialog.deny_message.is_some() {
            return vec![
                (WorkerQuestionButton::SendDeny, "deny", true),
                (WorkerQuestionButton::CancelDeny, "cancel", true),
            ];
        }
        if dialog.confirm_stop {
            return vec![
                (WorkerQuestionButton::ConfirmStop, "stop worker", true),
                (WorkerQuestionButton::CancelStop, "cancel", true),
            ];
        }
        let open = *status == WorkerQuestionStatus::Open;
        let answerable = open && dialog.detail.is_some();
        let live = matches!(
            status,
            WorkerQuestionStatus::Open | WorkerQuestionStatus::Loading
        );
        let mut buttons = Vec::new();
        let choice = dialog
            .detail
            .as_ref()
            .is_some_and(|detail| detail.question.kind == WorkerQuestionKind::Choice);
        if choice {
            let complete =
                !dialog.picks.is_empty() && dialog.picks.iter().all(|picks| picks.contains(&true));
            buttons.push((
                WorkerQuestionButton::SendAnswer,
                "send answer",
                answerable && complete,
            ));
            buttons.push((WorkerQuestionButton::Deny, "decline", answerable));
        } else {
            buttons.push((WorkerQuestionButton::Allow, "allow once", answerable));
            buttons.push((WorkerQuestionButton::Deny, "deny", answerable));
        }
        let log = self.supports_endpoint_method(&Method::WorkerOpenLog(WorkerTarget {
            worker_id: String::new(),
        }));
        buttons.push((WorkerQuestionButton::ViewLog, "view log", log));
        let stop =
            self.supports_endpoint_method(&Method::WorkerDenyAndStop(WorkerDenyAndStopParams {
                worker_id: String::new(),
                request_id: String::new(),
                message: None,
            }));
        buttons.push((WorkerQuestionButton::Stop, "stop worker", live && stop));
        if self.worker_question_owner_pane().is_some() {
            buttons.push((WorkerQuestionButton::OpenOwner, "open owner", true));
        }
        if self.next_worker_question(1).is_some() {
            buttons.push((WorkerQuestionButton::Previous, "‹ prev", true));
            buttons.push((WorkerQuestionButton::Next, "next ›", true));
        }
        buttons.push((WorkerQuestionButton::Close, "close", true));
        buttons
    }

    /// What the dialog draws; none when it is not open.
    pub(super) fn worker_question_view(&self) -> Option<WorkerQuestionView> {
        let dialog = self.worker_question_dialog()?;
        let snapshot = self.snapshot.as_deref()?;
        let status = self.worker_question_status()?;
        let preview = snapshot.worker_questions.iter().find(|question| {
            question.worker_id == dialog.worker_id && question.request_id == dialog.request_id
        });
        let worker = snapshot
            .workers
            .iter()
            .find(|worker| worker.worker_id == dialog.worker_id);
        let detail = dialog.detail.as_ref();
        let task = detail
            .map(|detail| detail.name.as_str())
            .or(worker.map(|worker| worker.name.as_str()))
            .filter(|name| !name.is_empty());
        let title = match task {
            Some(task) => format!("worker {} · {task}", dialog.worker_id),
            None => format!("worker {}", dialog.worker_id),
        };
        let pending = self.pending_worker_questions();
        let position = pending
            .iter()
            .position(|question| {
                question.worker_id == dialog.worker_id && question.request_id == dialog.request_id
            })
            .map(|index| (index + 1, pending.len()))
            .filter(|(_, total)| *total > 1);
        let cwd = detail
            .map(|detail| detail.cwd.as_str())
            .or(preview.map(|question| question.cwd.as_str()))
            .or(worker.map(|worker| worker.cwd.as_str()))
            .unwrap_or_default();
        let repo = std::path::Path::new(cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.to_owned());
        let tool = detail
            .map(|detail| detail.question.tool_name.as_str())
            .or(preview.map(|question| question.tool_name.as_str()))
            .unwrap_or_default();
        let context = [repo.as_str(), tool]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let owner = match detail {
            None => "owner: loading…".to_owned(),
            Some(detail) => match detail.owner_pane_id.as_deref() {
                None => "no owner: it asks you directly".to_owned(),
                Some(pane_id) => {
                    let pane = self.worker_question_owner_pane();
                    let mut text = match pane {
                        Some(pane) => {
                            let tab = snapshot
                                .tabs
                                .iter()
                                .find(|tab| tab.tab_id == pane.tab_id)
                                .map(|tab| {
                                    super::render::tabs::sidebar_tab_label(
                                        tab,
                                        snapshot,
                                        &self.config,
                                    )
                                });
                            match tab {
                                Some(tab) => format!("owner: pane {pane_id} in tab {tab}"),
                                None => format!("owner: pane {pane_id}"),
                            }
                        }
                        None => format!("owner: pane {pane_id} (closed)"),
                    };
                    if let Some(tenure) = detail.owner_coordinator_id.as_deref() {
                        text.push_str(&format!(" · tenure {tenure}"));
                    }
                    text
                }
            },
        };
        let escalation = match detail {
            Some(detail) if detail.quiet => {
                Some("waits for its coordinator; you may still answer it".to_owned())
            }
            Some(detail) => detail
                .question
                .escalated
                .as_ref()
                .map(|why| format!("escalated: {why}")),
            None => preview
                .filter(|question| question.quiet)
                .map(|_| "waits for its coordinator; you may still answer it".to_owned()),
        };
        let mut lines = Vec::new();
        match detail {
            Some(detail) if detail.question.kind == WorkerQuestionKind::Choice => {
                for (question_index, question) in detail.question.questions.iter().enumerate() {
                    let heading = match question.header.as_deref() {
                        Some(header) => format!("{header}: {}", question.question),
                        None => question.question.clone(),
                    };
                    lines.push(WorkerQuestionLine::Heading(heading));
                    for (option_index, label) in question.options.iter().enumerate() {
                        lines.push(WorkerQuestionLine::Option {
                            button: WorkerQuestionButton::Pick(question_index, option_index),
                            label: label.clone(),
                            picked: dialog
                                .picks
                                .get(question_index)
                                .and_then(|picks| picks.get(option_index))
                                .copied()
                                .unwrap_or(false),
                            multi: question.multi_select,
                        });
                    }
                }
            }
            Some(detail) => {
                lines.extend(
                    detail
                        .input_text
                        .split('\n')
                        .map(|line| WorkerQuestionLine::Text(line.to_owned())),
                );
            }
            // Until the whole input is here, only the preview, said as such.
            None => {
                if let Some(preview) = preview {
                    lines.push(WorkerQuestionLine::Text(preview.text.clone()));
                }
            }
        }
        let notice = match &status {
            WorkerQuestionStatus::Loading => Some(("loading the whole input…".to_owned(), false)),
            WorkerQuestionStatus::Sending(action) => {
                Some((action_progress(*action).to_owned(), false))
            }
            WorkerQuestionStatus::Gone(why) => Some((format!("no longer pending: {why}"), true)),
            WorkerQuestionStatus::WorkerEnded => Some(("the worker exited".to_owned(), true)),
            WorkerQuestionStatus::Open => match dialog.failure.as_ref() {
                Some(failure) => Some((format!("refused: {failure}"), true)),
                None if dialog.unsupported => Some((
                    "this server cannot send the whole input; update it to answer here".to_owned(),
                    true,
                )),
                None if dialog.confirm_stop => Some((
                    "stop the worker? its question is denied and its turn ends".to_owned(),
                    true,
                )),
                None if dialog.deny_message.is_some() => Some((
                    "deny with a message for the model (optional), then ↵".to_owned(),
                    false,
                )),
                None => None,
            },
        };
        let buttons = self.worker_question_buttons(&status);
        Some(WorkerQuestionView {
            title,
            position,
            context,
            owner,
            escalation,
            lines,
            status,
            notice,
            buttons,
            deny_message: dialog.deny_message.clone(),
            confirm_stop: dialog.confirm_stop,
            scroll: dialog.scroll,
            focused: dialog.focused,
        })
    }

    /// Presses a button: only one the view offers as pressable.
    pub(super) fn press_worker_question_button(
        &mut self,
        button: WorkerQuestionButton,
        outcome: &mut ClientShellInput,
    ) {
        let enabled = match button {
            WorkerQuestionButton::Pick(question, option) => {
                self.worker_question_view().is_some_and(|view| {
                    view.status == WorkerQuestionStatus::Open
                        && view.lines.iter().any(|line| {
                            matches!(line, WorkerQuestionLine::Option { button, .. }
                                if *button == WorkerQuestionButton::Pick(question, option))
                        })
                })
            }
            _ => self.worker_question_view().is_some_and(|view| {
                view.buttons
                    .iter()
                    .any(|(candidate, _, enabled)| *candidate == button && *enabled)
            }),
        };
        if !enabled {
            return;
        }
        outcome.repaint = true;
        let Some(dialog) = self.worker_question_dialog_mut() else {
            return;
        };
        dialog.failure = None;
        match button {
            WorkerQuestionButton::Pick(question, option) => {
                let multi = dialog
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.question.questions.get(question))
                    .is_some_and(|question| question.multi_select);
                if let Some(picks) = dialog.picks.get_mut(question) {
                    if multi {
                        if let Some(pick) = picks.get_mut(option) {
                            *pick = !*pick;
                        }
                    } else {
                        for (index, pick) in picks.iter_mut().enumerate() {
                            *pick = index == option;
                        }
                    }
                }
            }
            WorkerQuestionButton::Allow => {
                let params = answer_params(dialog, WorkerDecision::Allow, Vec::new(), None);
                self.send_worker_question_action(
                    Method::WorkerAnswer(params),
                    WorkerQuestionAction::Allow,
                    outcome,
                );
            }
            WorkerQuestionButton::SendAnswer => {
                let answers = dialog
                    .picks
                    .iter()
                    .map(|picks| {
                        picks
                            .iter()
                            .enumerate()
                            .filter(|(_, picked)| **picked)
                            .map(|(index, _)| (index + 1).to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .collect();
                let params = answer_params(dialog, WorkerDecision::Allow, answers, None);
                self.send_worker_question_action(
                    Method::WorkerAnswer(params),
                    WorkerQuestionAction::Answer,
                    outcome,
                );
            }
            WorkerQuestionButton::Deny => {
                dialog.deny_message = Some(TextEditor::default());
                dialog.focused = None;
            }
            WorkerQuestionButton::SendDeny => {
                let message = dialog
                    .deny_message
                    .take()
                    .map(|editor| editor.trim().to_owned())
                    .filter(|message| !message.is_empty());
                dialog.focused = None;
                let params = answer_params(dialog, WorkerDecision::Deny, Vec::new(), message);
                self.send_worker_question_action(
                    Method::WorkerAnswer(params),
                    WorkerQuestionAction::Deny,
                    outcome,
                );
            }
            WorkerQuestionButton::CancelDeny => {
                dialog.deny_message = None;
                dialog.focused = None;
            }
            WorkerQuestionButton::Stop => {
                dialog.confirm_stop = true;
                dialog.focused = None;
            }
            WorkerQuestionButton::CancelStop => {
                dialog.confirm_stop = false;
                dialog.focused = None;
            }
            WorkerQuestionButton::ConfirmStop => {
                dialog.confirm_stop = false;
                dialog.focused = None;
                let params = WorkerDenyAndStopParams {
                    worker_id: dialog.worker_id.clone(),
                    request_id: dialog.request_id.clone(),
                    message: None,
                };
                self.send_worker_question_action(
                    Method::WorkerDenyAndStop(params),
                    WorkerQuestionAction::Stop,
                    outcome,
                );
            }
            WorkerQuestionButton::ViewLog => {
                let worker_id = dialog.worker_id.clone();
                // The log is a popup under the dialog: it must show.
                self.overlay = None;
                self.open_worker_log(worker_id, outcome);
            }
            WorkerQuestionButton::OpenOwner => {
                let Some(pane) = self.worker_question_owner_pane().cloned() else {
                    return;
                };
                self.overlay = None;
                self.expand_for_jump(Some(&pane.workspace_id), outcome);
                self.push_endpoint_method(
                    Method::PaneFocus(PaneTarget {
                        pane_id: pane.pane_id,
                    }),
                    outcome,
                );
            }
            WorkerQuestionButton::Previous | WorkerQuestionButton::Next => {
                let step = if button == WorkerQuestionButton::Next {
                    1
                } else {
                    -1
                };
                if let Some((worker_id, request_id)) = self.next_worker_question(step) {
                    self.open_worker_question(worker_id, request_id, outcome);
                }
            }
            WorkerQuestionButton::Close => self.overlay = None,
        }
    }

    fn send_worker_question_action(
        &mut self,
        method: Method,
        action: WorkerQuestionAction,
        outcome: &mut ClientShellInput,
    ) {
        let Some(dialog) = self.worker_question_dialog() else {
            return;
        };
        let kind = PendingEndpointKind::WorkerQuestionAction {
            endpoint_id: dialog.endpoint_id.clone(),
            request_id: dialog.request_id.clone(),
        };
        if self.push_endpoint_method_with_kind(method, kind, outcome) {
            if let Some(dialog) = self.worker_question_dialog_mut() {
                dialog.sending = Some(action);
            }
        }
    }

    /// Moves the keyboard's choice over the pressable buttons and options.
    pub(super) fn focus_worker_question_button(&mut self, delta: isize) {
        let Some(view) = self.worker_question_view() else {
            return;
        };
        let mut targets: Vec<WorkerQuestionButton> = Vec::new();
        if view.status == WorkerQuestionStatus::Open
            && view.deny_message.is_none()
            && !view.confirm_stop
        {
            targets.extend(view.lines.iter().filter_map(|line| match line {
                WorkerQuestionLine::Option { button, .. } => Some(*button),
                _ => None,
            }));
        }
        targets.extend(
            view.buttons
                .iter()
                .filter(|(_, _, enabled)| *enabled)
                .map(|(button, _, _)| *button),
        );
        if targets.is_empty() {
            return;
        }
        let len = targets.len() as isize;
        let next = match view
            .focused
            .and_then(|focused| targets.iter().position(|button| *button == focused))
        {
            Some(index) => (index as isize + delta).rem_euclid(len) as usize,
            None if delta < 0 => targets.len() - 1,
            None => 0,
        };
        if let Some(dialog) = self.worker_question_dialog_mut() {
            dialog.focused = targets.get(next).copied();
        }
    }

    /// Scrolls the input by `delta` rows, kept within what the last frame
    /// could scroll.
    pub(super) fn scroll_worker_question(&mut self, delta: isize) {
        let max = self.hits.worker_question_max_scroll;
        if let Some(dialog) = self.worker_question_dialog_mut() {
            dialog.scroll = (dialog.scroll as isize + delta).clamp(0, max as isize) as usize;
        }
    }

    /// A key while the dialog is open. Enter presses only the button Tab
    /// chose, so no key answers by default; in Deny's message field Enter
    /// sends the denial the user opened it for.
    pub(super) fn worker_question_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        let typing = self
            .worker_question_dialog()
            .is_some_and(|dialog| dialog.deny_message.is_some());
        if typing {
            match key.code {
                KeyCode::Enter => {
                    self.press_worker_question_button(WorkerQuestionButton::SendDeny, outcome)
                }
                KeyCode::Esc => {
                    self.press_worker_question_button(WorkerQuestionButton::CancelDeny, outcome)
                }
                KeyCode::Tab => self.focus_worker_question_button(1),
                KeyCode::BackTab => self.focus_worker_question_button(-1),
                _ => {
                    if let Some(editor) = self
                        .worker_question_dialog_mut()
                        .and_then(|dialog| dialog.deny_message.as_mut())
                    {
                        editor.handle_key(key);
                    }
                }
            }
            return;
        }
        let page = self.hits.worker_question_body_rows.max(1) as isize;
        match key.code {
            KeyCode::Esc => {
                let dialog = self.worker_question_dialog();
                if dialog.is_some_and(|dialog| dialog.confirm_stop) {
                    self.press_worker_question_button(WorkerQuestionButton::CancelStop, outcome);
                } else {
                    self.overlay = None;
                }
            }
            KeyCode::Char('q') => self.overlay = None,
            KeyCode::Tab | KeyCode::Right => self.focus_worker_question_button(1),
            KeyCode::BackTab | KeyCode::Left => self.focus_worker_question_button(-1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(focused) = self
                    .worker_question_dialog()
                    .and_then(|dialog| dialog.focused)
                {
                    self.press_worker_question_button(focused, outcome);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_worker_question(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_worker_question(1),
            KeyCode::PageUp => self.scroll_worker_question(-page),
            KeyCode::PageDown => self.scroll_worker_question(page),
            KeyCode::Home => self.scroll_worker_question(isize::MIN / 2),
            KeyCode::End => self.scroll_worker_question(isize::MAX / 2),
            KeyCode::Char('l') => {
                self.press_worker_question_button(WorkerQuestionButton::ViewLog, outcome)
            }
            KeyCode::Char('o') => {
                self.press_worker_question_button(WorkerQuestionButton::OpenOwner, outcome)
            }
            KeyCode::Char('n') => {
                self.press_worker_question_button(WorkerQuestionButton::Next, outcome)
            }
            KeyCode::Char('p') => {
                self.press_worker_question_button(WorkerQuestionButton::Previous, outcome)
            }
            _ => {}
        }
    }

    /// Typed or pasted text goes into Deny's message field when it is open.
    pub(super) fn insert_worker_question_text(&mut self, text: &str) -> bool {
        match self
            .worker_question_dialog_mut()
            .and_then(|dialog| dialog.deny_message.as_mut())
        {
            Some(editor) => {
                editor.insert(text);
                true
            }
            None => false,
        }
    }

    /// A mouse event while the dialog is open: a button or option is
    /// pressed, the wheel scrolls the input, a click outside closes it.
    pub(super) fn worker_question_mouse(
        &mut self,
        kind: MouseEventKind,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) {
        match kind {
            MouseEventKind::ScrollUp => {
                self.scroll_worker_question(-3);
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown => {
                self.scroll_worker_question(3);
                outcome.repaint = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(button) = self
                    .hits
                    .worker_question_buttons
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .map(|(_, button)| *button)
                {
                    self.press_worker_question_button(button, outcome);
                } else if !super::contains(self.hits.worker_question_popup, point) {
                    self.overlay = None;
                    outcome.repaint = true;
                }
            }
            _ => {}
        }
    }
}

fn answer_params(
    dialog: &WorkerQuestionDialog,
    decision: WorkerDecision,
    answers: Vec<String>,
    message: Option<String>,
) -> WorkerAnswerParams {
    WorkerAnswerParams {
        worker_id: dialog.worker_id.clone(),
        request_id: Some(dialog.request_id.clone()),
        decision: Some(decision),
        answers,
        message,
        command_id: None,
    }
}
