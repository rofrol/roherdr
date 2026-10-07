use super::*;
use crate::api::schema::{
    AgentStatus, EventData, EventEnvelope, EventKind, PaneInfo, PaneReadResult, ReadFormat,
    ReadSource,
};
use crate::ipc::{poll_local_stream_read_count, LocalStreamReadCount};
use interprocess::local_socket::traits::Listener as _;
use serde_json::{json, Value};
use std::sync::atomic::AtomicU64;
use tokio::sync::mpsc;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

struct SocketTest {
    hub: EventHub,
    running: Arc<AtomicBool>,
    api_tx: ApiRequestSender,
    api_rx: mpsc::UnboundedReceiver<ApiRequestMessage>,
    workers: Vec<std::thread::JoinHandle<io::Result<()>>>,
    paths: Vec<PathBuf>,
}

impl SocketTest {
    fn new() -> Self {
        let (api_tx, api_rx) = mpsc::unbounded_channel();
        Self {
            hub: EventHub::default(),
            running: Arc::new(AtomicBool::new(true)),
            api_tx,
            api_rx,
            workers: Vec::new(),
            paths: Vec::new(),
        }
    }

    fn connect(&mut self) -> Client {
        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "herdr-sub-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = bind_local_listener(&path).unwrap();
        self.paths.push(path.clone());
        let mut stream = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        set_local_stream_polling(&mut stream, true).unwrap();
        let api_tx = self.api_tx.clone();
        let hub = self.hub.clone();
        let running = Arc::clone(&self.running);
        let worker =
            std::thread::spawn(move || handle_connection(server, &api_tx, &hub, &running, None));
        self.workers.push(worker);
        Client {
            stream,
            buffered: Vec::new(),
        }
    }

    fn app_request(&mut self) -> ApiRequestMessage {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            match self.api_rx.try_recv() {
                Ok(request) => return request,
                Err(mpsc::error::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for app request"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("app request channel closed: {error}"),
            }
        }
    }
}

impl Drop for SocketTest {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.api_rx.close();
        while self.api_rx.try_recv().is_ok() {}
        let deadline = Instant::now() + APP_RESPONSE_TIMEOUT + Duration::from_secs(1);
        let mut failures = Vec::new();
        for worker in self.workers.drain(..) {
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if worker.is_finished() {
                match worker.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => failures.push(format!("connection failed: {error}")),
                    Err(_) => failures.push("connection panicked".into()),
                }
            } else {
                failures.push("subscription connection did not stop".into());
            }
        }
        // Also remove Windows listener marker files, including after a setup failure.
        for path in self.paths.drain(..) {
            if let Err(error) = std::fs::remove_file(path) {
                if error.kind() != io::ErrorKind::NotFound {
                    failures.push(format!("socket cleanup failed: {error}"));
                }
            }
        }
        if !std::thread::panicking() {
            assert!(failures.is_empty(), "{failures:?}");
        }
    }
}

struct Client {
    stream: LocalStream,
    buffered: Vec<u8>,
}

impl Client {
    fn send(&mut self, request: Value) {
        writeln!(self.stream, "{request}").unwrap();
    }

    fn subscribe(&mut self, id: &str, subscriptions: Value) {
        self.send(json!({
            "id": id,
            "method": "events.subscribe",
            "params": {"subscriptions": subscriptions}
        }));
    }

    fn next_line(&mut self, deadline: Instant) -> Option<Value> {
        // LocalStream recv timeouts are unsupported on Windows. Use the same bounded
        // nonblocking/PeekNamedPipe reads as the API client, retaining partial JSON lines.
        loop {
            if let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') {
                let line: Vec<_> = self.buffered.drain(..=end).collect();
                return Some(serde_json::from_slice(&line).expect("subscription JSON"));
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for socket response"
            );
            let mut bytes = [0; 4096];
            match poll_local_stream_read_count(&mut self.stream, &mut bytes).unwrap() {
                LocalStreamReadCount::Data(count) => {
                    self.buffered.extend_from_slice(&bytes[..count])
                }
                LocalStreamReadCount::Pending => std::thread::sleep(Duration::from_millis(1)),
                LocalStreamReadCount::Closed => {
                    assert!(self.buffered.is_empty(), "incomplete JSON at EOF");
                    return None;
                }
            }
        }
    }

    fn response(&mut self) -> Value {
        self.next_line(Instant::now() + RESPONSE_TIMEOUT)
            .expect("socket response before EOF")
    }

    fn assert_started(&mut self, id: &str) {
        let response = self.response();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["type"], "subscription_started");
    }

    fn assert_renames(&mut self, indices: std::ops::Range<usize>, deadline: Instant) {
        for index in indices {
            let event = self.next_line(deadline).expect("rename before EOF");
            assert_eq!(event["event"], "workspace_renamed");
            assert_eq!(event["data"]["label"], format!("flood-{index}"));
        }
    }

    fn assert_history_lost(&mut self, id: &str) {
        let response = self.response();
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], "events_lost", "{response}");
        assert_eq!(self.next_line(Instant::now() + RESPONSE_TIMEOUT), None);
    }
}

