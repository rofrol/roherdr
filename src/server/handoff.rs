#[cfg(unix)]
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use std::collections::{BTreeMap, HashMap};

#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use tracing::{info, warn};

#[cfg(unix)]
const HANDOFF_VERSION: u32 = 1;
#[cfg(unix)]
const READY_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(unix)]
const OWNED_ACK_TIMEOUT: Duration = Duration::from_millis(500);
// Descriptors are transferred in batches of this size. A single SCM_RIGHTS
// control message caps out at 253 descriptors on Linux and 254 on macOS, so the
// batch stays well below both limits and the number of panes stays unbounded.
#[cfg(unix)]
const FDS_PER_MESSAGE: usize = 64;
#[cfg(unix)]
pub(crate) const MAX_REPLAY_BYTES_PER_PANE: usize = 8 * 1024;
// A full-screen program's screen with styling is typically tens of KiB; the
// manifest is read a byte at a time, so larger screens use the redraw nudge.
#[cfg(unix)]
pub(crate) const MAX_ALTERNATE_SCREEN_BYTES_PER_PANE: usize = 64 * 1024;

#[cfg(unix)]
#[derive(Serialize, Deserialize)]
pub(crate) struct HandoffManifest {
    pub version: u32,
    pub source_version: String,
    pub source_protocol: u32,
    pub expected_version: Option<String>,
    pub expected_protocol: Option<u32>,
    pub snapshot: crate::persist::SessionSnapshot,
    pub panes: Vec<crate::handoff_runtime::HandoffRuntimeState>,
    /// An outer window title set over the API outlives the server that took the
    /// call, so a handoff carries it rather than falling back to the config.
    /// Absent from manifests written before this field existed.
    #[serde(default)]
    pub api_window_title: Option<String>,
    /// The `todo run`s' caller environments, by run id, which the server
    /// keeps only in memory (they may hold credentials): the new server
    /// takes them so a run's install and push go on after the handoff. This
    /// manifest crosses the handoff's socket only and is never written to
    /// disk. Absent from manifests written before this field existed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub run_envs: BTreeMap<String, HashMap<String, String>>,
    /// The pseudo-terminal thresholds already announced, so an install's
    /// handoff does not announce the current level again. Absent from
    /// manifests written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_alerts: Option<crate::pty::usage::PtyAlertsHandoff>,
    /// Present when the source passes its two listening public sockets over
    /// this stream to an import server that takes them (`validated
    /// take-public-listeners`), so neither path is unlinked, renamed or
    /// bound again. Absent from manifests written before this field existed:
    /// such a source unlinks both paths on `restored` and the import server
    /// takes them over by path (`take_over_public_paths`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_listeners: Option<PublicListenersOffer>,
}

/// The `validated` line of an import server that takes the offered
/// listening sockets. An older import server only sends `validated`.
#[cfg(unix)]
const VALIDATED_TAKE_LISTENERS: &str = "validated take-public-listeners";

/// The two listening sockets a source offers to pass, in the order they are
/// sent.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PublicListenersOffer {
    pub api: PassedSocketFile,
    pub client: PassedSocketFile,
}

/// Where a passed listener's socket file is, and which file it is.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PassedSocketFile {
    pub path: PathBuf,
    pub file: SocketFileId,
}

/// A socket file's identity on the wire.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SocketFileId {
    pub dev: u64,
    pub ino: u64,
}

#[cfg(unix)]
impl SocketFileId {
    pub(crate) fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
}

/// How the import server on the other end takes the public sockets.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicSocketsHandoff {
    /// It takes the offered listening sockets over this stream.
    PassListeners,
    /// An import server from before the listener transfer: the source
    /// unlinks both paths on `restored` and the import server binds them.
    UnlinkOnRestored,
}

/// A listening socket an import server serves on, with the path and file it
/// listens at.
#[cfg(unix)]
pub(crate) struct PublicListener {
    pub listener: crate::ipc::LocalListener,
    pub path: PathBuf,
    pub identity: crate::ipc::SocketFileIdentity,
}

#[cfg(unix)]
pub(crate) struct ReceivedHandoff {
    pub manifest: HandoffManifest,
    pub fds: Vec<RawFd>,
    /// The API and client listeners the source passed, when it offered them.
    pub listeners: Option<[std::os::fd::OwnedFd; 2]>,
    pub stream: UnixStream,
}

#[cfg(unix)]
pub(crate) fn handoff_socket_path() -> PathBuf {
    crate::session::data_dir().join(format!("herdr-handoff-{}.sock", std::process::id()))
}

#[cfg(unix)]
pub(crate) fn spawn_handoff_import(
    import_exe: Option<&Path>,
    socket_path: &Path,
    token: &str,
) -> io::Result<Child> {
    let fallback_exe;
    let exe = if let Some(import_exe) = import_exe {
        import_exe
    } else {
        fallback_exe = std::env::current_exe().map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("failed to determine herdr executable path: {err}"),
            )
        })?;
        &fallback_exe
    };
    let mut command = Command::new(exe);
    command
        .arg("server")
        .arg("--handoff-import")
        .arg(socket_path)
        .arg(token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if crate::session::explicit_session_requested() {
        // The import child no longer has the original `--session` argument, so
        // stale socket overrides must not mask the inherited HERDR_SESSION.
        command
            .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
            .env_remove(crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR);
    }
    crate::platform::detach_server_daemon_command(&mut command);
    command.spawn().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to spawn handoff import server at {}: {err}",
                exe.display()
            ),
        )
    })
}

