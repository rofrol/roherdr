//! Blocking CLI waits (`agent wait`, `agent wait-turn`, `pane wait-output`,
//! `worker wait`, `todo wait`) that outlive a live handoff.
//!
//! Before it exits, a server that handed off answers each open wait with
//! `server_handed_off`, sent once the new server accepts on the same socket.
//! On that answer the wait checks the new server's protocol and sends the
//! same wait again (same target, condition and cursor, the rest of its
//! timeout): every one of these methods only reads and is level-triggered,
//! so a condition reached during the handoff answers at once, and a target
//! the new server does not know answers with its error, which ends the
//! wait. Nothing else is sent again: a connection that ends without an
//! answer, a refused connection and every other answer, an error answer
//! too, end the wait as before.

use std::time::{Duration, Instant};

use crate::api::client::{ApiClient, ApiClientError};
use crate::api::schema::Request;

/// The answer of a wait, and whether it came from a server that took over
/// after the one the wait started on handed off.
pub(super) struct WaitReply {
    pub(super) response: serde_json::Value,
    pub(super) reconnected: bool,
}

/// Sends the wait `request` builds and returns the answer, across live
/// handoffs of the local server. `request` gets the time the wait has
/// waited so far, so a wait with a timeout sends the rest of it. `command`
/// and `target` name the wait in what it prints to stderr.
pub(super) fn wait(
    command: &str,
    target: &str,
    mut request: impl FnMut(Duration) -> Request,
) -> std::io::Result<WaitReply> {
    if super::target::is_remote() {
        // A remote server's handoff is the bridge's business.
        return Ok(WaitReply {
            response: super::send_request(&request(Duration::ZERO))?,
            reconnected: false,
        });
    }
    let client = super::target::api_client()?;
    wait_on(
        &client,
        command,
        target,
        request,
        super::ensure_server_protocol_compatible,
    )
}

/// [`wait`] with `client`'s server, whose protocol `check` checks first and
/// after each handoff.
fn wait_on(
    client: &ApiClient,
    command: &str,
    target: &str,
    mut request: impl FnMut(Duration) -> Request,
    check: impl Fn(&ApiClient, &str) -> std::io::Result<()>,
) -> std::io::Result<WaitReply> {
    let request_id = request(Duration::ZERO).id;
    check(client, &request_id)?;
    let started = Instant::now();
    let mut reconnected = false;
    let response = send_across_handoffs(
        || client.request_value(&request(started.elapsed())),
        || {
            eprintln!(
                "herdr {command}: the server handed off; going on waiting on {target} with \
                 the new server"
            );
            reconnected = true;
            check(client, &request_id)
        },
    )?;
    let response = response.map_err(|error| {
        if matches!(error, ApiClientError::EmptyResponse) {
            eprintln!(
                "herdr {command}: the server closed the connection without an answer (it \
                 stopped or restarted); not waiting on {target} any more"
            );
        }
        super::map_server_not_running_or_io(error, &request_id, client)
    })?;
    Ok(WaitReply {
        response,
        reconnected,
    })
}

/// Prints, after a handoff, that the new server's error answer ended the
/// wait: the target is gone from it.
pub(super) fn report_gave_up(command: &str, target: &str, reply: &WaitReply) {
    let error = &reply.response["error"];
    if reply.reconnected && !error.is_null() {
        eprintln!(
            "herdr {command}: gave up on {target} after a server handoff: the new server \
             answered {}: {}",
            error["code"].as_str().unwrap_or("?"),
            error["message"].as_str().unwrap_or_default()
        );
    }
}

/// `total_ms` less what has passed; `None` (no timeout) stays `None`.
pub(super) fn remaining_ms(total_ms: Option<u64>, elapsed: Duration) -> Option<u64> {
    total_ms
        .map(|total| total.saturating_sub(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)))
}

/// Whether `response` is a handed-off server's answer to an open wait.
fn handed_off(response: &serde_json::Value) -> bool {
    response["error"]["code"] == crate::api::SERVER_HANDED_OFF
}

