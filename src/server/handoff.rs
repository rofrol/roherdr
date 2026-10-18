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
    /// The source whose unbound usage admissions the committed import retires.
    /// Optional for handoffs from servers predating admission reconciliation.
    #[serde(default)]
    pub admission_server: Option<(u32, Option<u64>)>,
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
    /// Present when the source passes the session lock it holds
    /// (`session_lock`) over this stream to an import server that takes it
    /// (`take-server-lock` in its `validated` line), so the lock is held
    /// throughout the handoff and never released between the two servers.
    /// Absent from manifests written before this field existed: such a
    /// source holds no lock, and the import server takes it by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_lock: Option<ServerLockOffer>,
}

/// The words an import server adds to its `validated` line for what it
/// takes of the source's offer, each only when offered. An import server
/// from before the listener transfer sends a bare `validated`.
#[cfg(unix)]
const TAKE_LISTENERS: &str = "take-public-listeners";
#[cfg(unix)]
const TAKE_SERVER_LOCK: &str = "take-server-lock";

/// The session lock a source offers to pass, sent after the listeners.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServerLockOffer {
    pub path: PathBuf,
}

/// What the import server on the other end takes of the source's offer.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImportTakes {
    pub public_sockets: PublicSocketsHandoff,
    /// It takes the session lock's descriptor. An import server from before
    /// the lock does not: the source holds the lock until it exits, and
    /// nothing holds it for that import server's life.
    pub server_lock: bool,
}

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
    /// The session lock's descriptor, when the source offered it.
    pub server_lock: Option<std::os::fd::OwnedFd>,
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
) -> io::Result<(UnixStream, ImportTakes)> {
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
    let takes = parse_validated(validated.trim_end(), manifest)
        .ok_or_else(|| io::Error::other("handoff import did not validate manifest"))?;
    let _ = std::fs::remove_file(socket_path);
    Ok((stream, takes))
}

/// Reads an import server's `validated` line: `validated`, then a word for
/// each part of the offer it takes. A word for something not offered, or
/// one this build does not know, refuses the handoff.
#[cfg(unix)]
fn parse_validated(line: &str, manifest: &HandoffManifest) -> Option<ImportTakes> {
    let mut words = line.split(' ');
    if words.next() != Some("validated") {
        return None;
    }
    let mut takes = ImportTakes {
        public_sockets: PublicSocketsHandoff::UnlinkOnRestored,
        server_lock: false,
    };
    for word in words {
        match word {
            TAKE_LISTENERS if manifest.public_listeners.is_some() => {
                takes.public_sockets = PublicSocketsHandoff::PassListeners;
            }
            TAKE_SERVER_LOCK if manifest.server_lock.is_some() => takes.server_lock = true,
            _ => return None,
        }
    }
    Some(takes)
}

/// The import server's `validated` line for what `manifest` offers.
#[cfg(unix)]
fn validated_line(manifest: &HandoffManifest) -> String {
    let mut line = String::from("validated");
    if manifest.public_listeners.is_some() {
        line.push(' ');
        line.push_str(TAKE_LISTENERS);
    }
    if manifest.server_lock.is_some() {
        line.push(' ');
        line.push_str(TAKE_SERVER_LOCK);
    }
    line.push('\n');
    line
}

/// Sends the pane descriptors, then the session lock when the import server
/// takes it, and waits for `restored`.
#[cfg(unix)]
pub(crate) fn send_fds_and_wait_restored(
    stream: &mut UnixStream,
    fds: &[RawFd],
    server_lock: Option<std::os::fd::BorrowedFd<'_>>,
) -> io::Result<()> {
    send_fds(stream, fds)?;
    send_server_lock(stream, server_lock)?;
    wait_restored(stream)
}