#[cfg(unix)]
pub(crate) fn cleanup_failed_import_child(child: &mut Child) {
    let pid = child.id();
    match child.try_wait() {
        Ok(Some(status)) => {
            info!(pid, status = %status, "handoff import server exited during rollback");
            return;
        }
        Ok(None) => {}
        Err(err) => {
            warn!(pid, err = %err, "failed to inspect handoff import server before rollback");
        }
    }

    if let Err(err) = child.kill() {
        warn!(pid, err = %err, "failed to kill handoff import server during rollback");
    }
    match child.wait() {
        Ok(status) => {
            info!(pid, status = %status, "handoff import server reaped during rollback");
        }
        Err(err) => {
            warn!(pid, err = %err, "failed to reap handoff import server during rollback");
        }
    }
}

#[cfg(unix)]
pub(crate) fn bind_listener(socket_path: &Path) -> io::Result<UnixListener> {
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;
    restrict_socket_permissions(socket_path)?;
    Ok(listener)
}

#[cfg(unix)]
pub(crate) fn accept_and_validate_on(
    listener: UnixListener,
    socket_path: &Path,
    token: &str,
    manifest: &HandoffManifest,
) -> io::Result<(UnixStream, PublicSocketsHandoff)> {
    let (mut stream, _) = accept_with_timeout(&listener, READY_TIMEOUT)?;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    stream.set_write_timeout(Some(READY_TIMEOUT))?;
    let token_line = read_line_unbuffered(&mut stream)?;
    if token_line.trim_end() != token {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "handoff import token mismatch",
        ));
    }

    serde_json::to_writer(&mut stream, manifest).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let validated = read_line_unbuffered(&mut stream)?;
    let public_sockets = match validated.trim_end() {
        "validated" => PublicSocketsHandoff::UnlinkOnRestored,
        VALIDATED_TAKE_LISTENERS if manifest.public_listeners.is_some() => {
            PublicSocketsHandoff::PassListeners
        }
        _ => return Err(io::Error::other("handoff import did not validate manifest")),
    };
    let _ = std::fs::remove_file(socket_path);
    Ok((stream, public_sockets))
}

#[cfg(unix)]
pub(crate) fn send_fds_and_wait_restored(stream: &mut UnixStream, fds: &[RawFd]) -> io::Result<()> {
    send_fds(stream, fds)?;
    wait_restored(stream)
}

/// A source's listening public sockets, as the listener handoff uses them.
#[cfg(unix)]
pub(crate) trait SourceListeners {
    /// Returns once this process accepts on neither listener.
    fn pause_accepting(&mut self);
    /// Accepts on both again, after a failed handoff.
    fn resume_accepting(&mut self);
    /// The API and client listeners.
    fn listener_fds(&self) -> io::Result<[std::os::fd::BorrowedFd<'_>; 2]>;
    /// Closes this process's copies after the commit.
    fn release(&mut self);
}

/// The points of a listener handoff at which a caller may observe it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListenerHandoffPoint {
    /// The source accepts no more; nothing is sent yet.
    Paused,
    /// Both listeners are sent; the import server has not reported restored.
    Passed,
    /// The import server reported ready; the source has not committed.
    Ready,
}

/// The source's side of a handoff to an import server that takes the
/// listening sockets: stops accepting, passes the pane descriptors and both
/// listeners, and commits once the import server is ready. Nothing on the
/// file system changes, so the public paths always lead to a listening
/// socket and connections meanwhile wait in its backlog. On a failure it
/// runs `reap_import` (so the import server's copies are closed) and then
/// accepts on the same listeners again; after the commit it releases them.
#[cfg(unix)]
pub(crate) fn pass_public_listeners(
    stream: &mut UnixStream,
    pane_fds: &[RawFd],
    listeners: &mut impl SourceListeners,
    reap_import: impl FnOnce(),
    at: &mut dyn FnMut(ListenerHandoffPoint),
) -> io::Result<()> {
    listeners.pause_accepting();
    at(ListenerHandoffPoint::Paused);
    let handed_off = (|| {
        send_fds(stream, pane_fds)?;
        let [api, client] = listeners.listener_fds()?;
        send_listener_fds(stream, api, client)?;
        at(ListenerHandoffPoint::Passed);
        wait_restored(stream)?;
        wait_ready(stream).map_err(|err| {
            io::Error::other(format!(
                "handoff replacement server did not become ready: {err}"
            ))
        })?;
        at(ListenerHandoffPoint::Ready);
        report_committed(stream).map_err(|err| {
            io::Error::other(format!(
                "handoff replacement server was ready, but commit failed: {err}"
            ))
        })
    })();
    match handed_off {
        Ok(()) => {
            listeners.release();
            Ok(())
        }
        Err(err) => {
            reap_import();
            listeners.resume_accepting();
            Err(err)
        }
    }
}

/// Passes the source's listening API and client sockets, in that order. The
/// source must not accept on them from now on unless the handoff fails.
#[cfg(unix)]
pub(crate) fn send_listener_fds(
    stream: &UnixStream,
    api: std::os::fd::BorrowedFd<'_>,
    client: std::os::fd::BorrowedFd<'_>,
) -> io::Result<()> {
    send_fd_batch(stream, &[api.as_raw_fd(), client.as_raw_fd()])
}

