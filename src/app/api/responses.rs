use crate::api::schema::{ErrorBody, ErrorResponse, ResponseResult, SuccessResponse};

pub(crate) fn encode_success(id: String, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse { id, result }).unwrap()
}

pub(crate) fn encode_error(id: String, code: &str, message: impl Into<String>) -> String {
    encode_error_body(
        id,
        ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    )
}

/// Encodes a failed pane spawn, reporting `pty_exhausted` instead of
/// `fallback` when the system ran out of pseudo-terminals.
pub(crate) fn encode_spawn_error(id: String, fallback: &str, err: &std::io::Error) -> String {
    encode_error(
        id,
        crate::pty::headroom::spawn_error_code(err, fallback),
        err.to_string(),
    )
}

pub(super) fn encode_error_body(id: String, error: ErrorBody) -> String {
    serde_json::to_string(&ErrorResponse { id, error }).unwrap()
}