/// Passes the session lock's descriptor. The source keeps its own copy, so
/// the lock stays held whatever the import server does with it.
#[cfg(unix)]
fn send_server_lock(
    stream: &UnixStream,
    server_lock: Option<std::os::fd::BorrowedFd<'_>>,
) -> io::Result<()> {
    match server_lock {
        Some(fd) => send_fd_batch(stream, &[fd.as_raw_fd()]),
        None => Ok(()),
    }
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
    /// Both listeners (and the session lock, when taken) are sent; the
    /// import server has not reported restored.
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
/// `server_lock`, when the import server takes it, is sent after the
/// listeners; the caller keeps its copy until the commit, so the lock is
/// held throughout and still the source's after a failure.
#[cfg(unix)]
pub(crate) fn pass_public_listeners(
    stream: &mut UnixStream,
    pane_fds: &[RawFd],
    listeners: &mut impl SourceListeners,
    server_lock: Option<std::os::fd::BorrowedFd<'_>>,
    reap_import: impl FnOnce(),
    at: &mut dyn FnMut(ListenerHandoffPoint),
) -> io::Result<()> {
    listeners.pause_accepting();
    at(ListenerHandoffPoint::Paused);
    let handed_off = (|| {
        send_fds(stream, pane_fds)?;
        let [api, client] = listeners.listener_fds()?;
        send_listener_fds(stream, api, client)?;
        send_server_lock(stream, server_lock)?;
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
    stream.write_all(validated_line(&manifest).as_bytes())?;
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
    let server_lock = if manifest.server_lock.is_some() {
        match recv_fds(&stream, 1) {
            Ok(lock) => {
                use std::os::fd::FromRawFd as _;

                // `recv_fds` returns exactly the one descriptor asked for,
                // new in this process.
                lock.first()
                    .map(|fd| unsafe { std::os::fd::OwnedFd::from_raw_fd(*fd) })
            }
            Err(err) => {
                // The listeners close as they drop.
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
        server_lock,
        stream,
    })
}

/// The session lock this import server holds: the one the source passed,
/// or, from a source from before the lock (which holds none), the lock
/// taken from its path, refused while another server holds it. Must run
/// before the import restores anything. A passed lock must be this
/// server's own lock file by identity (device and inode), whatever path the
/// source spelled it with (a symlinked state directory, `/tmp` against
/// `/private/tmp` on macOS); its offer's path only names it in messages.
pub(crate) fn import_session_lock(
    passed: Option<std::os::fd::OwnedFd>,
    offer: Option<&ServerLockOffer>,
    own_path: &Path,
) -> io::Result<crate::server::session_lock::SessionLock> {
    match (passed, offer) {
        (Some(fd), Some(offer)) => {
            crate::server::session_lock::SessionLock::adopt_passed(fd, own_path).map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "the handoff source's session lock ({}) is not this server's ({}): {err}",
                        offer.path.display(),
                        own_path.display()
                    ),
                )
            })
        }
        (None, None) => crate::server::session_lock::SessionLock::acquire(own_path),
        _ => Err(io::Error::other(
            "the handoff's session lock does not match its offer",
        )),
    }
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

/// The server whose unrecorded usage admissions the import retires: the
/// identity the manifest carries, or, for a source from before that field
/// existed, the pid the stream's peer names, with the start token read now
/// while that process still runs. `None` when neither is known: the import
/// then leaves the source's rows to the next server start.
#[cfg(unix)]
pub(crate) fn admission_server(
    manifest: Option<(u32, Option<u64>)>,
    source_pid: Option<u32>,
) -> Option<(u32, Option<u64>)> {
    manifest.or_else(|| {
        let pid = source_pid?;
        Some((pid, crate::platform::process_start_token(pid)))
    })
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
pub(crate) fn source_pid(stream: &UnixStream) -> Option<u32> {
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
        admission_server: Some((
            std::process::id(),
            crate::platform::process_start_token(std::process::id()),
        )),
        source_protocol: crate::protocol::PROTOCOL_VERSION,
        expected_version,
        expected_protocol,
        snapshot,
        panes,
        api_window_title,
        run_envs,
        pty_alerts: None,
        public_listeners: None,
        server_lock: None,
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
    // `SCM_RIGHTS` delivers them without `FD_CLOEXEC`: no process this
    // server starts may inherit a pane, a listener or the session lock.
    for fd in &out {
        let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        if flags >= 0 {
            unsafe { libc::fcntl(*fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        }
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
        assert!(older.server_lock.is_none());
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

        let (mut stream, takes) =
            accept_and_validate_on(listener, &handoff_path, "token", &manifest)
                .expect("the import server validates");
        assert_eq!(takes.public_sockets, PublicSocketsHandoff::PassListeners);
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
            None,
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
            None,
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

        let (_stream, takes) = accept_and_validate_on(listener, &handoff_path, "token", &manifest)
            .expect("the older import server validates");
        let _import = importer.join().expect("the importer thread");

        assert_eq!(takes.public_sockets, PublicSocketsHandoff::UnlinkOnRestored);
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
        let (_stream, takes) = accept_and_validate_on(listener, &handoff_path, "token", &manifest)
            .expect("the import server validates");
        let received = importer
            .join()
            .expect("the importer thread")
            .expect("receive");

        assert_eq!(takes.public_sockets, PublicSocketsHandoff::UnlinkOnRestored);
        assert!(!takes.server_lock);
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
    fn a_manifest_without_the_source_identity_falls_back_to_the_peers_pid() {
        let manifest = manifest_offering(None);
        let pid = std::process::id();
        let mut json = serde_json::to_value(&manifest).expect("a manifest");
        assert_eq!(
            json.get("admission_server"),
            Some(&serde_json::json!([
                pid,
                crate::platform::process_start_token(pid)
            ])),
            "a source names itself"
        );
        // A source from before the field sends none, and the manifest still
        // deserializes.
        json.as_object_mut()
            .expect("an object")
            .remove("admission_server");
        let older: HandoffManifest = serde_json::from_value(json).expect("an older manifest");
        assert_eq!(older.admission_server, None);
        // Its stream's peer names the pid; the start token is read while
        // that process still runs, so a later process with the pid cannot
        // pass for it.
        assert_eq!(
            admission_server(None, Some(pid)),
            Some((pid, crate::platform::process_start_token(pid)))
        );
        // A manifest that names its source is used as it is.
        assert_eq!(
            admission_server(Some((7, Some(11))), Some(pid)),
            Some((7, Some(11)))
        );
        // Neither: the import leaves the source's rows to the next start.
        assert_eq!(admission_server(None, None), None);
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

    /// Whether a holder has the lock at `path`, by trying to take it: only
    /// in these tests, where no server start contends for the file.
    fn lock_held(path: &Path) -> bool {
        crate::platform::ExclusiveFileLock::try_acquire(path)
            .expect("the session lock file")
            .is_none()
    }

    /// A source's listeners and session lock, offered in its manifest.
    fn lock_handoff_source(
        dir: &Path,
    ) -> (
        TestSource,
        crate::server::session_lock::SessionLock,
        HandoffManifest,
        std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    ) {
        let api_path = dir.join("a.sock");
        let client_path = dir.join("c.sock");
        let lock_path = dir.join("herdr-server.lock");
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let source = TestSource {
            api: Some(listen(&api_path)),
            client: Some(listen(&client_path)),
            log: log.clone(),
        };
        let lock = crate::server::session_lock::SessionLock::acquire(&lock_path)
            .expect("the source holds the session lock");
        let mut manifest = manifest_offering(Some(offer_for(&api_path, &client_path)));
        manifest.server_lock = Some(ServerLockOffer {
            path: lock_path.clone(),
        });
        (source, lock, manifest, log)
    }

    // Report of 2026-10-16: a client that found no socket during a failed
    // handoff started a second server. The lock that refuses it is passed
    // with the listeners, so it is held at every point, the transfer point
    // included, and never free between the two servers.
    #[test]
    fn the_session_lock_is_held_throughout_a_live_handoff() {
        use std::os::fd::AsFd as _;

        let dir = socket_test_dir("lock");
        let lock_path = dir.join("herdr-server.lock");
        let handoff_path = dir.join("h.sock");
        let (mut source, source_lock, manifest, log) = lock_handoff_source(&dir);
        let listener = bind_listener(&handoff_path).expect("bind the handoff socket");
        let (adopted_tx, adopted_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let importer = {
            let handoff_path = handoff_path.clone();
            let lock_path = lock_path.clone();
            std::thread::spawn(move || -> io::Result<_> {
                let mut received = receive(&handoff_path, "token")?;
                let lock = import_session_lock(
                    received.server_lock.take(),
                    received.manifest.server_lock.as_ref(),
                    &lock_path,
                )?;
                let _ = adopted_tx.send(());
                go_rx.recv().map_err(io::Error::other)?;
                report_restored(&mut received.stream)?;
                report_ready(&mut received.stream)?;
                let (_wake_tx, wake_rx) = UnixStream::pair()?;
                wait_committed(&mut received.stream, wake_rx.as_fd())?;
                report_owned(&mut received.stream)?;
                Ok((lock, received.listeners.take()))
            })
        };
        let (mut stream, takes) =
            accept_and_validate_on(listener, &handoff_path, "token", &manifest)
                .expect("the import server validates");
        assert!(takes.server_lock);
        let mut checked = Vec::new();
        let mut probe = |point: ListenerHandoffPoint| {
            if point == ListenerHandoffPoint::Passed {
                // The import server holds its copy and waits.
                adopted_rx.recv().expect("the import server took the lock");
            }
            assert!(
                lock_held(&lock_path),
                "{point:?}: the session lock was free"
            );
            checked.push(point);
            if point == ListenerHandoffPoint::Passed {
                go_tx.send(()).expect("release the import server");
            }
        };
        pass_public_listeners(
            &mut stream,
            &[],
            &mut source,
            Some(source_lock.as_fd()),
            || panic!("a successful handoff reaps nothing"),
            &mut probe,
        )
        .expect("the handoff commits");
        wait_owned_ack(&mut stream);
        assert_eq!(
            checked,
            vec![
                ListenerHandoffPoint::Paused,
                ListenerHandoffPoint::Passed,
                ListenerHandoffPoint::Ready
            ]
        );
        // The source lets go of its copy once committed, as the server does.
        drop(source_lock);
        let (import_lock, _listeners) = importer
            .join()
            .expect("the importer thread")
            .expect("the import server takes the lock");
        assert!(lock_held(&lock_path), "the import server holds the lock");
        assert_eq!(*log.borrow(), vec!["pause", "release"]);
        drop(import_lock);
        assert!(!lock_held(&lock_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_import_leaves_the_session_lock_with_the_source() {
        let dir = socket_test_dir("lockfail");
        let lock_path = dir.join("herdr-server.lock");
        let handoff_path = dir.join("h.sock");
        let (mut source, source_lock, manifest, log) = lock_handoff_source(&dir);
        let listener = bind_listener(&handoff_path).expect("bind the handoff socket");
        let importer = {
            let handoff_path = handoff_path.clone();
            let lock_path = lock_path.clone();
            std::thread::spawn(move || {
                // Takes the lock, then fails before `restored`: its copy
                // closes as a failed import server's does when it exits.
                let mut received = receive(&handoff_path, "token").expect("receive");
                let lock = import_session_lock(
                    received.server_lock.take(),
                    received.manifest.server_lock.as_ref(),
                    &lock_path,
                )
                .expect("the passed lock");
                drop(lock);
            })
        };

        let (mut stream, takes) =
            accept_and_validate_on(listener, &handoff_path, "token", &manifest)
                .expect("the import server validates");
        assert!(takes.server_lock);
        let reap_log = log.clone();
        let reap_lock_path = lock_path.clone();
        pass_public_listeners(
            &mut stream,
            &[],
            &mut source,
            Some(source_lock.as_fd()),
            || {
                importer.join().expect("the importer thread");
                // The import server is gone; its copy is closed.
                assert!(lock_held(&reap_lock_path), "the source's lock was let go");
                reap_log.borrow_mut().push("reap");
            },
            &mut |_| {},
        )
        .expect_err("the import failed");

        assert_eq!(*log.borrow(), vec!["pause", "reap", "resume"]);
        assert!(lock_held(&lock_path), "the source still holds the lock");
        // Nobody else can take it while the source runs on.
        assert!(crate::server::session_lock::SessionLock::acquire(&lock_path).is_err());
        drop(source_lock);
        assert!(!lock_held(&lock_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_import_server_takes_the_lock_by_path_from_a_source_from_before_the_lock() {
        let dir = socket_test_dir("lockold");
        let lock_path = dir.join("herdr-server.lock");

        // Such a source holds no lock and offers none.
        let lock = import_session_lock(None, None, &lock_path).expect("free: taken by path");
        assert!(lock_held(&lock_path));
        // While another server holds it, the import is refused before it
        // restores anything.
        let err = import_session_lock(None, None, &lock_path)
            .err()
            .expect("held");
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        drop(lock);
        // A passed lock without an offer, or an offer without one, is refused.
        assert!(import_session_lock(
            None,
            Some(&ServerLockOffer {
                path: lock_path.clone()
            }),
            &lock_path
        )
        .is_err());
        // An offer for another session's lock is refused.
        let other = crate::server::session_lock::SessionLock::acquire(&dir.join("o-server.lock"))
            .expect("other lock");
        let fd = std::os::fd::BorrowedFd::try_clone_to_owned(&other.as_fd()).expect("dup");
        assert!(import_session_lock(
            Some(fd),
            Some(&ServerLockOffer {
                path: dir.join("o-server.lock")
            }),
            &lock_path
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Review round 20261016-170554-f593: the import compared the offered
    // path with its own as strings, so a source that spelled the state
    // directory another way (`/tmp` against `/private/tmp` on macOS, a
    // symlink) failed a legitimate handoff. The file's identity decides.
    #[test]
    fn a_passed_lock_is_adopted_under_another_spelling_of_its_path() {
        let dir = socket_test_dir("lockalias");
        let real = dir.join("state");
        std::fs::create_dir_all(&real).expect("state dir");
        let alias = dir.join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("symlink");
        let own_path = real.join("herdr-server.lock");
        let source_path = alias.join("herdr-server.lock");
        assert_ne!(own_path, source_path);

        let source_lock = crate::server::session_lock::SessionLock::acquire(&source_path)
            .expect("the source holds the session lock");
        let fd = std::os::fd::BorrowedFd::try_clone_to_owned(&source_lock.as_fd()).expect("dup");
        let import_lock = import_session_lock(
            Some(fd),
            Some(&ServerLockOffer {
                path: source_path.clone(),
            }),
            &own_path,
        )
        .expect("the same file under another spelling");
        drop(source_lock);
        assert!(lock_held(&own_path), "the import server holds the lock");
        drop(import_lock);
        assert!(!lock_held(&own_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A source that hands its socket paths over by name (the import server
    // took no listeners) still passes the lock, after the pane descriptors.
    #[test]
    fn the_session_lock_passes_with_the_pane_descriptors_in_a_by_path_handoff() {
        let dir = socket_test_dir("lockpath");
        let lock_path = dir.join("herdr-server.lock");
        let source_lock = crate::server::session_lock::SessionLock::acquire(&lock_path)
            .expect("the source holds the session lock");
        let (mut source, mut import) = UnixStream::pair().expect("handoff stream");
        let offer = ServerLockOffer {
            path: lock_path.clone(),
        };
        let (adopted_tx, adopted_rx) = std::sync::mpsc::channel();
        let importer = {
            let lock_path = lock_path.clone();
            std::thread::spawn(move || -> io::Result<_> {
                use std::os::fd::FromRawFd as _;

                let fd = recv_fds(&import, 1)?
                    .first()
                    .map(|fd| unsafe { std::os::fd::OwnedFd::from_raw_fd(*fd) });
                let lock = import_session_lock(fd, Some(&offer), &lock_path)?;
                let _ = adopted_tx.send(());
                report_restored(&mut import)?;
                Ok(lock)
            })
        };

        send_fds_and_wait_restored(&mut source, &[], Some(source_lock.as_fd())).expect("restored");
        adopted_rx.recv().expect("the import server took the lock");
        drop(source_lock);
        let import_lock = importer
            .join()
            .expect("the importer thread")
            .expect("the passed lock");
        assert!(lock_held(&lock_path), "the import server holds the lock");
        assert!(crate::server::session_lock::SessionLock::acquire(&lock_path).is_err());
        drop(import_lock);
        assert!(!lock_held(&lock_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_validated_line_names_only_what_was_offered() {
        let mut manifest = manifest_offering(None);
        assert_eq!(validated_line(&manifest), "validated\n");
        let bare = parse_validated("validated", &manifest).expect("bare");
        assert_eq!(bare.public_sockets, PublicSocketsHandoff::UnlinkOnRestored);
        assert!(!bare.server_lock);
        // Not offered: refused.
        assert!(parse_validated("validated take-server-lock", &manifest).is_none());
        assert!(parse_validated("validated take-public-listeners", &manifest).is_none());

        manifest.server_lock = Some(ServerLockOffer {
            path: PathBuf::from("/s/herdr-server.lock"),
        });
        assert_eq!(validated_line(&manifest), "validated take-server-lock\n");
        let takes = parse_validated("validated take-server-lock", &manifest).expect("lock");
        assert!(takes.server_lock);
        // An import server from before the lock ignores the offer.
        assert!(
            !parse_validated("validated", &manifest)
                .expect("older")
                .server_lock
        );
        assert!(parse_validated("validated take-something-else", &manifest).is_none());
        assert!(parse_validated("refused", &manifest).is_none());
    }
}