#[cfg(unix)]
pub(crate) fn wait_restored(stream: &mut UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let restored = read_line_unbuffered(&mut *stream)?;
    if restored.trim_end() != "restored" {
        return Err(io::Error::other(
            "handoff import did not report restored runtimes",
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn wait_ready(stream: &mut UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let ready = read_line_unbuffered(&mut *stream)?;
    if ready.trim_end() != "ready" {
        return Err(io::Error::other("handoff import did not report ready"));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn report_committed(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"committed\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn wait_owned_ack(stream: &mut UnixStream) {
    if let Err(err) = stream.set_read_timeout(Some(OWNED_ACK_TIMEOUT)) {
        warn!(err = %err, "failed to set handoff ownership ack timeout");
        return;
    }
    match read_line_unbuffered(&mut *stream) {
        Ok(owned) if owned.trim_end() == "owned" => {}
        Ok(other) => {
            warn!(
                response = %other.trim_end(),
                "handoff import sent unexpected ownership ack after commit"
            );
        }
        Err(err) => {
            warn!(err = %err, "handoff import ownership ack was not received after commit");
        }
    }
}

#[cfg(unix)]
pub(crate) fn receive(socket_path: &Path, token: &str) -> io::Result<ReceivedHandoff> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.write_all(token.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let manifest_line = read_line_unbuffered(&mut stream)?;
    let manifest: HandoffManifest =
        serde_json::from_str(&manifest_line).map_err(io::Error::other)?;
    if manifest.version != HANDOFF_VERSION {
        return Err(io::Error::other(format!(
            "unsupported handoff version {}",
            manifest.version
        )));
    }
    if manifest
        .expected_protocol
        .is_some_and(|protocol| protocol != crate::protocol::PROTOCOL_VERSION)
    {
        return Err(io::Error::other(format!(
            "handoff expected protocol {}, but this server speaks protocol {}",
            manifest.expected_protocol.unwrap_or_default(),
            crate::protocol::PROTOCOL_VERSION
        )));
    }
    if manifest
        .expected_version
        .as_deref()
        .is_some_and(|version| version != crate::build_info::version())
    {
        return Err(io::Error::other(format!(
            "handoff expected herdr v{}, but this server is v{}",
            manifest.expected_version.as_deref().unwrap_or("unknown"),
            crate::build_info::version()
        )));
    }
    let take_listeners = manifest.public_listeners.is_some();
    if take_listeners {
        stream.write_all(VALIDATED_TAKE_LISTENERS.as_bytes())?;
        stream.write_all(b"\n")?;
    } else {
        stream.write_all(b"validated\n")?;
    }
    stream.flush()?;
    let fds = recv_fds(&stream, manifest.panes.len())?;
    let listeners = if take_listeners {
        match recv_fds(&stream, 2) {
            Ok(listeners) => {
                use std::os::fd::FromRawFd as _;

                // `recv_fds` returns exactly the two descriptors asked for,
                // each one new in this process.
                let [api, client]: [RawFd; 2] = listeners
                    .try_into()
                    .map_err(|_| io::Error::other("handoff passed no two listeners"))?;
                Some(unsafe {
                    [
                        std::os::fd::OwnedFd::from_raw_fd(api),
                        std::os::fd::OwnedFd::from_raw_fd(client),
                    ]
                })
            }
            Err(err) => {
                close_raw_fds(&fds);
                return Err(err);
            }
        }
    } else {
        None
    };
    Ok(ReceivedHandoff {
        manifest,
        fds,
        listeners,
        stream,
    })
}

/// The listeners a source passed, checked against its offer: each socket
/// file must still be the one the source listened at, so this server later
/// removes only its own file.
#[cfg(unix)]
pub(crate) fn passed_listeners(
    fds: [std::os::fd::OwnedFd; 2],
    offer: &PublicListenersOffer,
) -> io::Result<(PublicListener, PublicListener)> {
    let [api_fd, client_fd] = fds;
    let api = passed_listener(api_fd, &offer.api)?;
    let client = passed_listener(client_fd, &offer.client)?;
    Ok((api, client))
}

#[cfg(unix)]
fn passed_listener(
    fd: std::os::fd::OwnedFd,
    passed: &PassedSocketFile,
) -> io::Result<PublicListener> {
    let listener = crate::ipc::LocalListener::from(
        interprocess::os::unix::uds_local_socket::Listener::from(fd),
    );
    // Read between two checks of the file, so it names the offered file.
    let before = SocketFileId::of(&passed.path)?;
    let identity = crate::ipc::socket_file_identity(&passed.path)?;
    if before != passed.file || SocketFileId::of(&passed.path)? != passed.file {
        return Err(io::Error::other(format!(
            "{} is no longer the socket file the handoff source listened at; refusing the \
             handoff",
            passed.path.display()
        )));
    }
    Ok(PublicListener {
        listener,
        path: passed.path.clone(),
        identity,
    })
}

/// Takes the public paths from a source that offers no listeners (a build
/// from before the transfer, which unlinks both paths, each only if still its
/// own file, once it reads `restored`). Binds a listener beside each path,
/// lets `build` make the servers on them, then renames each over its path,
/// so the paths never lead nowhere and the source's later unlinks find
/// another file. Only a path the source itself accepts on (or nobody does)
/// is replaced; another process that accepts there refuses the handoff by
/// name. Runs before `restored` is sent.
#[cfg(unix)]
pub(crate) fn take_over_public_paths<T>(
    stream: &UnixStream,
    api_path: &Path,
    client_path: &Path,
    build: impl FnOnce(PublicListener, PublicListener) -> io::Result<T>,
) -> io::Result<T> {
    let source = source_pid(stream);
    let api = stage_listener(api_path, "a")?;
    let client = match stage_listener(client_path, "c") {
        Ok(client) => client,
        Err(err) => {
            api.discard();
            return Err(err);
        }
    };
    let api_staging = api.staging_file();
    let client_staging = client.staging_file();
    let discard = || {
        let _ = crate::ipc::remove_socket_file_if_owned(&api_staging.0, &api_staging.1);
        let _ = crate::ipc::remove_socket_file_if_owned(&client_staging.0, &client_staging.1);
    };
    let built = build(api.public, client.public).inspect_err(|_| discard())?;
    install_staged(&api_staging.0, api_path, source)
        .and_then(|()| install_staged(&client_staging.0, client_path, source))
        .inspect_err(|_| discard())?;
    Ok(built)
}

/// A listener bound at a staging path beside its public path; its
/// `PublicListener` already names the public path, which the staging file
/// is renamed to.
#[cfg(unix)]
struct StagedListener {
    public: PublicListener,
    staging: PathBuf,
}

#[cfg(unix)]
impl StagedListener {
    fn staging_file(&self) -> (PathBuf, crate::ipc::SocketFileIdentity) {
        (self.staging.clone(), self.public.identity.clone())
    }

    fn discard(self) {
        let _ = crate::ipc::remove_socket_file_if_owned(&self.staging, &self.public.identity);
    }
}

#[cfg(unix)]
fn stage_listener(path: &Path, kind: &str) -> io::Result<StagedListener> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Short and in the same directory: a rename is atomic only within one
    // file system, and a socket path is limited to about 100 bytes.
    let staging = parent.join(format!(".h{kind}-{}.sock", std::process::id()));
    // Removes only a file nobody accepts on.
    crate::server::socket_paths::prepare_socket_path(&staging)?;
    let listener = crate::ipc::bind_local_listener(&staging)?;
    let identity = crate::ipc::socket_file_identity(&staging)?;
    let staged = StagedListener {
        public: PublicListener {
            listener,
            path: path.to_path_buf(),
            identity,
        },
        staging,
    };
    if let Err(err) = crate::server::socket_paths::restrict_socket_permissions(&staged.staging) {
        staged.discard();
        return Err(err);
    }
    Ok(staged)
}

#[cfg(unix)]
fn install_staged(staging: &Path, path: &Path, source: Option<u32>) -> io::Result<()> {
    check_held_by_source(path, source)?;
    std::fs::rename(staging, path)
}

/// Succeeds when the file at `path` is absent, stale, or the one `source`
/// accepts on, and the same file throughout the check.
#[cfg(unix)]
fn check_held_by_source(path: &Path, source: Option<u32>) -> io::Result<()> {
    let file = |path: &Path| match SocketFileId::of(path) {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    };
    let before = file(path)?;
    if before.is_some() {
        match crate::ipc::connect_local_stream(path) {
            Ok(stream) => {
                let holder = crate::platform::local_stream_peer_description(&stream);
                let holder_pid = holder.as_deref().and_then(described_pid);
                if source.is_none() || holder_pid != source {
                    return Err(held_socket_error(path, holder));
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) => {}
            Err(err) => return Err(err),
        }
    }
    if file(path)? != before {
        return Err(io::Error::other(format!(
            "{} changed while the handoff checked it; refusing the handoff",
            path.display()
        )));
    }
    Ok(())
}

/// The process id a peer description (`pid 42 (herdr), ...`) starts with.
#[cfg(unix)]
fn described_pid(description: &str) -> Option<u32> {
    let digits = description.strip_prefix("pid ")?;
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse().ok()
}

/// The handoff source's process id, from the stream's peer.
#[cfg(unix)]
fn source_pid(stream: &UnixStream) -> Option<u32> {
    let stream = crate::ipc::LocalStream::from(
        interprocess::os::unix::uds_local_socket::Stream::from(stream.try_clone().ok()?),
    );
    crate::platform::local_stream_peer_description(&stream)
        .as_deref()
        .and_then(described_pid)
}

#[cfg(unix)]
fn held_socket_error(path: &Path, holder: Option<String>) -> io::Error {
    let holder = holder.unwrap_or_else(|| "an unknown process".to_owned());
    io::Error::new(
        io::ErrorKind::AddrInUse,
        format!(
            "{holder} accepts on {}, which is not this handoff's source; refusing the handoff",
            path.display()
        ),
    )
}

#[cfg(unix)]
pub(crate) fn report_restored(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"restored\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn report_ready(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"ready\n")?;
    stream.flush()
}

/// Waits for the source's `committed` line, or until `wake` turns readable
/// (the import server's stop) or the source closes the stream; no timeout.
#[cfg(unix)]
pub(crate) fn wait_committed(
    stream: &mut UnixStream,
    wake: std::os::fd::BorrowedFd<'_>,
) -> io::Result<()> {
    // Nonblocking reads after `poll`: the stream's read timeout plays no
    // part.
    stream.set_nonblocking(true)?;
    let committed = read_line_until_woken(stream, wake);
    stream.set_nonblocking(false)?;
    if committed?.trim_end() != "committed" {
        return Err(io::Error::other("handoff source did not commit"));
    }
    Ok(())
}

/// Reads one line from a nonblocking stream, waiting in `poll` for the
/// stream or `wake`, and ending with `Interrupted` once `wake` is readable.
/// Reads a byte at a time, so nothing after the line is consumed.
#[cfg(unix)]
fn read_line_until_woken(
    stream: &mut UnixStream,
    wake: std::os::fd::BorrowedFd<'_>,
) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut fds = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        // The stream first: a commit that already arrived wins over a stop
        // that came with it, since the source no longer keeps the panes.
        loop {
            let mut byte = [0u8; 1];
            match stream.read(&mut byte) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "handoff stream closed while reading line",
                    ))
                }
                Ok(_) => {
                    bytes.push(byte[0]);
                    if byte[0] == b'\n' {
                        return String::from_utf8(bytes)
                            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
                    }
                    if bytes.len() > 1024 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "handoff line exceeded maximum size",
                        ));
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        if fds[1].revents != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "the import server was stopped while waiting for the handoff commit",
            ));
        }
    }
}

