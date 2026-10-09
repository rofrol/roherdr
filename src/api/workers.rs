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
            | Method::WorkerStatus(_)
            | Method::WorkerList(_)
            | Method::WorkerWait(_)
            | Method::WorkerPrompt(_)
            | Method::WorkerInterrupt(_)
            | Method::WorkerStop(_)
            | Method::WorkerKill(_)
            | Method::WorkerAnswer(_)
            | Method::WorkerAck(_)
            | Method::WorkerObligations(_)
            | Method::WorkerDrain(_)
            | Method::WorkerWaitDrained(_)
            | Method::WorkerRuns(_)
            | Method::WorkerEscalate(_)
            | Method::WorkerVerify(_)
            | Method::TodoRun(_)
            | Method::TodoResume(_)
            | Method::TodoWait(_)
            | Method::TodoStatus(_)
            | Method::TodoRuns(_)
            | Method::TodoReview(_)
            | Method::HistoryList(_)
            | Method::HistoryItem(_)
            | Method::HistoryReconcile(_)
            | Method::HistoryOverrides(_)
    )
}

/// Answers one `worker.*` request. `None` means the client went away or the
/// server is stopping while `worker.wait` was blocked.
pub(super) fn handle_worker_request(
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
                        after,
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
        Method::WorkerWaitDrained(params) => {
            // As above: only to notice a client that went away; the
            // workers' turn-end events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
            let drain = supervisor.wait_drained(&params, CONNECTION_POLL_INTERVAL, keep_waiting)?;
            Ok::<_, WorkerError>(ResponseResult::WorkerDrain { drain })
        }
        Method::TodoWait(params) => {
            // As above: the run's events end the wait.
            let keep_waiting = || !should_stop_connection(stream, running).unwrap_or(true);
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
        Method::WorkerStart(params) => supervisor.start(&params)?,
        Method::WorkerStatus(target) => supervisor.status(&target.worker_id)?,
        Method::WorkerList(_) => {
            return Ok(ResponseResult::WorkerList {
                workers: supervisor.list(),
            })
        }
        Method::WorkerPrompt(params) => supervisor.prompt_command(&params)?,
        Method::WorkerInterrupt(params) => supervisor.interrupt(&params)?,
        Method::WorkerStop(target) => supervisor.stop_command(&target)?,
        Method::WorkerKill(params) => {
            let (worker, killed) = supervisor.kill_command(&params)?;
            return Ok(ResponseResult::WorkerKilled { worker, killed });
        }
        Method::WorkerAnswer(params) => supervisor.answer(&params)?,
        Method::WorkerAck(params) => supervisor.ack(&params.worker_id, params.seq)?,
        Method::WorkerEscalate(params) => {
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
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_run(params)?,
            })
        }
        Method::TodoResume(params) => {
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_resume(params)?,
            })
        }
        Method::TodoStatus(target) => {
            return Ok(ResponseResult::TodoRun {
                run: supervisor.todo_status(&target.run_id)?,
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
        Method::HistoryOverrides(params) => {
            return Ok(ResponseResult::HistoryOverrides {
                overrides: supervisor.overrides(params.repo.as_deref())?,
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
