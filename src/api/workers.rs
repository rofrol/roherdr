//! `worker.*` requests. Workers are server runtime state that `AppState`
//! does not hold, so these run on the API connection's thread instead of
//! going through the app channel.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, ResponseResult, SuccessResponse, WorkerWaitUntil,
};
use crate::api::server::{should_stop_connection, CONNECTION_POLL_INTERVAL};
use crate::ipc::LocalStream;
use crate::workers::{WorkerError, WorkerSupervisor};

pub(super) fn is_worker_method(method: &Method) -> bool {
    matches!(
        method,
        Method::WorkerStart(_)
            | Method::WorkerStartChoice(_)
            | Method::WorkerGatedRun(_)
            | Method::WorkerStatus(_)
            | Method::WorkerList(_)
            | Method::WorkerAdmissions(_)
            | Method::WorkerWait(_)
            | Method::WorkerEvents(_)
            | Method::WorkerPrompt(_)
            | Method::WorkerInterrupt(_)
            | Method::WorkerStop(_)
            | Method::WorkerKill(_)
            | Method::WorkerAnswer(_)
            | Method::WorkerAnswerAs(_)
            | Method::WorkerAnswerFromPane(_)
            | Method::WorkerAck(_)
            | Method::WorkerObligations(_)
            | Method::WorkerDrain(_)
            | Method::WorkerWaitDrained(_)
            | Method::WorkerRuns(_)
            | Method::WorkerEscalate(_)
            | Method::WorkerVerify(_)
            | Method::WorkerQuestion(_)
            | Method::WorkerTranscript(_)
            | Method::WorkerTranscriptWait(_)
            | Method::WorkerDenyAndStop(_)
            | Method::TodoRun(_)
            | Method::TodoRunWait(_)
            | Method::TodoRunAnthropic(_)
            | Method::TodoCancelPending(_)
            | Method::TodoDraftRun(_)
            | Method::TodoResume(_)
            | Method::TodoWait(_)
            | Method::TodoStatus(_)
            | Method::TodoAckStop(_)
            | Method::TodoRuns(_)
            | Method::TodoReview(_)
            | Method::TodoNext(_)
            | Method::TodoStop(_)
            | Method::TodoQueueSet(_)
            | Method::TodoQueueStatus(_)
            | Method::TodoRunnableState(_)
            | Method::TodoGrant(_)
            | Method::TodoGrants(_)
            | Method::TodoRevoke(_)
            | Method::DecisionAdd(_)
            | Method::DecisionDecide(_)
            | Method::DecisionList(_)
            | Method::DecisionGet(_)
            | Method::HistoryList(_)
            | Method::HistoryItem(_)
            | Method::HistoryReconcile(_)
            | Method::HistoryOverrides(_)
            | Method::HistoryWouldDeny(_)
            | Method::ReportList(_)
    )
}