fn renamed_event(index: usize) -> EventEnvelope {
    EventEnvelope {
        event: EventKind::WorkspaceRenamed,
        data: EventData::WorkspaceRenamed {
            workspace_id: "workspace_1".into(),
            label: format!("flood-{index}"),
        },
    }
}

fn output_subscription() -> Value {
    json!({
        "type": "pane.output_matched",
        "pane_id": "pane_1",
        "source": "recent",
        "match": {"type": "substring", "value": "never"}
    })
}

fn reply(request: ApiRequestMessage, result: ResponseResult) {
    request
        .respond_to
        .send(
            serde_json::to_string(&SuccessResponse {
                id: request.request.id,
                result,
            })
            .unwrap(),
        )
        .unwrap();
}

fn test_pane_info() -> PaneInfo {
    PaneInfo {
        pane_id: "pane_1".into(),
        terminal_id: "term_1".into(),
        workspace_id: "workspace_1".into(),
        tab_id: "tab_1".into(),
        focused: true,
        cwd: None,
        foreground_cwd: None,
        restore_error: None,
        label: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        display_agent: None,
        agent_status: AgentStatus::Working,
        state_labels: Default::default(),
        tokens: Default::default(),
        agent_session: None,
        scroll: None,
        revision: 0,
    }
}

fn reply_to_probe(request: ApiRequestMessage) {
    let result = match request.request.method {
        Method::PaneGet(_) => ResponseResult::PaneInfo {
            pane: test_pane_info(),
        },
        Method::PaneRead(_) => ResponseResult::PaneRead {
            read: PaneReadResult {
                pane_id: "pane_1".into(),
                workspace_id: "workspace_1".into(),
                tab_id: "tab_1".into(),
                source: ReadSource::RecentUnwrapped,
                format: ReadFormat::Text,
                text: String::new(),
                revision: 0,
                truncated: false,
            },
        },
        ref other => panic!("unexpected subscription probe: {other:?}"),
    };
    reply(request, result);
}

#[test]
fn subscriptions_drain_retained_bursts_without_per_event_poll_delay() {
    let mut test = SocketTest::new();
    let mut client = test.connect();
    client.subscribe("burst", json!([{"type": "workspace.renamed"}]));
    client.assert_started("burst");
    for index in 0..128 {
        test.hub.push(renamed_event(index));
    }
    // One deadline for the entire batch detects a 100 ms delay per event.
    client.assert_renames(0..128, Instant::now() + RESPONSE_TIMEOUT);
}

#[test]
fn subscriptions_report_history_loss_before_sending_a_partial_stream() {
    assert_subscription_history_loss(false);
}

#[test]
fn subscriptions_report_history_loss_before_initial_agent_status() {
    assert_subscription_history_loss(true);
}

fn assert_subscription_history_loss(agent_status: bool) {
    let mut test = SocketTest::new();
    let mut client = test.connect();
    let subscriptions = if agent_status {
        json!([{
            "type": "pane.agent_status_changed",
            "pane_id": "pane_1",
            "agent_status": "working"
        }])
    } else {
        json!([{"type": "workspace.renamed"}, output_subscription()])
    };
    client.subscribe("history-gap", subscriptions);
    // Hold the setup probe after the server pins its subscription cursor.
    let probe = test.app_request();
    assert!(probe.request.id.ends_with(":probe"));
    for index in 0..600 {
        test.hub.push(renamed_event(index));
    }
    reply_to_probe(probe);
    client.assert_started("history-gap");
    client.assert_history_lost("history-gap");
}

#[test]
fn lagging_subscription_closes_without_interrupting_other_clients() {
    let mut test = SocketTest::new();
    let mut healthy = test.connect();
    healthy.subscribe("healthy", json!([{"type": "workspace.renamed"}]));
    healthy.assert_started("healthy");

    let mut slow = test.connect();
    slow.subscribe(
        "slow",
        json!([{"type": "workspace.renamed"}, output_subscription()]),
    );
    let probe = test.app_request();
    assert_eq!(probe.request.id, "slow:sub:1:probe");
    reply_to_probe(probe);
    slow.assert_started("slow");
    // Pause only this connection in an existing app request, rather than depending
    // on OS socket buffer sizes or sleeping to make its event cursor fall behind.
    let paused_read = test.app_request();
    assert_eq!(paused_read.request.id, "slow:sub:1:read");
    assert!(matches!(paused_read.request.method, Method::PaneRead(_)));
    // All five batches must finish before the held app request can time out.
    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    for batch in 0..5 {
        let indices = batch * 128..(batch + 1) * 128;
        for index in indices.clone() {
            test.hub.push(renamed_event(index));
        }
        healthy.assert_renames(indices, deadline);
    }

    reply_to_probe(paused_read);
    slow.assert_history_lost("slow");
    test.hub.push(renamed_event(640));
    healthy.assert_renames(640..641, Instant::now() + RESPONSE_TIMEOUT);

    let mut ordinary = test.connect();
    ordinary.send(json!({"id": "ordinary", "method": "workspace.list", "params": {}}));
    let request = test.app_request();
    assert_eq!(request.request.id, "ordinary");
    assert!(matches!(request.request.method, Method::WorkspaceList(_)));
    reply(
        request,
        ResponseResult::WorkspaceList {
            workspaces: Vec::new(),
        },
    );
    let response = ordinary.response();
    assert_eq!(response["id"], "ordinary");
    assert_eq!(response["result"]["type"], "workspace_list");
}