#[cfg(unix)]
pub(crate) fn report_owned(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"owned\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn manifest_for(
    snapshot: crate::persist::SessionSnapshot,
    panes: Vec<crate::handoff_runtime::HandoffRuntimeState>,
    expected_protocol: Option<u32>,
    expected_version: Option<String>,
    api_window_title: Option<String>,
    run_envs: BTreeMap<String, HashMap<String, String>>,
) -> HandoffManifest {
    HandoffManifest {
        version: HANDOFF_VERSION,
        source_version: crate::build_info::version(),
        source_protocol: crate::protocol::PROTOCOL_VERSION,
        expected_version,
        expected_protocol,
        snapshot,
        panes,
        api_window_title,
        run_envs,
        pty_alerts: None,
        public_listeners: None,
    }
}

#[cfg(unix)]
fn restrict_socket_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn accept_with_timeout(
    listener: &UnixListener,
    timeout: Duration,
) -> io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok(accepted) => return Ok(accepted),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for handoff import connection",
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn read_line_unbuffered(stream: &mut UnixStream) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "handoff stream closed while reading line",
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
        }
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff line exceeded maximum size",
            ));
        }
    }
}

#[cfg(unix)]
fn send_fds(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    for batch in fds.chunks(FDS_PER_MESSAGE) {
        send_fd_batch(stream, batch)?;
    }
    Ok(())
}