/// Answers one `worker.*` request. `None` means the client went away or the
/// server is stopping while `worker.wait` was blocked.
pub(crate) fn handle_worker_request(
    request_id: String,
    method: Method,
    stream: &mut LocalStream,
    running: &Arc<AtomicBool>,
) -> Option<String> {
    let supervisor = crate::workers::supervisor();
    let result = match method {
        Method::WorkerWait(params) => {
            // Only to notice a client that went away or a stopping server;
            // the worker's events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            let until = params.until.unwrap_or(WorkerWaitUntil::TurnEnd);
            let waited = match (until, params.after) {
                (WorkerWaitUntil::Attention, after) => supervisor
                    .wait_attention(
                        &params.worker_id,
                        after.or_else(|| {
                            params
                                .caller_pane_id
                                .as_deref()
                                .and_then(|pane| supervisor.owner_cursor(&params.worker_id, pane))
                        }),
                        CONNECTION_POLL_INTERVAL,
                        keep_waiting,
                    )
                    .map(|attention| {
                        attention.map(|attention| ResponseResult::WorkerAttention {
                            reason: attention.reason,
                            questions: attention.questions,
                            seq: attention.seq,
                            worker: attention.worker,
                        })
                    }),
                (_, Some(_)) => Err(WorkerError::Invalid(
                    "after is taken only with until: attention".into(),
                )),
                (until, None) => supervisor
                    .wait(
                        &params.worker_id,
                        until,
                        CONNECTION_POLL_INTERVAL,
                        keep_waiting,
                    )
                    .map(|worker| worker.map(|worker| ResponseResult::WorkerInfo { worker })),
            };
            match waited {
                Ok(None) => return None,
                Ok(Some(result)) => Ok(result),
                Err(error) => Err(error),
            }
        }
        Method::WorkerEvents(params) => {
            // As above: the owner's workers' events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            match supervisor.events(&params, CONNECTION_POLL_INTERVAL, keep_waiting) {
                Ok(None) => return None,
                Ok(Some(events)) => Ok(ResponseResult::WorkerEvents {
                    owner: events.owner,
                    events: events.events,
                    next_cursor: events.next_cursor,
                    more: events.more,
                    resync_required: events.resync_required,
                    snapshot: events.snapshot,
                }),
                Err(error) => Err(error),
            }
        }
        Method::WorkerTranscriptWait(params) => {
            // As above: the worker's events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            match supervisor.transcript_wait(&params, CONNECTION_POLL_INTERVAL, keep_waiting) {
                Ok(None) => return None,
                Ok(Some(transcript)) => Ok(ResponseResult::WorkerTranscript { transcript }),
                Err(error) => Err(error),
            }
        }
        Method::WorkerWaitDrained(params) => {
            // As above: only to notice a client that went away; the
            // workers' turn-end events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            let drain = supervisor.wait_drained(&params, CONNECTION_POLL_INTERVAL, keep_waiting)?;
            Ok::<_, WorkerError>(ResponseResult::WorkerDrain { drain })
        }
        Method::WorkerGatedRun(params) => {
            // As above: the worker's turn-end, question and exit events end
            // the run; a client that went away ends it too, killing the
            // worker.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            match supervisor.gated_run(&params, CONNECTION_POLL_INTERVAL, keep_waiting) {
                Ok(None) => return None,
                Ok(Some(run)) => Ok(ResponseResult::WorkerGatedRun { run }),
                Err(error) => Err(error),
            }
        }
        Method::TodoWait(params) => {
            // As above: the run's events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            // The caller's pane waits on the run while this blocks.
            let _edge = params
                .waiter_pane
                .clone()
                .filter(|pane| !pane.trim().is_empty())
                .map(|pane| crate::workers::WaitEdge::begin(pane, params.run_id.clone()));
            match supervisor.todo_wait(&params, CONNECTION_POLL_INTERVAL, keep_waiting) {
                Ok(None) => return None,
                Ok(Some((event, run))) => Ok(ResponseResult::TodoRunEvent { event, run }),
                Err(error) => Err(error),
            }
        }
        method => handle_immediate(supervisor, method),
    };
    Some(encode(request_id, result))
}