// `agent.wait_turn`: the app answers each `agent.prompt_status` read; events about the pane
// make the wait read again.

fn wait_turn(client: &mut Client, request_id: &str) {
    client.send(json!({
        "id": "wait",
        "method": "agent.wait_turn",
        "params": {"request_id": request_id}
    }));
}

fn reply_prompt_state(test: &mut SocketTest, state: &str, error: Option<&str>) {
    let request = test.app_request();
    let Method::AgentPromptStatus(params) = &request.request.method else {
        panic!(
            "expected a prompt status read: {:?}",
            request.request.method
        );
    };
    assert_eq!(params.request_id, "prompt_1");
    let mut prompt_request = json!({"request_id": "prompt_1", "state": state});
    if let Some(error) = error {
        prompt_request["error"] = error.into();
    }
    reply(
        request,
        ResponseResult::AgentPromptStatus {
            pane_id: "pane_1".into(),
            prompt_request: serde_json::from_value(prompt_request).unwrap(),
        },
    );
}

fn assert_turn_ended(client: &mut Client, reason: &str) -> Value {
    let response = client.response();
    assert_eq!(response["id"], "wait");
    assert_eq!(response["result"]["type"], "agent_turn_ended", "{response}");
    assert_eq!(response["result"]["reason"], reason, "{response}");
    assert_eq!(response["result"]["request_id"], "prompt_1");
    response
}

fn pane_updated() -> EventEnvelope {
    EventEnvelope {
        event: EventKind::PaneUpdated,
        data: EventData::PaneUpdated {
            pane: test_pane_info(),
        },
    }
}

#[test]
fn wait_turn_answers_at_once_for_a_turn_that_ended_before_it() {
    let mut test = SocketTest::new();
    let mut client = test.connect();
    wait_turn(&mut client, "prompt_1");
    reply_prompt_state(&mut test, "finished", None);
    let response = assert_turn_ended(&mut client, "finished");
    assert_eq!(response["result"]["pane_id"], "pane_1");
}

#[test]
fn wait_turn_reads_again_on_a_pane_event_and_reports_each_end() {
    for (state, error, reason) in [
        ("finished", None, "finished"),
        ("failed", Some("server_error: overloaded"), "failed"),
        ("interrupted", None, "interrupted"),
        ("exited", None, "exited"),
    ] {
        let mut test = SocketTest::new();
        let mut client = test.connect();
        wait_turn(&mut client, "prompt_1");
        reply_prompt_state(&mut test, "working", None);
        test.hub.push(pane_updated());
        reply_prompt_state(&mut test, state, error);
        let response = assert_turn_ended(&mut client, reason);
        match error {
            Some(error) => assert_eq!(response["result"]["error"], error),
            None => assert!(response["result"].get("error").is_none()),
        }
    }
}

#[test]
fn wait_turn_sees_a_change_made_while_its_first_read_was_answered() {
    let mut test = SocketTest::new();
    let mut client = test.connect();
    wait_turn(&mut client, "prompt_1");
    let first = test.app_request();
    // The turn ends after the app read the request but before the wait got the answer.
    test.hub.push(pane_updated());
    reply(
        first,
        ResponseResult::AgentPromptStatus {
            pane_id: "pane_1".into(),
            prompt_request: serde_json::from_value(
                json!({"request_id": "prompt_1", "state": "working"}),
            )
            .unwrap(),
        },
    );
    reply_prompt_state(&mut test, "finished", None);
    assert_turn_ended(&mut client, "finished");
}

#[test]
fn wait_turn_reports_exited_when_the_pane_exits_during_the_turn() {
    let mut test = SocketTest::new();
    let mut client = test.connect();
    wait_turn(&mut client, "prompt_1");
    reply_prompt_state(&mut test, "working", None);
    test.hub.push(EventEnvelope {
        event: EventKind::PaneExited,
        data: EventData::PaneExited {
            pane_id: "pane_1".into(),
            workspace_id: "workspace_1".into(),
        },
    });
    reply_prompt_state(&mut test, "working", None);
    assert_turn_ended(&mut client, "exited");
}

#[test]
fn wait_turn_reports_a_request_the_server_does_not_know() {
    // A request id from before a server restart: the new server never saw it.
    let mut test = SocketTest::new();
    let mut client = test.connect();
    wait_turn(&mut client, "prompt_1");
    let request = test.app_request();
    request
        .respond_to
        .send(
            json!({
                "id": request.request.id,
                "error": {"code": "prompt_request_not_found", "message": "no followed prompt"}
            })
            .to_string(),
        )
        .unwrap();
    let response = assert_turn_ended(&mut client, "unknown_request");
    assert!(response["result"].get("pane_id").is_none());
}