/// Sends a request with `send` and returns the server's answer. Each
/// `server_handed_off` answer runs `reconnected` (the protocol check of the
/// new server) and sends the request again, once per answer; any other
/// answer or error ends it.
fn send_across_handoffs(
    mut send: impl FnMut() -> Result<serde_json::Value, ApiClientError>,
    mut reconnected: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<Result<serde_json::Value, ApiClientError>> {
    loop {
        match send() {
            Ok(response) if handed_off(&response) => reconnected()?,
            other => return Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    fn handed_off_answer() -> serde_json::Value {
        serde_json::json!({
            "id": "cli:wait",
            "error": {"code": "server_handed_off", "message": "the server handed off"},
        })
    }

    /// Each handoff answer is followed by the new server's check and the
    /// same wait again, whose answer ends it.
    #[test]
    fn a_handoff_answer_sends_the_same_wait_to_the_new_server() {
        let reached = serde_json::json!({"id": "cli:wait", "result": {"type": "agent_info"}});
        let mut replies = vec![
            Ok(handed_off_answer()),
            Ok(handed_off_answer()),
            Ok(reached.clone()),
        ]
        .into_iter();
        let (mut sent, mut checked) = (0, 0);
        let response = send_across_handoffs(
            || {
                sent += 1;
                replies.next().unwrap()
            },
            || {
                checked += 1;
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(response, reached);
        assert_eq!((sent, checked), (3, 2));

        // A new server that refuses the check (another protocol) ends it.
        let mut replies = vec![Ok(handed_off_answer())].into_iter();
        let response = send_across_handoffs(
            || replies.next().unwrap(),
            || Err(Error::other("protocol mismatch")),
        );
        assert!(response.is_err());
    }

    /// A refusal, a connection closed without an answer and a refused
    /// connection are sent once, never again.
    #[test]
    fn a_refusal_or_a_lost_server_is_not_sent_again() {
        let refusals = [
            Ok(serde_json::json!({"error": {"code": "agent_not_found", "message": "no w1"}})),
            Ok(serde_json::json!({"error": {
                "code": "server_unavailable",
                "message": "server is shutting down",
            }})),
            Err(ApiClientError::EmptyResponse),
            Err(ApiClientError::Io(Error::from(
                ErrorKind::ConnectionRefused,
            ))),
        ];
        for refusal in refusals {
            let expected = refusal.as_ref().ok().cloned();
            let mut refusal = Some(refusal);
            let mut sent = 0;
            let response = send_across_handoffs(
                || {
                    sent += 1;
                    refusal.take().unwrap()
                },
                || panic!("a refusal checked a new server"),
            )
            .unwrap();
            assert_eq!(sent, 1);
            assert_eq!(response.ok(), expected);
        }
    }

    #[test]
    fn the_rest_of_a_timeout_is_sent_again() {
        // delay: not a wait, elapsed and total times of a timeout's arithmetic.
        let (total, elapsed, past) = (5000, Duration::from_millis(1200), Duration::from_secs(9));
        assert_eq!(remaining_ms(None, past), None);
        assert_eq!(remaining_ms(Some(total), elapsed), Some(3800));
        assert_eq!(remaining_ms(Some(total), past), Some(0));
    }

    /// Simulated live handoffs over a real local socket.
    #[cfg(unix)]
    mod socket {
        use super::*;
        use crate::api::schema::{AgentStatus, AgentWaitParams, Method};
        use std::io::{BufRead as _, Write as _};

        type Connection = std::io::BufReader<crate::ipc::LocalStream>;

        fn agent_wait(timeout_ms: Option<u64>) -> impl FnMut(Duration) -> Request {
            move |elapsed| Request {
                id: "cli:agent:wait".into(),
                method: Method::AgentWait(AgentWaitParams {
                    target: "w1".into(),
                    prefer_workspace_id: None,
                    until: vec![AgentStatus::Done],
                    timeout_ms: remaining_ms(timeout_ms, elapsed),
                }),
            }
        }

        /// Accepts the next connection on `listener` and reads its request.
        fn next_request(listener: &crate::ipc::LocalListener) -> (serde_json::Value, Connection) {
            use interprocess::local_socket::traits::Listener as _;
            let mut connection = std::io::BufReader::new(listener.accept().unwrap());
            let mut line = String::new();
            connection.read_line(&mut line).unwrap();
            (serde_json::from_str(&line).unwrap(), connection)
        }

        fn reply(connection: Connection, response: &serde_json::Value) {
            let mut stream = connection.into_inner();
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
        }

        fn socket(name: &str) -> std::path::PathBuf {
            let path = std::env::temp_dir().join(format!("hrc-{name}-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            path
        }

        /// A live handoff as the servers make it: the new server binds the
        /// socket path while the old one still holds the wait (its socket
        /// file removed first), then the old one answers `server_handed_off`.
        /// `new_server` answers the wait sent to the new server.
        fn across_a_handoff(
            name: &str,
            timeout_ms: Option<u64>,
            new_server: impl FnOnce(serde_json::Value, Connection) + Send + 'static,
        ) -> (std::io::Result<WaitReply>, serde_json::Value) {
            let path = socket(name);
            let old = crate::ipc::bind_private_local_listener(&path).unwrap();
            let server_path = path.clone();
            let servers = std::thread::spawn(move || {
                let (first, connection) = next_request(&old);
                std::fs::remove_file(&server_path).unwrap();
                let new = crate::ipc::bind_private_local_listener(&server_path).unwrap();
                reply(connection, &handed_off_answer());
                drop(old);
                let (second, connection) = next_request(&new);
                assert_eq!(first["method"], second["method"]);
                assert_eq!(first["params"]["target"], second["params"]["target"]);
                new_server(second.clone(), connection);
                second
            });
            let client = ApiClient::for_target(crate::api::client::ConnectionTarget::SocketPath(
                path.clone(),
            ));
            let reply = wait_on(
                &client,
                "agent wait",
                "agent w1",
                agent_wait(timeout_ms),
                |_, _| Ok(()),
            );
            let second = servers.join().unwrap();
            let _ = std::fs::remove_file(path);
            (reply, second)
        }

        /// The agent reached the status during the handoff: the same wait,
        /// sent to the new server, answers at once with it.
        #[test]
        fn a_wait_goes_on_with_the_server_a_handoff_started() {
            let reached = serde_json::json!({
                "id": "cli:agent:wait",
                "result": {"type": "agent_info", "agent": {"agent_status": "done"}},
            });
            let answer = reached.clone();
            // delay: not a wait, the timeout the wait is given.
            let timeout_ms = 600_000;
            let (reply, second) =
                across_a_handoff("handoff", Some(timeout_ms), move |_, c| reply(c, &answer));
            let reply = reply.unwrap();
            assert_eq!(reply.response, reached);
            assert!(reply.reconnected);
            assert!(second["params"]["timeout_ms"].as_u64().unwrap() <= timeout_ms);
        }

        /// The new server no longer has the target: its error answer, which
        /// names it, ends the wait.
        #[test]
        fn a_wait_ends_with_the_error_of_a_new_server_without_the_target() {
            let missing = serde_json::json!({
                "id": "cli:agent:wait",
                "error": {"code": "agent_not_found", "message": "agent w1 not found"},
            });
            let answer = missing.clone();
            let (reply, _) = across_a_handoff("vanished", None, move |_, c| reply(c, &answer));
            let reply = reply.unwrap();
            assert_eq!(reply.response, missing);
            assert!(reply.reconnected);
        }

        /// A server that closes the wait without an answer is not asked
        /// again: the wait ends with that error, not with the missing
        /// socket a second try would meet.
        #[test]
        fn a_connection_closed_without_an_answer_ends_the_wait() {
            let path = socket("closed");
            let server = crate::ipc::bind_private_local_listener(&path).unwrap();
            let server_path = path.clone();
            let servers = std::thread::spawn(move || {
                let (_, connection) = next_request(&server);
                std::fs::remove_file(&server_path).unwrap();
                drop(server);
                drop(connection);
            });
            let client = ApiClient::for_target(crate::api::client::ConnectionTarget::SocketPath(
                path.clone(),
            ));
            let reply = wait_on(
                &client,
                "agent wait",
                "agent w1",
                agent_wait(None),
                |_, _| Ok(()),
            );
            servers.join().unwrap();
            let Err(error) = reply else {
                panic!("a closed connection was answered");
            };
            assert!(!crate::cli::server_not_running_was_reported(&error));
            assert!(error.to_string().contains("empty api response"), "{error}");
        }
    }
}