fn handle_immediate(
    supervisor: &WorkerSupervisor,
    method: Method,
) -> Result<ResponseResult, WorkerError> {
    let worker = match method {
        Method::WorkerStart(params) => {
            supervisor.fence_pane(params.owner_pane_id.as_deref())?;
            supervisor.start(&params)?
        }
        Method::WorkerStartChoice(params) => {
            supervisor.fence_pane(params.start.owner_pane_id.as_deref())?;
            supervisor.start_choice(params)?
        }
        Method::WorkerAdmissions(params) => {
            return Ok(ResponseResult::WorkerAdmissions {
                admissions: supervisor.admissions(&params.cwd)?,
            })
        }
        Method::WorkerStatus(target) => supervisor.status(&target.worker_id)?,
        Method::WorkerList(_) => {
            return Ok(ResponseResult::WorkerList {
                workers: supervisor.list(),
            })
        }
        Method::WorkerPrompt(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            supervisor.prompt_command(&params)?
        }
        Method::WorkerInterrupt(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            supervisor.interrupt(&params)?
        }
        Method::WorkerStop(target) => {
            supervisor.fence_pane(target.caller_pane_id.as_deref())?;
            supervisor.stop_command(&target)?
        }
        Method::WorkerKill(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            let (worker, killed) = supervisor.kill_command(&params)?;
            return Ok(ResponseResult::WorkerKilled { worker, killed });
        }
        Method::WorkerAnswer(params) => supervisor.answer(&params)?,
        Method::WorkerAnswerFromPane(params) => {
            supervisor.fence_pane(Some(&params.caller_pane_id))?;
            supervisor.answer(&params.answer)?
        }
        Method::WorkerAnswerAs(params) => {
            supervisor.fence_tenure(Some(&params.coordinator_id))?;
            supervisor.answer_as(&params)?
        }
        Method::WorkerTranscript(params) => {
            return Ok(ResponseResult::WorkerTranscript {
                transcript: supervisor.transcript(&params)?,
            })
        }
        Method::WorkerQuestion(target) => {
            return Ok(ResponseResult::WorkerQuestionDetail {
                detail: supervisor.question_detail(&target.worker_id, &target.request_id)?,
            })
        }
        // A user's click in a client (its shape is advertised, so it carries
        // no caller pane); the CLI has no command for it, so no coordinator
        // sends it.
        Method::WorkerDenyAndStop(params) => supervisor.deny_and_stop(&params)?,
        Method::WorkerAck(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            supervisor.ack(&params.worker_id, params.seq)?
        }
        Method::WorkerEscalate(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            supervisor.escalate(&params.worker_id, &params.request_id)?
        }
        Method::WorkerRuns(params) => {
            let (items, unassigned) = supervisor.runs(&params)?;
            return Ok(ResponseResult::WorkerRuns { items, unassigned });
        }
        Method::WorkerVerify(params) => {
            return Ok(ResponseResult::WorkerVerification {
                verification: supervisor.verify(&params)?,
            })
        }
        Method::TodoRun(params) => {
            supervisor.fence_pane(params.owner_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_run(params)?,
            });
        }
        Method::TodoRunAnthropic(params) => {
            supervisor.fence_pane(params.owner_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_run_anthropic(params)?,
            });
        }
        Method::TodoRunWait(params) => {
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_run_wait(params)?,
            })
        }
        Method::TodoCancelPending(target) => {
            supervisor.fence_pane(target.caller_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_cancel_pending_owned(&target)?,
            });
        }
        Method::TodoDraftRun(params) => {
            supervisor.fence_pane(params.owner_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_draft_run(params)?,
            });
        }
        Method::TodoResume(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_resume(params)?,
            });
        }
        Method::TodoStatus(target) => {
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_status(&target.run_id)?,
            })
        }
        Method::TodoAckStop(target) => {
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_ack_stop(&target.run_id)?,
            })
        }
        Method::TodoRuns(params) => {
            let (runs, landing) =
                supervisor.todo_runs(params.repo.as_deref(), params.commit.as_deref())?;
            return Ok(ResponseResult::TodoRuns { runs, landing });
        }
        Method::TodoReview(params) => {
            return Ok(ResponseResult::TodoReview {
                review: Box::new(supervisor.todo_review(
                    &params.run_id,
                    params.attempt,
                    params.diff,
                )?),
            })
        }
        Method::TodoNext(params) => {
            supervisor.fence_pane(params.owner_pane_id.as_deref())?;
            let started = supervisor.todo_next(&params)?;
            return Ok(ResponseResult::TodoNext {
                item: started.item,
                coordinator: started.coordinator,
                worker: started.worker,
                chain: started.chain,
            });
        }
        Method::TodoStop(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoChain {
                chain: supervisor.todo_stop(&params.cwd)?,
            });
        }
        Method::TodoQueueSet(params) => {
            supervisor.fence_pane(params.owner_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoQueue {
                queue: supervisor.todo_queue_set(params)?,
            });
        }
        Method::TodoQueueStatus(target) => {
            return Ok(ResponseResult::TodoQueue {
                queue: supervisor.todo_queue_status(&target.cwd)?,
            })
        }
        Method::TodoRunnableState(target) => {
            return Ok(ResponseResult::TodoRunnableState {
                state: supervisor.todo_runnable_state(&target.cwd)?,
            })
        }
        Method::TodoGrant(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoGrant {
                grant: supervisor.todo_grant(params)?,
            });
        }
        Method::TodoGrants(params) => {
            return Ok(ResponseResult::TodoGrants {
                grants: supervisor.todo_grants(params)?,
            })
        }
        Method::TodoRevoke(params) => {
            supervisor.fence_pane(params.caller_pane_id.as_deref())?;
            return Ok(ResponseResult::TodoGrants {
                grants: supervisor.todo_revoke(params)?,
            });
        }
        Method::HistoryList(params) => {
            return Ok(ResponseResult::HistoryList {
                items: supervisor.history_list(params.repo.as_deref())?,
            })
        }
        Method::HistoryItem(params) => {
            return Ok(ResponseResult::HistoryItem {
                item: supervisor.history_item(&params.item, params.repo.as_deref())?,
            })
        }
        Method::HistoryReconcile(params) => {
            return Ok(ResponseResult::HistoryReconcile {
                reconcile: supervisor.history_reconcile(&params.repo)?,
            })
        }
        Method::DecisionAdd(params) => {
            return Ok(ResponseResult::Decision {
                decision: supervisor.decision_add(params)?,
            })
        }
        Method::DecisionDecide(params) => {
            return Ok(ResponseResult::Decision {
                decision: supervisor.decision_decide(params)?,
            })
        }
        Method::DecisionList(params) => {
            return Ok(ResponseResult::Decisions {
                decisions: supervisor.decision_list(params)?,
            })
        }
        Method::DecisionGet(params) => {
            return Ok(ResponseResult::Decision {
                decision: supervisor.decision_get(&params.id)?,
            })
        }
        Method::HistoryOverrides(params) => {
            return Ok(ResponseResult::HistoryOverrides {
                overrides: supervisor.overrides(params.repo.as_deref())?,
            })
        }
        Method::HistoryWouldDeny(params) => {
            let (would_deny, shapes) = supervisor.would_deny(params.repo.as_deref())?;
            return Ok(ResponseResult::HistoryWouldDeny { would_deny, shapes });
        }
        Method::ReportList(params) => {
            return Ok(ResponseResult::Reports {
                reports: supervisor.reports(params.all)?,
            })
        }
        Method::WorkerDrain(params) => {
            return Ok(ResponseResult::WorkerDrain {
                drain: supervisor.drain(params.action, params.reason.as_deref()),
            })
        }
        Method::WorkerObligations(params) => {
            return Ok(ResponseResult::WorkerObligations {
                obligations: supervisor.obligations(params.owner_pane_id.as_deref()),
            })
        }
        _ => return Err(WorkerError::Invalid("not a worker method".into())),
    };
    Ok(ResponseResult::WorkerInfo { worker })
}

