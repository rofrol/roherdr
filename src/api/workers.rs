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
            let mut disconnected = false;
            let waited = supervisor.wait(
                &params.worker_id,
                params.until.unwrap_or(WorkerWaitUntil::TurnEnd),
                CONNECTION_POLL_INTERVAL,
                || {
                    disconnected = should_stop_connection(stream, running).unwrap_or(true);
                    !disconnected
                },
            );
            match waited {
                Ok(None) => return None,
                Ok(Some(worker)) => Ok(ResponseResult::WorkerInfo { worker }),
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
            supervisor.start(&params.cwd, &params.prompt, params.model.as_deref())?
        }
        Method::WorkerStatus(target) => supervisor.status(&target.worker_id)?,
        Method::WorkerList(_) => {
            return Ok(ResponseResult::WorkerList {
                workers: supervisor.list(),
            })
        }
        Method::WorkerPrompt(params) => supervisor.prompt(&params.worker_id, &params.text)?,
        Method::WorkerInterrupt(target) => supervisor.interrupt(&target.worker_id)?,
        Method::WorkerStop(target) => supervisor.stop(&target.worker_id)?,
        Method::WorkerKill(target) => supervisor.kill(&target.worker_id)?,
        Method::WorkerAnswer(params) => supervisor.answer(&params)?,
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