#[cfg(unix)]
fn send_fd_batch(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    if fds.is_empty() {
        return Ok(());
    }
    let byte = [b'F'];
    let iov = [libc::iovec {
        iov_base: byte.as_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    let fd_bytes = std::mem::size_of_val(fds);
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(fd_bytes as u32) as usize }];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len() as _;

    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(io::Error::other("failed to allocate fd control message"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(fd_bytes as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr() as *const u8, libc::CMSG_DATA(cmsg), fd_bytes);
        if libc::sendmsg(stream.as_raw_fd(), &msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn close_raw_fds(fds: &[RawFd]) {
    for fd in fds {
        let _ = unsafe { libc::close(*fd) };
    }
}

#[cfg(unix)]
fn recv_fds(stream: &UnixStream, expected: usize) -> io::Result<Vec<RawFd>> {
    let mut out: Vec<RawFd> = Vec::with_capacity(expected);
    while out.len() < expected {
        let wanted = (expected - out.len()).min(FDS_PER_MESSAGE);
        let batch = match recv_fd_batch(stream, wanted) {
            Ok(batch) => batch,
            Err(err) => {
                close_raw_fds(&out);
                return Err(err);
            }
        };
        if batch.is_empty() {
            let received = out.len();
            close_raw_fds(&out);
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "handoff stream closed after {received} of {expected} pane file descriptors"
                ),
            ));
        }
        out.extend(batch);
    }
    Ok(out)
}

#[cfg(unix)]
fn recv_fd_batch(stream: &UnixStream, wanted: usize) -> io::Result<Vec<RawFd>> {
    let mut byte = [0u8; 1];
    let mut iov = [libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    let fd_bytes = wanted * std::mem::size_of::<RawFd>();
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(fd_bytes as u32) as usize }];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len() as _;

    let read = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut out = Vec::new();
    unsafe {
        let control_end = control.as_ptr() as usize + msg.msg_controllen as usize;
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg);
                // Bound the payload by both the header's own length and the
                // bytes the kernel wrote into `control`, so the read below can
                // never run past the buffer.
                let available = control_end.saturating_sub(data as usize);
                let data_len = ((*cmsg).cmsg_len as usize)
                    .saturating_sub(libc::CMSG_LEN(0) as usize)
                    .min(available);
                let count = data_len / std::mem::size_of::<RawFd>();
                let data = data as *const RawFd;
                for idx in 0..count {
                    out.push(*data.add(idx));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }

    // Truncation means the kernel closed the descriptors that did not fit, so
    // the batch is unrecoverable rather than merely short.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        close_raw_fds(&out);
        return Err(io::Error::other("handoff fd control message was truncated"));
    }
    if read == 0 {
        close_raw_fds(&out);
        return Ok(Vec::new());
    }
    if out.len() > wanted {
        let received = out.len();
        close_raw_fds(&out);
        return Err(io::Error::other(format!(
            "handoff fd message carried {received} descriptors, expected at most {wanted}"
        )));
    }
    if out.is_empty() {
        return Err(io::Error::other("handoff fd message missing SCM_RIGHTS"));
    }
    Ok(out)
}

