//! A client shell's worker tab: the server pushes the worker's transcript
//! (`endpoint.worker-transcript.v1`) to the shell that asked for it with
//! `client_shell.worker_transcript.set`, first what the shell did not have,
//! then each new journal line as the worker writes it.

use super::*;
use crate::server::clients::ShellWorkerTranscript;
use crate::workers::transcript::JournalPosition;
use crate::workers::WorkerError;

impl HeadlessServer {
    /// Points the shell's worker tab at a worker (or at none) and sends it
    /// what it lacks of the transcript at once.
    pub(super) fn set_client_worker_transcript(
        &mut self,
        client_id: u64,
        params: &api::schema::ClientShellWorkerTranscriptSetParams,
    ) -> Result<(), WorkerError> {
        let watch = match &params.worker_id {
            Some(worker_id) => {
                let supervisor = crate::workers::supervisor();
                // Refused before the shell is pointed at it.
                supervisor.status(worker_id)?;
                Some(ShellWorkerTranscript {
                    worker_id: worker_id.clone(),
                    run_id: supervisor.todo_run_of_worker(worker_id),
                    position: JournalPosition::default(),
                    after: params.after,
                    sent: None,
                })
            }
            None => None,
        };
        let Some(client) = self.clients.get_mut(&client_id) else {
            return Ok(());
        };
        client.shell_worker_transcript = watch;
        self.push_worker_transcript(client_id);
        Ok(())
    }

    /// Sends every shell that shows a worker's tab the lines its worker's
    /// journal got since the last message.
    pub(super) fn push_worker_transcripts(&mut self) {
        let watching: Vec<u64> = self
            .clients
            .iter()
            .filter(|(_, client)| client.shell_worker_transcript.is_some())
            .map(|(client_id, _)| *client_id)
            .collect();
        for client_id in watching {
            self.push_worker_transcript(client_id);
        }
    }

    fn push_worker_transcript(&mut self, client_id: u64) {
        let Some(watch) = self
            .clients
            .get(&client_id)
            .and_then(|client| client.shell_worker_transcript.clone())
        else {
            return;
        };
        let supervisor = crate::workers::supervisor();
        let worker = match supervisor.status(&watch.worker_id) {
            Ok(worker) => worker,
            Err(error) => {
                debug!(client_id, worker = watch.worker_id, %error, "worker tab has no worker");
                return;
            }
        };
        let (transcript, position) = match supervisor.transcript_from(
            &worker,
            watch.run_id.clone(),
            watch.position,
            watch.after,
            None,
        ) {
            Ok(read) => read,
            Err(error) => {
                warn!(client_id, worker = watch.worker_id, %error, "worker transcript unreadable");
                return;
            }
        };
        let shown = (transcript.state, transcript.tab.clone());
        if transcript.events.is_empty() && watch.sent.as_ref() == Some(&shown) {
            if let Some(client) = self.clients.get_mut(&client_id) {
                if let Some(current) = client.shell_worker_transcript.as_mut() {
                    current.position = position;
                }
            }
            return;
        }
        let message = crate::protocol::endpoint::EndpointWorkerTranscript {
            boot_id: self.client_shell_boot_id.clone(),
            transcript,
        };
        let message = match crate::protocol::endpoint::worker_transcript_message(&message) {
            Ok(message) => message,
            Err(error) => {
                warn!(client_id, %error, "failed to encode worker transcript");
                return;
            }
        };
        if !self.send_to_client(client_id, message) {
            return;
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            // Only if the shell did not point its tab elsewhere meanwhile.
            if let Some(current) = client
                .shell_worker_transcript
                .as_mut()
                .filter(|current| current.worker_id == watch.worker_id)
            {
                current.position = position;
                current.sent = Some(shown);
            }
        }
    }
}
