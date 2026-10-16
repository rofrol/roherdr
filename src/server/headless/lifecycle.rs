use super::*;

const LIVE_HANDOFF_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(6);

pub(super) fn wait_for_live_handoff_response_write(
    response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
) {
    let Some(response_write_complete) = response_write_complete else {
        return;
    };

    match response_write_complete.recv_timeout(LIVE_HANDOFF_RESPONSE_WRITE_TIMEOUT) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            warn!("timed out waiting for live handoff response write; old server exiting");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            warn!("live handoff response writer disconnected; old server exiting");
        }
    }
}

impl HeadlessServer {
    #[cfg(unix)]
    pub(super) fn perform_live_handoff(
        &mut self,
        params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        info!("starting live handoff");
        let import_exe = params.import_exe.as_deref().map(std::path::PathBuf::from);
        let socket_path = crate::server::handoff::handoff_socket_path();
        let token = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let listener = match crate::server::handoff::bind_listener(&socket_path) {
            Ok(listener) => listener,
            Err(err) => {
                self.handoff_in_progress = false;
                return Err(err);
            }
        };

        let mut pane_by_terminal = HashMap::new();
        for ws in &self.app.state.workspaces {
            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    pane_by_terminal.insert(pane.attached_terminal_id.clone(), pane_id.raw());
                }
            }
        }

        self.handoff_in_progress = true;
        self.disconnect_all_clients_for_handoff();
        if let Some(client_listener) = &self.client_listener {
            let _ = reject_pending_client_connections(client_listener);
        }

        let mut paused_terminal_ids = Vec::new();
        for terminal_id in pane_by_terminal.keys() {
            if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                if let Err(err) = runtime.pause_handoff_reader(Duration::from_secs(2)) {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(err);
                }
                paused_terminal_ids.push(terminal_id.clone());
            }
        }

        let snapshot = crate::persist::capture(
            &self.app.state.workspaces,
            &self.app.state.terminals,
            &self.app.terminal_runtimes,
            self.app.state.active,
            self.app.state.selected,
        );

        let mut handoff_entries = Vec::new();
        for (terminal_id, runtime) in self.app.terminal_runtimes.iter() {
            let Some(pane_id) = pane_by_terminal.get(terminal_id).copied() else {
                continue;
            };
            let mut handoff_runtime = runtime.handoff_runtime_state(pane_id);
            handoff_runtime.agent_state = self
                .app
                .state
                .terminals
                .get(terminal_id)
                .and_then(|terminal| terminal.handoff_agent_state());
            if let Some(terminal) = self.app.state.terminals.get(terminal_id) {
                let open_questions = terminal.open_questions_for_handoff();
                // The newest also goes in the single fields, for an older receiver.
                if let Some((question, since_ms)) = open_questions.last() {
                    handoff_runtime.awaiting_reply_reported = true;
                    handoff_runtime.awaiting_reply_question = question.clone();
                    handoff_runtime.awaiting_reply_since_ms = Some(*since_ms);
                }
                handoff_runtime.open_questions = open_questions
                    .into_iter()
                    .map(
                        |(question, since_ms)| crate::handoff_runtime::HandoffOpenQuestion {
                            question,
                            since_ms,
                        },
                    )
                    .collect();
                handoff_runtime.limit_report = terminal.limit_report().cloned();
                handoff_runtime.prompt_turns = terminal.prompt_turns_for_handoff();
            }
            handoff_runtime.reported_task = self
                .app
                .state
                .terminals
                .get(terminal_id)
                .and_then(|terminal| terminal.reported_task().map(str::to_owned));
            let has_agent_session = self
                .app
                .state
                .terminals
                .get(terminal_id)
                .is_some_and(|terminal| terminal.persisted_agent_session.is_some());
            if !has_agent_session {
                handoff_runtime.initial_history_ansi = runtime.handoff_history_ansi();
            }
            handoff_entries.push((terminal_id.clone(), handoff_runtime));
        }

        let panes = handoff_entries
            .iter()
            .map(|(_, runtime)| runtime.clone())
            .collect();
        let mut manifest = crate::server::handoff::manifest_for(
            snapshot,
            panes,
            params.expected_protocol,
            params.expected_version,
            self.api_window_title.clone(),
            crate::workers::envs_for_handoff(),
        );
        manifest.pty_alerts = Some(self.pty_alerts.handoff());
        manifest.public_listeners = self.public_listeners_offer();
        manifest.server_lock =
            self.server_lock
                .as_ref()
                .map(|lock| crate::server::handoff::ServerLockOffer {
                    path: lock.path().to_path_buf(),
                });
        let mut import_child = match crate::server::handoff::spawn_handoff_import(
            import_exe.as_deref(),
            &socket_path,
            &token,
        ) {
            Ok(child) => child,
            Err(err) => {
                self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                return Err(err);
            }
        };
        let child_pid = import_child.id();
        info!(pid = child_pid, socket = %socket_path.display(), "spawned handoff import server");

        let mut fds = Vec::new();
        let duplicate_result = (|| {
            for (terminal_id, _) in &handoff_entries {
                let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) else {
                    continue;
                };
                fds.push(runtime.duplicate_handoff_fd()?);
            }
            Ok::<(), io::Error>(())
        })();
        if let Err(err) = duplicate_result {
            for fd in fds {
                let _ = unsafe { libc::close(fd) };
            }
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
            return Err(err);
        }

        let (mut stream, takes) = match crate::server::handoff::accept_and_validate_on(
            listener,
            &socket_path,
            &token,
            &manifest,
        ) {
            Ok(accepted) => accepted,
            Err(err) => {
                for fd in fds {
                    let _ = unsafe { libc::close(fd) };
                }
                crate::server::handoff::cleanup_failed_import_child(&mut import_child);
                self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                return Err(err);
            }
        };

        if takes.public_sockets == crate::server::handoff::PublicSocketsHandoff::UnlinkOnRestored {
            self.hand_off_public_sockets_by_path(
                &mut stream,
                &fds,
                &mut import_child,
                &socket_path,
                &paused_terminal_ids,
                takes.server_lock,
            )?;
        } else {
            self.pass_public_listeners(
                &mut stream,
                &fds,
                &mut import_child,
                &socket_path,
                &paused_terminal_ids,
                takes.server_lock,
            )?;
        }
        if takes.server_lock {
            // Committed: the new server holds the lock now, through the
            // descriptor it was passed. An import server from before the
            // lock took none; this server keeps it until it exits, and
            // nothing holds it for that server's life.
            self.server_lock = None;
        }

        for (terminal_id, runtime) in self.app.terminal_runtimes.drain_for_handoff() {
            if !pane_by_terminal.contains_key(&terminal_id) {
                continue;
            }
            debug!(terminal = %terminal_id, "preserving pane runtime for handoff");
            runtime.preserve_for_handoff();
        }
        crate::server::handoff::wait_owned_ack(&mut stream);

        Ok(())
    }

    pub(super) fn finish_live_handoff_shutdown(&mut self) {
        // The new server holds the API socket (and accepts on it once it
        // took ownership): each open wait this server ends from now on tells
        // its client to go on with it.
        if let Some(api_server) = &self.api_server {
            api_server.mark_handed_off();
        }
        self.shutting_down = true;
        self.app.state.should_quit = true;
        self.app.policy.persist_session = false;
        info!("live handoff completed; old server exiting");
    }

    #[cfg(not(unix))]
    pub(super) fn perform_live_handoff(
        &mut self,
        _params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        Err(io::Error::other("live handoff is only supported on Unix"))
    }

    /// What this server offers to pass to an import server: its listening
    /// API and client sockets, while both files at their paths are still its
    /// own. Without the offer, the import server takes the paths over.
    #[cfg(unix)]
    fn public_listeners_offer(&self) -> Option<crate::server::handoff::PublicListenersOffer> {
        use crate::server::handoff::{PassedSocketFile, PublicListenersOffer, SocketFileId};

        let api_server = self.api_server.as_ref()?;
        api_server.listener_fd()?;
        self.client_listener.as_ref()?;
        let api_path = api_server.socket_path().to_path_buf();
        let api_file = SocketFileId::of(&api_path).ok()?;
        let client_file = SocketFileId::of(&self.client_socket_path).ok()?;
        let client_own = socket_file_identity(&self.client_socket_path)
            .is_ok_and(|current| current == self.client_socket_identity);
        if !api_server.socket_file_is_own() || !client_own {
            return None;
        }
        Some(PublicListenersOffer {
            api: PassedSocketFile {
                path: api_path,
                file: api_file,
            },
            client: PassedSocketFile {
                path: self.client_socket_path.clone(),
                file: client_file,
            },
        })
    }

    /// The handoff to an import server that takes the listening sockets
    /// (`handoff::pass_public_listeners`): no path is unlinked, renamed or
    /// bound, a failure resumes accepting on the same listeners, and after
    /// the commit this server closes its copies and never accepts on them.
    #[cfg(unix)]
    fn pass_public_listeners(
        &mut self,
        stream: &mut std::os::unix::net::UnixStream,
        fds: &[std::os::fd::RawFd],
        import_child: &mut std::process::Child,
        socket_path: &Path,
        paused_terminal_ids: &[crate::terminal::TerminalId],
        pass_server_lock: bool,
    ) -> io::Result<()> {
        let mut listeners = OwnListeners {
            api_server: &mut self.api_server,
            client_listener: &mut self.client_listener,
        };
        let server_lock = pass_server_lock
            .then(|| self.server_lock.as_ref().map(|lock| lock.as_fd()))
            .flatten();
        let handed_off = crate::server::handoff::pass_public_listeners(
            stream,
            fds,
            &mut listeners,
            server_lock,
            || crate::server::handoff::cleanup_failed_import_child(import_child),
            &mut |_| {},
        );
        for fd in fds {
            let _ = unsafe { libc::close(*fd) };
        }
        if let Err(err) = handed_off {
            self.rollback_handoff_before_commit(socket_path, paused_terminal_ids);
            return Err(err);
        }
        // The import server's file now, as the listener is.
        self.owns_client_socket_file = false;
        Ok(())
    }

    /// The handoff to an import server from before the listener transfer, as
    /// that build's source did it: unlink both paths (each only if still
    /// this server's file) on `restored`, for the import server to bind; on
    /// a failure, reap it and bind both paths again.
    #[cfg(unix)]
    fn hand_off_public_sockets_by_path(
        &mut self,
        stream: &mut std::os::unix::net::UnixStream,
        fds: &[std::os::fd::RawFd],
        import_child: &mut std::process::Child,
        socket_path: &Path,
        paused_terminal_ids: &[crate::terminal::TerminalId],
        pass_server_lock: bool,
    ) -> io::Result<()> {
        let server_lock = pass_server_lock
            .then(|| self.server_lock.as_ref().map(|lock| lock.as_fd()))
            .flatten();
        let send_result =
            crate::server::handoff::send_fds_and_wait_restored(stream, fds, server_lock);
        for fd in fds {
            let _ = unsafe { libc::close(*fd) };
        }
        if let Err(err) = send_result {
            crate::server::handoff::cleanup_failed_import_child(import_child);
            self.rollback_handoff_before_commit(socket_path, paused_terminal_ids);
            return Err(err);
        }

        if let Some(api_server) = &self.api_server {
            let _ = api_server.remove_socket_file_if_owned();
        }
        let _ = remove_socket_file_if_owned(&self.client_socket_path, &self.client_socket_identity);
        let handed_off = crate::server::handoff::wait_ready(stream)
            .map_err(|err| {
                io::Error::other(format!(
                    "handoff replacement server did not become ready: {err}"
                ))
            })
            .and_then(|()| {
                crate::server::handoff::report_committed(stream).map_err(|err| {
                    io::Error::other(format!(
                        "handoff replacement server was ready, but commit failed: {err}"
                    ))
                })
            });
        if let Err(err) = handed_off {
            // Reaped first: its listeners are closed once it is gone, so
            // only another process can still accept on the paths.
            crate::server::handoff::cleanup_failed_import_child(import_child);
            let restored = self.restore_public_sockets_after_failed_handoff();
            self.rollback_handoff_before_commit(socket_path, paused_terminal_ids);
            return Err(match restored {
                Ok(()) => err,
                Err(restore_err) => io::Error::other(format!(
                    "{err}; old server could not restore public sockets: {restore_err}"
                )),
            });
        }
        // The paths are the import server's files now, which may reuse the
        // inodes of the unlinked ones: neither listener may remove them.
        if let Some(api_server) = self.api_server.as_mut() {
            api_server.release_listener();
        }
        self.client_listener = None;
        self.owns_client_socket_file = false;
        Ok(())
    }

    /// Binds both public paths again after a by-path handoff failed. A path
    /// another process accepts on is refused (`AddrInUse`).
    #[cfg(unix)]
    fn restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let api_tx = self
            .api_tx
            .clone()
            .ok_or_else(|| io::Error::other("cannot restore api socket without api sender"))?;
        let api_server = api::start_server_with_stop_control(
            api_tx,
            self.app.event_hub.clone(),
            self.server_stop.clone(),
        )?;
        // The old handle's file is gone; its drop must not remove the new
        // one, which may reuse the inode.
        if let Some(mut old) = self.api_server.replace(api_server) {
            old.release_listener();
        }

        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;
        let listener = bind_local_listener(&client_path)?;
        restrict_socket_permissions(&client_path)?;
        let client_socket_identity = socket_file_identity(&client_path)?;
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        self.client_listener = Some(listener);
        self.client_socket_path = client_path;
        self.client_socket_identity = client_socket_identity;
        Ok(())
    }

    #[cfg(unix)]
    fn rollback_handoff_before_commit(
        &mut self,
        socket_path: &Path,
        paused_terminal_ids: &[crate::terminal::TerminalId],
    ) {
        for terminal_id in paused_terminal_ids {
            if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                runtime.set_handoff_reader_paused(false);
            }
        }
        self.handoff_in_progress = false;
        let _ = std::fs::remove_file(socket_path);
    }

    #[cfg(unix)]
    pub(super) fn nudge_handoff_panes_on_first_client_attach(&mut self) {
        if !self.pending_handoff_repaint_nudge {
            return;
        }
        self.pending_handoff_repaint_nudge = false;
        info!("nudging pane redraws after handoff");
        self.app
            .terminal_runtimes
            .nudge_child_redraw_after_handoff();
    }

    #[cfg(not(unix))]
    pub(super) fn nudge_handoff_panes_on_first_client_attach(&mut self) {}

    /// Panes imported by a handoff keep the sizes the previous server gave
    /// them until a client attaches. Resizing them to the default geometry in
    /// between would discard the restored screens and make every full-screen
    /// program redraw twice, first for the default size and then for the
    /// client's.
    #[cfg(unix)]
    pub(super) fn keeps_imported_pane_sizes(&self) -> bool {
        self.pending_handoff_repaint_nudge
    }

    #[cfg(not(unix))]
    pub(super) fn keeps_imported_pane_sizes(&self) -> bool {
        false
    }
    /// Initiates graceful shutdown.
    pub(super) fn initiate_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        let reason = if self.host_shutdown_requested.load(Ordering::Acquire) {
            ShutdownReason::HostShutdown
        } else {
            self.server_stop
                .take_reason()
                .unwrap_or(ShutdownReason::Unknown)
        };
        info!(%reason, "server shutdown initiated");
        self.shutting_down = true;

        // Clear client-local host graphics, then send ServerShutdown to all connected clients.
        let shutdown_msg = ServerMessage::ServerShutdown {
            reason: Some("server is shutting down".to_owned()),
        };
        self.send_to_all_clients(shutdown_msg);

        // Give client writer threads a moment to flush the shutdown message.
        // A short sleep ensures the message is written to the socket before
        // we close the connections.
        std::thread::sleep(Duration::from_millis(50));

        // Signal the main loop to exit.
        self.should_quit.store(true, Ordering::Release);
        self.app.state.should_quit = true;
    }

    /// Completes the shutdown sequence: send ServerShutdown to clients,
    /// close client connections, remove socket files, and clean up.
    pub(super) async fn complete_shutdown(&mut self) -> io::Result<()> {
        info!("completing server shutdown");
        self.reject_late_client_connections().await;

        // Send ServerShutdown to all remaining clients.
        if !self.clients.is_empty() {
            let shutdown_msg = ServerMessage::ServerShutdown {
                reason: Some("server is shutting down".to_owned()),
            };
            self.send_to_all_clients(shutdown_msg);

            // Give writer threads a moment to flush before closing.
            std::thread::sleep(Duration::from_millis(50));
        }

        // Reject only the requests already queued when shutdown reached cleanup.
        self.reject_queued_api_requests_for_shutdown();

        // Close all client connections.
        let staged_files = self
            .clients
            .drain()
            .flat_map(|(_, client)| client.staged_clipboard_files)
            .collect::<Vec<_>>();
        crate::server::clipboard_image::remove_files(staged_files);

        // Remove socket files.
        self.cleanup_sockets()?;

        Ok(())
    }

    /// Removes socket files created by the server.
    pub(super) fn cleanup_sockets(&self) -> io::Result<()> {
        // Its listener passed to a live handoff's new server, which listens
        // on the same file.
        if !self.owns_client_socket_file {
            return Ok(());
        }
        if let Err(err) =
            remove_socket_file_if_owned(&self.client_socket_path, &self.client_socket_identity)
        {
            if err.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %self.client_socket_path.display(),
                    err = %err,
                    "failed to remove client socket on shutdown"
                );
            }
        }
        Ok(())
    }
}