#[cfg(unix)]
pub(crate) fn log_import_result(panes: usize) {
    info!(panes, "handoff import ready");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn empty_snapshot() -> crate::persist::SessionSnapshot {
        crate::persist::SessionSnapshot {
            version: 0,
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        }
    }

    #[test]
    fn a_handoff_carries_an_api_set_window_title() {
        let manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            None,
            None,
            Some("deploying".to_string()),
            BTreeMap::new(),
        );

        assert_eq!(manifest.api_window_title.as_deref(), Some("deploying"));
    }

    #[test]
    fn a_manifest_written_before_the_title_field_still_loads() {
        let manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            None,
            None,
            Some("deploying".to_string()),
            BTreeMap::new(),
        );
        let mut value = serde_json::to_value(&manifest).expect("manifest should serialize");
        value
            .as_object_mut()
            .expect("manifest should be a json object")
            .remove("api_window_title");

        let older: HandoffManifest =
            serde_json::from_value(value).expect("an older manifest should still load");

        assert!(older.api_window_title.is_none());
        assert!(older.run_envs.is_empty());
        assert!(older.pty_alerts.is_none());
        assert!(older.public_listeners.is_none());
    }

    #[test]
    fn a_handoff_carries_the_announced_pty_thresholds() {
        let mut manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            None,
            None,
            None,
            BTreeMap::new(),
        );
        let announced = crate::pty::usage::PtyAlertsHandoff {
            announced: Some(80),
        };
        manifest.pty_alerts = Some(announced);
        let value = serde_json::to_value(&manifest).expect("manifest should serialize");

        let received: HandoffManifest =
            serde_json::from_value(value).expect("the manifest should load");

        assert_eq!(received.pty_alerts, Some(announced));
    }

    #[test]
    fn a_handoff_carries_the_runs_environments_over_its_socket_only() {
        let secret = format!("run-env-secret-{}", std::process::id());
        let env = HashMap::from([("PUSH_TOKEN".to_string(), secret.clone())]);
        let run_envs = BTreeMap::from([("r-abcdefgh".to_string(), env)]);
        let manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            None,
            None,
            None,
            run_envs.clone(),
        );
        let dir = std::env::temp_dir().join(format!("herdr-handoff-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        let socket_path = dir.join("h.sock");
        let listener = bind_listener(&socket_path).expect("bind the handoff socket");
        let receiver = {
            let socket_path = socket_path.clone();
            std::thread::spawn(move || receive(&socket_path, "token"))
        };
        let _stream = accept_and_validate_on(listener, &socket_path, "token", &manifest)
            .expect("the manifest is sent");
        let received = receiver
            .join()
            .expect("the receiver thread")
            .expect("the manifest is received");

        assert_eq!(received.manifest.run_envs, run_envs);
        // Nothing of the environment is left in the handoff's directory.
        for entry in std::fs::read_dir(&dir).expect("read the test dir") {
            let path = entry.expect("dir entry").path();
            let bytes = std::fs::read(&path).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(&secret),
                "{} holds the environment",
                path.display()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn socket_test_dir(name: &str) -> PathBuf {
        // Short: a socket path is limited to about 100 bytes.
        let dir = std::env::temp_dir().join(format!("hh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    fn listen(path: &Path) -> crate::ipc::LocalListener {
        use interprocess::local_socket::traits::Listener as _;
        use interprocess::local_socket::ListenerNonblockingMode;

        let listener = crate::ipc::bind_local_listener(path).expect("bind a listener");
        listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .expect("nonblocking accept");
        listener
    }

    /// How many connections wait on `listener`, accepting them.
    fn take_connections(listener: &crate::ipc::LocalListener) -> usize {
        use interprocess::local_socket::traits::Listener as _;
        use interprocess::local_socket::ListenerNonblockingMode;

        listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .expect("nonblocking accept");
        let mut taken = 0;
        loop {
            match listener.accept() {
                Ok(_) => taken += 1,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return taken,
                Err(err) => panic!("accept failed: {err}"),
            }
        }
    }

    fn offer_for(api_path: &Path, client_path: &Path) -> PublicListenersOffer {
        PublicListenersOffer {
            api: PassedSocketFile {
                path: api_path.to_path_buf(),
                file: SocketFileId::of(api_path).expect("api file"),
            },
            client: PassedSocketFile {
                path: client_path.to_path_buf(),
                file: SocketFileId::of(client_path).expect("client file"),
            },
        }
    }

    fn manifest_offering(offer: Option<PublicListenersOffer>) -> HandoffManifest {
        let mut manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            None,
            None,
            None,
            BTreeMap::new(),
        );
        manifest.public_listeners = offer;
        manifest
    }

    /// A source's listeners as `pass_public_listeners` drives them, recording
    /// each step in `log`.
    struct TestSource {
        api: Option<crate::ipc::LocalListener>,
        client: Option<crate::ipc::LocalListener>,
        log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    }

    impl SourceListeners for TestSource {
        fn pause_accepting(&mut self) {
            self.log.borrow_mut().push("pause");
        }

        fn resume_accepting(&mut self) {
            self.log.borrow_mut().push("resume");
        }

        fn listener_fds(&self) -> io::Result<[std::os::fd::BorrowedFd<'_>; 2]> {
            use std::os::fd::AsFd as _;

            let (
                Some(crate::ipc::LocalListener::UdSocket(api)),
                Some(crate::ipc::LocalListener::UdSocket(client)),
            ) = (&self.api, &self.client)
            else {
                return Err(io::Error::other("released"));
            };
            Ok([api.as_fd(), client.as_fd()])
        }

        fn release(&mut self) {
            self.log.borrow_mut().push("release");
            self.api = None;
            self.client = None;
        }
    }

    // The import server's side of a listener handoff, holding back
    // `restored` until `go` (the latch at the transfer point).
    fn import_listeners(
        handoff_path: &Path,
        passed: std::sync::mpsc::Sender<()>,
        go: std::sync::mpsc::Receiver<()>,
    ) -> io::Result<(PublicListener, PublicListener)> {
        use std::os::fd::AsFd as _;

        let mut received = receive(handoff_path, "token")?;
        let listeners = received
            .listeners
            .take()
            .ok_or_else(|| io::Error::other("no listeners passed"))?;
        let offer = received
            .manifest
            .public_listeners
            .clone()
            .ok_or_else(|| io::Error::other("no offer"))?;
        let taken = passed_listeners(listeners, &offer)?;
        let _ = passed.send(());
        go.recv().map_err(io::Error::other)?;
        report_restored(&mut received.stream)?;
        report_ready(&mut received.stream)?;
        let (_wake_tx, wake_rx) = UnixStream::pair()?;
        wait_committed(&mut received.stream, wake_rx.as_fd())?;
        report_owned(&mut received.stream)?;
        Ok(taken)
    }

    // Report r-86: the old server unlinked both public sockets on
    // `restored`, and a `herdr` started before the import server bound found
    // no client socket and started a second server. Now a connection at any
    // point of the handoff reaches a listening socket (none is ENOENT, so
    // nothing starts a second server), waits there and is accepted by the
    // import server; no file changes.
    #[test]
    fn a_client_reaches_a_listener_on_both_paths_throughout_a_listener_handoff() {
        let dir = socket_test_dir("pass");
        let api_path = dir.join("a.sock");
        let client_path = dir.join("c.sock");
        let handoff_path = dir.join("h.sock");
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut source = TestSource {
            api: Some(listen(&api_path)),
            client: Some(listen(&client_path)),
            log: log.clone(),
        };
        let offer = offer_for(&api_path, &client_path);
        let manifest = manifest_offering(Some(offer.clone()));
        let listener = bind_listener(&handoff_path).expect("bind the handoff socket");
        let (passed_tx, passed_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        let importer = {
            let handoff_path = handoff_path.clone();
            std::thread::spawn(move || import_listeners(&handoff_path, passed_tx, go_rx))
        };

        let (mut stream, public_sockets) =
            accept_and_validate_on(listener, &handoff_path, "token", &manifest)
                .expect("the import server validates");
        assert_eq!(public_sockets, PublicSocketsHandoff::PassListeners);
        let mut probes = Vec::new();
        let mut probe = |point: ListenerHandoffPoint| {
            if point == ListenerHandoffPoint::Passed {
                // The import server holds both listeners and waits.
                passed_rx
                    .recv()
                    .expect("the import server took the listeners");
            }
            for path in [&api_path, &client_path] {
                match crate::ipc::connect_local_stream(path) {
                    Ok(stream) => probes.push(stream),
                    Err(err) => panic!("{point:?}: {} led nowhere: {err}", path.display()),
                }
            }
            if point == ListenerHandoffPoint::Passed {
                go_tx.send(()).expect("release the import server");
            }
        };
        pass_public_listeners(
            &mut stream,
            &[],
            &mut source,
            || panic!("a successful handoff reaps nothing"),
            &mut probe,
        )
        .expect("the handoff commits");
        wait_owned_ack(&mut stream);
        let (new_api, new_client) = importer
            .join()
            .expect("the importer thread")
            .expect("the import server takes both listeners");

        assert_eq!(*log.borrow(), vec!["pause", "release"]);
        assert_eq!(probes.len(), 6);
        // No path was unlinked, renamed or bound again.
        assert_eq!(SocketFileId::of(&api_path).ok(), Some(offer.api.file));
        assert_eq!(SocketFileId::of(&client_path).ok(), Some(offer.client.file));
        assert_eq!(new_api.path, api_path);
        assert_eq!(new_client.path, client_path);
        assert_eq!(
            new_api.identity,
            crate::ipc::socket_file_identity(&api_path).expect("api identity")
        );
        // Every probe waited for the import server, which accepts it.
        assert_eq!(take_connections(&new_api.listener), 3);
        assert_eq!(take_connections(&new_client.listener), 3);
        let _client = crate::ipc::connect_local_stream(&client_path).expect("connect");
        assert_eq!(take_connections(&new_client.listener), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_import_after_the_transfer_leaves_the_source_on_its_own_listeners() {
        let dir = socket_test_dir("fail");
        let api_path = dir.join("a.sock");
        let client_path = dir.join("c.sock");
        let handoff_path = dir.join("h.sock");
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut source = TestSource {
            api: Some(listen(&api_path)),
            client: Some(listen(&client_path)),
            log: log.clone(),
        };
        let offer = offer_for(&api_path, &client_path);
        let manifest = manifest_offering(Some(offer.clone()));
        let listener = bind_listener(&handoff_path).expect("bind the handoff socket");
        let importer = {
            let handoff_path = handoff_path.clone();
            std::thread::spawn(move || {
                // Takes the listeners, then fails before `restored`.
                let mut received = receive(&handoff_path, "token").expect("receive");
                let offer = received.manifest.public_listeners.clone().expect("offer");
                let listeners = received.listeners.take().expect("listeners");
                drop(passed_listeners(listeners, &offer).expect("passed listeners"));
            })
        };

        let (mut stream, _) = accept_and_validate_on(listener, &handoff_path, "token", &manifest)
            .expect("the import server validates");
        let reap_log = log.clone();
        let err = pass_public_listeners(
            &mut stream,
            &[],
            &mut source,
            || {
                importer.join().expect("the importer thread");
                reap_log.borrow_mut().push("reap");
            },
            &mut |_| {},
        )
        .expect_err("the import failed");

        assert!(!err.to_string().is_empty());
        assert_eq!(*log.borrow(), vec!["pause", "reap", "resume"]);
        assert_eq!(SocketFileId::of(&api_path).ok(), Some(offer.api.file));
        assert_eq!(SocketFileId::of(&client_path).ok(), Some(offer.client.file));
        let _api = crate::ipc::connect_local_stream(&api_path).expect("connect");
        let _client = crate::ipc::connect_local_stream(&client_path).expect("connect");
        assert_eq!(take_connections(source.api.as_ref().expect("api")), 1);
        assert_eq!(take_connections(source.client.as_ref().expect("client")), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_hands_its_paths_by_name_to_an_import_server_from_before_the_transfer() {
        let dir = socket_test_dir("oldimp");
        let api_path = dir.join("a.sock");
        let client_path = dir.join("c.sock");
        let handoff_path = dir.join("h.sock");
        let _api = listen(&api_path);
        let _client = listen(&client_path);
        let manifest = manifest_offering(Some(offer_for(&api_path, &client_path)));
        let listener = bind_listener(&handoff_path).expect("bind the handoff socket");
        let importer = {
            let handoff_path = handoff_path.clone();
            // That build's `receive`: it ignores the offer it does not know.
            std::thread::spawn(move || {
                let mut stream = UnixStream::connect(&handoff_path).expect("connect");
                stream.write_all(b"token\n").expect("token");
                let manifest = read_line_unbuffered(&mut stream).expect("manifest");
                assert!(manifest.contains("public_listeners"));
                stream.write_all(b"validated\n").expect("validated");
                stream
            })
        };

        let (_stream, public_sockets) =
            accept_and_validate_on(listener, &handoff_path, "token", &manifest)
                .expect("the older import server validates");
        let _import = importer.join().expect("the importer thread");

        assert_eq!(public_sockets, PublicSocketsHandoff::UnlinkOnRestored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_import_server_takes_no_listeners_from_a_source_from_before_the_transfer() {
        let dir = socket_test_dir("oldsrc");
        let handoff_path = dir.join("h.sock");
        let manifest = manifest_offering(None);
        let listener = bind_listener(&handoff_path).expect("bind the handoff socket");
        let importer = {
            let handoff_path = handoff_path.clone();
            std::thread::spawn(move || receive(&handoff_path, "token"))
        };

        // That build accepts only a plain `validated`.
        let (_stream, public_sockets) =
            accept_and_validate_on(listener, &handoff_path, "token", &manifest)
                .expect("the import server validates");
        let received = importer
            .join()
            .expect("the importer thread")
            .expect("receive");

        assert_eq!(public_sockets, PublicSocketsHandoff::UnlinkOnRestored);
        assert!(received.listeners.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_import_server_renames_its_listeners_over_the_paths_of_a_source_from_before_the_transfer()
    {
        let dir = socket_test_dir("take");
        let api_path = dir.join("a.sock");
        let client_path = dir.join("c.sock");
        let old_api = listen(&api_path);
        let old_client = listen(&client_path);
        let old_api_identity = crate::ipc::socket_file_identity(&api_path).expect("api");
        let old_client_identity = crate::ipc::socket_file_identity(&client_path).expect("client");
        // The source is this process, as the stream's peer and as the
        // listeners' holder.
        let (_source, import) = UnixStream::pair().expect("handoff stream");

        let (new_api, new_client) =
            take_over_public_paths(&import, &api_path, &client_path, |api, client| {
                // Before the rename, the paths still lead to the source.
                let _api = crate::ipc::connect_local_stream(&api_path).expect("connect");
                let _client = crate::ipc::connect_local_stream(&client_path).expect("connect");
                Ok((api, client))
            })
            .expect("the import server takes both paths");
        // The probe above and the take-over's own check reached the source.
        assert_eq!(take_connections(&old_api), 2);
        assert_eq!(take_connections(&old_client), 2);
        // The source's unlinks on `restored`, each only of its own file,
        // change nothing.
        let _ = crate::ipc::remove_socket_file_if_owned(&api_path, &old_api_identity);
        let _ = crate::ipc::remove_socket_file_if_owned(&client_path, &old_client_identity);

        assert_eq!(new_api.path, api_path);
        assert_eq!(
            crate::ipc::socket_file_identity(&api_path).ok(),
            Some(new_api.identity.clone())
        );
        let _api = crate::ipc::connect_local_stream(&api_path).expect("connect");
        let _client = crate::ipc::connect_local_stream(&client_path).expect("connect");
        assert_eq!(take_connections(&new_api.listener), 1);
        assert_eq!(take_connections(&new_client.listener), 1);
        assert_eq!(take_connections(&old_api), 0);
        assert_eq!(take_connections(&old_client), 0);
        let staging_left = std::fs::read_dir(&dir)
            .expect("read the test dir")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".h"));
        assert!(!staging_left, "a staging socket was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_listener_on_a_public_path_refuses_the_take_over_by_name() {
        let dir = socket_test_dir("foreign");
        let path = dir.join("c.sock");
        let foreign = listen(&path);
        let file = SocketFileId::of(&path).expect("foreign file");
        // A source other than this process, which holds the listener.
        let source = Some(std::process::id().wrapping_add(1));

        let err = install_staged(&dir.join("unused.sock"), &path, source)
            .expect_err("a live foreign listener is refused");

        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        let message = err.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "{message}"
        );
        assert_eq!(SocketFileId::of(&path).ok(), Some(file));
        assert_eq!(
            take_connections(&foreign),
            1,
            "the refused probe reached it"
        );
        // A path nobody accepts on is replaced.
        drop(foreign);
        assert!(check_held_by_source(&path, source).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wait_for_the_commit_ends_on_the_commit_a_stop_or_a_closed_stream() {
        use std::os::fd::AsFd as _;

        let (mut source, mut import) = UnixStream::pair().expect("handoff stream");
        let (_wake_tx, wake_rx) = UnixStream::pair().expect("wake");
        source.write_all(b"committed\nnext").expect("commit");
        wait_committed(&mut import, wake_rx.as_fd()).expect("committed");
        let mut next = [0u8; 4];
        import.read_exact(&mut next).expect("the rest stays unread");
        assert_eq!(&next, b"next");

        // A commit that came with a stop wins: the source let go of the panes.
        let (wake_tx, wake_rx) = UnixStream::pair().expect("wake");
        (&wake_tx).write_all(&[1]).expect("wake");
        source.write_all(b"committed\n").expect("commit");
        wait_committed(&mut import, wake_rx.as_fd()).expect("committed");

        let stopped = wait_committed(&mut import, wake_rx.as_fd()).expect_err("stopped");
        assert_eq!(stopped.kind(), io::ErrorKind::Interrupted);

        let (_wake_tx, wake_rx) = UnixStream::pair().expect("wake");
        drop(source);
        let closed = wait_committed(&mut import, wake_rx.as_fd()).expect_err("closed");
        assert_eq!(closed.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_peer_description_names_its_pid() {
        assert_eq!(
            described_pid("pid 42 (herdr), parent pid 7 (zsh)"),
            Some(42)
        );
        assert_eq!(described_pid("pid 42"), Some(42));
        assert_eq!(described_pid("an unknown process"), None);
    }
}
