use super::*;

/// Run the headless server. This is the entry point called from main.rs.
pub fn run_server() -> io::Result<()> {
    crate::platform::ignore_server_hangup();
    // Before anything this process starts could inherit the pipe.
    let mut start_reporter = crate::server::start_report::StartReporter::from_starter();
    let args: Vec<String> = std::env::args().collect();
    let handoff_import = args.get(2).map(String::as_str) == Some("--handoff-import");
    let process_context = crate::platform::prepare_server_process(handoff_import);
    init_logging();
    match process_context {
        Ok(true) => info!("server using persistent user service context"),
        Ok(false) => {}
        Err(err) => {
            warn!(%err, "could not select persistent user service context; retaining inherited context")
        }
    }
    crate::platform::raise_server_nofile_limit();

    if handoff_import {
        let socket_path = args
            .get(3)
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing handoff socket"))?;
        let token = args
            .get(4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing handoff token"))?;
        return run_handoff_import_server(&socket_path, token);
    }

    // Before any startup effect (a socket bound, the session restored, an
    // agent started): a second server for this session refuses here.
    let lock_path = crate::server::session_lock::active_lock_path();
    let session_lock = match crate::server::session_lock::SessionLock::acquire(&lock_path) {
        Ok(lock) => lock,
        Err(err) if err.kind() == io::ErrorKind::ResourceBusy => {
            warn!(err = %err, "refusing to start a second server for this session");
            start_reporter.refused(crate::server::session_lock::recorded_holder(&lock_path));
            eprintln!("error: {err}");
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let loaded_config = config::Config::load();
    #[cfg(windows)]
    if loaded_config.config.server.allow_unelevated_clients {
        crate::platform::allow_unelevated_clients();
    }
    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let server_stop = crate::server::shutdown::ServerStop::default();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;
    install_stop_signals(&rt, &server_stop);

    // Start the JSON API socket server.
    let _api_server = match api::start_server_with_stop_control(
        api_tx.clone(),
        event_hub.clone(),
        server_stop.clone(),
    ) {
        Ok(server) => server,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("error: herdr server is already running");
            eprintln!("api socket: {}", api::socket_path().display());
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let result = rt.block_on(async {
        // Create the App (with AppState, event channels, etc.).
        let mut app = app::App::try_new(
            &loaded_config.config,
            app::AppPolicy::PRODUCTION,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            api_rx,
            event_hub,
        )?;
        seed_startup_workspace_if_empty(&mut app);

        // Create the headless server.
        let mut server = match HeadlessServer::new(
            app,
            &loaded_config.diagnostics,
            Some(api_tx.clone()),
            Some(_api_server),
            server_stop,
        ) {
            Ok(server) => server,
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                eprintln!("error: herdr server is already running");
                eprintln!("client socket: {}", client_socket_path().display());
                std::process::exit(1);
            }
            Err(err) => return Err(err),
        };
        server.hold_session_lock(session_lock);
        // Both sockets listen: the client that spawned this server attaches.
        start_reporter.ready();

        info!(
            api_socket = %api::socket_path().display(),
            client_socket = %client_socket_path().display(),
            "herdr server started"
        );
        print_ready_message(&api::socket_path(), &client_socket_path());
        server.app.run_plugin_startup_hooks();

        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

/// Routes SIGINT and SIGTERM to `server_stop` (and logs SIGHUP) before the
/// API socket listens. A client that finds the socket may signal the server
/// at once, while the app is still starting; without a handler yet, the
/// signal's default action would end the process without a shutdown or its
/// log line. The event loop acts on the request once it runs.
fn install_stop_signals(rt: &tokio::runtime::Runtime, server_stop: &ServerStop) {
    let _entered = rt.enter();
    let server_stop = server_stop.clone();
    crate::platform::spawn_server_signal_monitor(move |signal| {
        server_stop.request(ShutdownReason::Signal(signal));
    });
}

fn seed_startup_workspace_if_empty(app: &mut app::App) {
    let Some(cwd) = take_startup_cwd() else {
        return;
    };

    if !app.state.workspaces.is_empty() {
        info!(
            cwd = %cwd.display(),
            "restored session already has workspaces; ignoring startup cwd"
        );
        return;
    }

    match app.create_workspace_with_options(cwd.clone(), true) {
        Ok(_) => {
            info!(cwd = %cwd.display(), "created startup workspace");
        }
        Err(err) => {
            warn!(cwd = %cwd.display(), err = %err, "failed to create startup workspace");
            app.state.mode = app::Mode::Navigate;
        }
    }
}

fn take_startup_cwd() -> Option<PathBuf> {
    let cwd = std::env::var_os(crate::server::autodetect::STARTUP_CWD_ENV_VAR)?;
    std::env::remove_var(crate::server::autodetect::STARTUP_CWD_ENV_VAR);
    (!cwd.is_empty()).then(|| PathBuf::from(cwd))
}

#[cfg(unix)]
fn run_handoff_import_server(socket_path: &Path, token: &str) -> io::Result<()> {
    let loaded_config = config::Config::load();
    let mut received = crate::server::handoff::receive(socket_path, token)?;
    // Before anything is restored. Held from here on, also if the import
    // fails: dropping this copy leaves the source's.
    let session_lock = crate::server::handoff::import_session_lock(
        received.server_lock.take(),
        received.manifest.server_lock.as_ref(),
        &crate::server::session_lock::active_lock_path(),
    )?;
    crate::server::handoff::log_import_result(received.manifest.panes.len());
    // Before the server resumes its `todo run`s: their install and push
    // need the caller's environment, which only memory held.
    crate::workers::restore_handed_off_envs(std::mem::take(&mut received.manifest.run_envs));

    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let server_stop = crate::server::shutdown::ServerStop::default();

    let mut imports = HashMap::new();
    for (pane, fd) in received.manifest.panes.into_iter().zip(received.fds) {
        let pane_id = pane.pane_id;
        imports.insert(
            pane_id,
            crate::handoff_runtime::ImportedHandoffRuntime {
                master_fd: fd,
                state: pane,
            },
        );
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let result = rt.block_on(async {
        let app = app::App::new_from_handoff(
            &loaded_config.config,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            api_rx,
            event_hub.clone(),
            &received.manifest.snapshot,
            &mut imports,
        )?;
        // Both servers start on the source's listening sockets without
        // accepting: connections wait in the backlog until this server owns
        // the session (after the commit), and the source resumes accepting
        // on the same listeners if the handoff fails before it.
        let start_servers =
            |api: crate::server::handoff::PublicListener,
             client: crate::server::handoff::PublicListener| {
                let api_server = api::ServerHandle::start_on_listener(
                    api_tx.clone(),
                    event_hub.clone(),
                    server_stop.clone(),
                    api.listener,
                    api.path,
                    api.identity,
                    false,
                )?;
                HeadlessServer::new_with_client_socket(
                    app,
                    &loaded_config.diagnostics,
                    Some(api_tx.clone()),
                    Some(api_server),
                    server_stop.clone(),
                    client,
                )
            };
        let mut server = match (
            received.listeners.take(),
            &received.manifest.public_listeners,
        ) {
            (Some(listeners), Some(offer)) => {
                let (api, client) = crate::server::handoff::passed_listeners(listeners, offer)?;
                start_servers(api, client)?
            }
            (None, None) => crate::server::handoff::take_over_public_paths(
                &received.stream,
                &api::socket_path(),
                &client_socket_path(),
                start_servers,
            )?,
            _ => return Err(io::Error::other("handoff listeners do not match the offer")),
        };
        server.hold_session_lock(session_lock);
        crate::server::handoff::report_restored(&mut received.stream)?;
        if std::env::var("HERDR_TEST_HANDOFF_IMPORT_FAIL").as_deref() == Ok("after_restored") {
            return Err(io::Error::other(
                "test handoff import failure after restored",
            ));
        }

        // Not earlier: a signal during the import still ends this process,
        // and the old server keeps its panes. From now on a signal ends the
        // wait for the commit below instead.
        install_stop_signals(&rt, &server_stop);
        // Carried across before any client attaches, so the first title sent is
        // the override rather than the configured one it replaced.
        server.api_window_title = received.manifest.api_window_title.take();
        // Likewise the announced PTY thresholds, so this server does not
        // notify again about the level the old one already reported.
        server.pty_alerts =
            crate::pty::usage::PtyAlerts::from_handoff(received.manifest.pty_alerts.take());
        crate::server::handoff::report_ready(&mut received.stream)?;
        wait_committed_or_stopped(&mut received.stream, &server_stop)?;
        server.app.assume_handoff_ownership();
        if let Some(lock) = &server.server_lock {
            lock.write_record();
        }
        server.assume_client_socket_file();
        if let Some(api_server) = server.api_server.as_mut() {
            api_server.assume_socket_file();
            api_server.resume_accepting();
        }
        server.app.unpause_handoff_readers();
        server.pending_handoff_repaint_nudge = true;
        if let Err(err) = crate::server::handoff::report_owned(&mut received.stream) {
            warn!(err = %err, "failed to report handoff ownership; continuing as owner");
        }
        info!("handoff import server started");
        print_ready_message(&api::socket_path(), &client_socket_path());
        server.app.run_plugin_startup_hooks();
        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

/// Waits for the source's commit; a stop signal (routed to `server_stop`)
/// or the source closing the stream ends the wait with an error, so the
/// import ends and the source keeps its panes.
#[cfg(unix)]
fn wait_committed_or_stopped(
    stream: &mut std::os::unix::net::UnixStream,
    server_stop: &ServerStop,
) -> io::Result<()> {
    use std::os::fd::AsFd as _;

    let (wake_tx, wake_rx) = std::os::unix::net::UnixStream::pair()?;
    let stop = server_stop.clone();
    let waker = tokio::spawn(async move {
        stop.requested().await;
        let _ = io::Write::write_all(&mut &wake_tx, &[1]);
    });
    let committed = crate::server::handoff::wait_committed(stream, wake_rx.as_fd());
    waker.abort();
    if committed.is_ok() && server_stop.is_requested() {
        // The waker may have taken the wake-up the event loop needs to see
        // this stop; the first reason recorded stays.
        server_stop.request(ShutdownReason::Unknown);
    }
    committed
}

#[cfg(not(unix))]
fn run_handoff_import_server(_socket_path: &Path, _token: &str) -> io::Result<()> {
    Err(io::Error::other("live handoff is only supported on Unix"))
}

fn print_ready_message(api_socket: &Path, client_socket: &Path) {
    let message = format!(
        "herdr server running; you can use any herdr CLI command in another terminal.\n\
         api socket: {}\n\
         client socket: {}\n\
         logs: {}\n\
         did you mean to open the Herdr TUI? run `herdr`; you do not need `herdr server`.\n",
        api_socket.display(),
        client_socket.display(),
        crate::session::data_dir()
            .join("herdr-server.log")
            .display()
    );
    // The launching terminal may already be gone; the server keeps running.
    let _ = io::Write::write_all(&mut io::stderr(), message.as_bytes());
}

/// Initialize logging for the server process.
fn init_logging() {
    crate::logging::init_file_logging("herdr-server.log");
}