/// This server's public listeners, as a listener handoff passes them. The
/// event loop, which accepts clients, waits for the handoff, so only the
/// API's accept thread is paused.
#[cfg(unix)]
struct OwnListeners<'a> {
    api_server: &'a mut Option<api::ServerHandle>,
    client_listener: &'a mut Option<LocalListener>,
}

#[cfg(unix)]
impl crate::server::handoff::SourceListeners for OwnListeners<'_> {
    fn pause_accepting(&mut self) {
        if let Some(api_server) = self.api_server.as_ref() {
            api_server.pause_accepting();
        }
    }

    fn resume_accepting(&mut self) {
        if let Some(api_server) = self.api_server.as_ref() {
            api_server.resume_accepting();
        }
    }

    fn listener_fds(&self) -> io::Result<[std::os::fd::BorrowedFd<'_>; 2]> {
        let api = self
            .api_server
            .as_ref()
            .and_then(|api_server| api_server.listener_fd())
            .ok_or_else(|| io::Error::other("the api listener is not this server's"))?;
        let Some(LocalListener::UdSocket(client)) = self.client_listener.as_ref() else {
            return Err(io::Error::other("the client listener is not this server's"));
        };
        Ok([api, std::os::fd::AsFd::as_fd(client)])
    }

    fn release(&mut self) {
        if let Some(api_server) = self.api_server.as_mut() {
            api_server.release_listener();
        }
        *self.client_listener = None;
    }
}