fn encode(id: String, result: Result<ResponseResult, WorkerError>) -> String {
    let encoded = match result {
        Ok(result) => serde_json::to_string(&SuccessResponse { id, result }),
        Err(error) => serde_json::to_string(&ErrorResponse {
            id,
            error: ErrorBody {
                code: error.code().into(),
                message: error.to_string(),
            },
        }),
    };
    encoded.unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One method of each fenced family, with the field that names its
    /// caller's pane.
    fn fenced_methods(pane: Option<&str>) -> Vec<(&'static str, Method)> {
        let cwd = "/nonexistent-herdr-fence-test";
        let run = json!({
            "cwd": cwd, "item": "t-abcd2345", "task": "t", "message": "feat: x",
            "paths": ["src/**"], "checks": ["lint"],
        });
        let with = |mut value: serde_json::Value, field: &str| {
            if let Some(pane) = pane {
                value[field] = json!(pane);
            }
            value
        };
        let cases = vec![
            (
                "worker.start",
                with(json!({"cwd": cwd, "prompt": "p"}), "owner_pane_id"),
            ),
            (
                "worker.prompt",
                with(json!({"worker_id": "w99", "text": "t"}), "caller_pane_id"),
            ),
            (
                "worker.interrupt",
                with(json!({"worker_id": "w99"}), "caller_pane_id"),
            ),
            (
                "worker.stop",
                with(json!({"worker_id": "w99"}), "caller_pane_id"),
            ),
            (
                "worker.kill",
                with(json!({"worker_id": "w99"}), "caller_pane_id"),
            ),
            match pane {
                Some(_) => (
                    "worker.answer_from_pane",
                    with(json!({"worker_id": "w99"}), "caller_pane_id"),
                ),
                None => ("worker.answer", json!({"worker_id": "w99"})),
            },
            (
                "worker.ack",
                with(json!({"worker_id": "w99", "seq": 1}), "caller_pane_id"),
            ),
            (
                "worker.escalate",
                with(
                    json!({"worker_id": "w99", "request_id": "r"}),
                    "caller_pane_id",
                ),
            ),
            ("todo.run", with(run.clone(), "owner_pane_id")),
            ("todo.run_anthropic", with(run, "owner_pane_id")),
            (
                "todo.cancel_pending",
                with(json!({"run_id": "r-none"}), "caller_pane_id"),
            ),
            (
                "todo.resume",
                with(
                    json!({"run_id": "r-none", "action": "retry", "event": 1}),
                    "caller_pane_id",
                ),
            ),
            ("todo.next", with(json!({"cwd": cwd}), "owner_pane_id")),
            ("todo.stop", with(json!({"cwd": cwd}), "caller_pane_id")),
            (
                "todo.queue_set",
                with(json!({"cwd": cwd, "mode": "paused"}), "owner_pane_id"),
            ),
            (
                "todo.grant",
                with(
                    json!({"cwd": cwd, "operation": "prepare", "hash": "h"}),
                    "caller_pane_id",
                ),
            ),
            (
                "todo.revoke",
                with(
                    json!({"cwd": cwd, "operation": "prepare"}),
                    "caller_pane_id",
                ),
            ),
        ];
        cases
            .into_iter()
            .map(|(name, params)| {
                let method = serde_json::from_value(json!({"method": name, "params": params}))
                    .unwrap_or_else(|error| panic!("{name}: {error}"));
                (name, method)
            })
            .collect()
    }

    fn codes(supervisor: &WorkerSupervisor, pane: Option<&str>) -> Vec<(&'static str, String)> {
        fenced_methods(pane)
            .into_iter()
            .map(|(name, method)| {
                let code = match handle_immediate(supervisor, method) {
                    Ok(_) => "ok".to_owned(),
                    Err(error) => error.code().to_owned(),
                };
                (name, code)
            })
            .collect()
    }

    /// A frozen tenure's pane is refused by every mutation family, a pane
    /// whose tenure was handed over is refused with its own code, and a
    /// caller outside any pane (the user's shell) is never fenced.
    #[test]
    fn a_frozen_or_superseded_caller_is_refused_by_every_mutation_family() {
        let root = std::env::temp_dir().join(format!(
            "herdr-fence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let note = root.join("note.md");
        std::fs::write(&note, "go on\n").unwrap();
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        supervisor.coordinator_start("/repo", "p1", None).unwrap();

        // Not frozen yet: nothing is refused for being frozen.
        for (name, code) in codes(&supervisor, Some("p1")) {
            assert!(!code.starts_with("coordinator_"), "{name}: {code}");
        }
        supervisor.handoff_ready("p1", &note).unwrap();
        for (name, code) in codes(&supervisor, Some("p1")) {
            assert_eq!(code, "coordinator_frozen", "{name}");
        }
        // The user's own shell, and another pane, are not the caller.
        for pane in [None, Some("p9")] {
            for (name, code) in codes(&supervisor, pane) {
                assert!(!code.starts_with("coordinator_"), "{name} {pane:?}: {code}");
            }
        }

        let prepared = supervisor.handoff_prepare("p1", "claude").unwrap();
        supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        let sha = supervisor
            .coordinator_status(None)
            .unwrap()
            .remove(0)
            .handoff
            .unwrap()
            .snapshot_sha256
            .unwrap();
        for (name, code) in codes(&supervisor, Some("p1")) {
            assert_eq!(code, "coordinator_frozen", "{name}");
        }
        supervisor.handoff_ack("p2", 1, &sha, None, None).unwrap();
        for (name, code) in codes(&supervisor, Some("p1")) {
            assert_eq!(code, "coordinator_superseded", "{name}");
        }
        for (name, code) in codes(&supervisor, Some("p2")) {
            assert!(!code.starts_with("coordinator_"), "{name}: {code}");
        }
        for (name, code) in codes(&supervisor, None) {
            assert!(!code.starts_with("coordinator_"), "{name}: {code}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
