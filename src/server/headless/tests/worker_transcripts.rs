//! A client shell's worker tab: `client_shell.worker_transcript.set` gets
//! the shell the transcript it lacks, then each new journal line, until it
//! names no worker.

use super::*;

fn connect_shell(
    server: &mut HeadlessServer,
    client_id: u64,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (writer, control_rx, _render_rx) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientShellConnected {
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        client_id,
        surface_cols: 100,
        surface_rows: 30,
        cell_width_px: 8,
        cell_height_px: 16,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: true,
        mouse_capture: true,
        surface_active: true,
        writer,
    });
    let _ = client_shell_snapshot(&control_rx);
    control_rx
}

fn set_transcript(
    server: &mut HeadlessServer,
    client_id: u64,
    request_id: &str,
    worker_id: Option<&str>,
    after: u64,
) {
    let boot_id = server.client_shell_boot_id.clone();
    server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
        client_id,
        boot_id,
        request: Box::new(api::schema::Request {
            id: request_id.into(),
            method: api::schema::Method::ClientShellWorkerTranscriptSet(
                api::schema::ClientShellWorkerTranscriptSetParams {
                    worker_id: worker_id.map(str::to_owned),
                    after,
                },
            ),
        }),
    });
}

type Transcript = crate::protocol::endpoint::EndpointWorkerTranscript;

/// The transcripts the shell was sent up to the next endpoint response, and
/// that response. The server queues both before the handler returns, and the
/// test writer's drain thread forwards them in order on its own, so this
/// blocks on the channel until the response itself arrives.
fn until_response(
    control_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> (Vec<Transcript>, serde_json::Value) {
    let mut transcripts = Vec::new();
    loop {
        let bytes = control_rx.recv().expect("endpoint response");
        match read_server_message(bytes) {
            ServerMessage::EndpointControl { kind, data }
                if kind == protocol::endpoint::WORKER_TRANSCRIPT_KIND =>
            {
                transcripts.push(serde_json::from_str(&data).expect("worker transcript"));
            }
            ServerMessage::ClientShellEndpointResponseChunk { data, .. } => {
                return (
                    transcripts,
                    serde_json::from_slice(&data).expect("endpoint response"),
                );
            }
            _ => {}
        }
    }
}

/// The transcripts the shell was sent before this call. A request naming an
/// unknown worker is refused before it changes the shell's tab, so its
/// response marks the end of what was queued before it.
fn sent_before_fence(
    server: &mut HeadlessServer,
    client_id: u64,
    control_rx: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<Transcript> {
    set_transcript(server, client_id, "fence", Some("w999"), 0);
    let (transcripts, response) = until_response(control_rx);
    assert_eq!(response["error"]["code"], "worker_not_found", "{response}");
    transcripts
}

#[tokio::test]
async fn a_worker_tab_gets_its_transcript_then_each_new_line_until_it_stops() {
    let worker = crate::workers::FinishedWorker::new("endpoint-tab");
    crate::workers::set_test_supervisor(worker.supervisor().clone());
    let mut server = test_headless_server();
    let client_id = 61;
    let control_rx = connect_shell(&mut server, client_id);

    // Asked from line 1 on: everything after it, tagged and with the tab.
    set_transcript(&mut server, client_id, "set-1", Some(&worker.worker_id), 1);
    let (transcripts, response) = until_response(&control_rx);
    assert!(response["result"].is_object(), "{response}");
    let [first] = transcripts.as_slice() else {
        panic!("one transcript: {transcripts:?}");
    };
    assert_eq!(first.boot_id, server.client_shell_boot_id);
    let transcript = &first.transcript;
    assert_eq!(transcript.worker_id, worker.worker_id);
    assert_eq!(
        transcript.tab.tab_id,
        format!("worker:{}", worker.worker_id)
    );
    assert_eq!(transcript.tab.pane_kind, api::schema::PaneKind::Worker);
    assert!(transcript.tab.read_only);
    assert_eq!(transcript.cursor, worker.journal_lines());
    assert!(!transcript.events.is_empty());
    assert!(transcript
        .events
        .iter()
        .all(|event| event.line > 1 && event.worker_id == worker.worker_id));

    // A new turn's lines follow, and only those.
    let cursor = transcript.cursor;
    worker.run_another_turn();
    server.push_worker_transcripts();
    let transcripts = sent_before_fence(&mut server, client_id, &control_rx);
    let lines: Vec<u64> = transcripts
        .iter()
        .flat_map(|sent| sent.transcript.events.iter().map(|event| event.line))
        .collect();
    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| *line > cursor), "{lines:?}");
    assert_eq!(
        transcripts.last().map(|sent| sent.transcript.cursor),
        Some(worker.journal_lines())
    );
    // Nothing new: nothing sent.
    server.push_worker_transcripts();
    assert!(sent_before_fence(&mut server, client_id, &control_rx).is_empty());

    // Naming no worker stops it.
    set_transcript(&mut server, client_id, "set-2", None, 0);
    let (transcripts, response) = until_response(&control_rx);
    assert!(transcripts.is_empty());
    assert!(response["result"].is_object(), "{response}");
    worker.run_another_turn();
    server.push_worker_transcripts();
    assert!(sent_before_fence(&mut server, client_id, &control_rx).is_empty());
    shutdown_test_runtimes(&mut server);
}
