//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

#[cfg(unix)]
pub(crate) mod ssh_agent;

pub(crate) struct HostShutdownMonitor {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostShutdownMonitor {
    pub(crate) fn start(
        requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let task = monitor_host_shutdown(requested, wake);
        Self { task }
    }
}

impl Drop for HostShutdownMonitor {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn monitor_host_shutdown(
    _requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _wake: impl Fn() + Send + Sync + 'static,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

/// System-wide pseudo-terminal allocation: how many are open and the kernel limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SystemPtyUsage {
    pub in_use: u32,
    pub max: u32,
}

/// Counts the system's live pseudo-terminals, or `None` where the platform
/// has no fixed PTY pool or the count cannot be read. Reads the filesystem:
/// call it per spawn or on a slow timer, never per frame.
pub(crate) fn system_pty_usage() -> Option<SystemPtyUsage> {
    system_pty_usage_platform()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

/// A request from outside the process to stop the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerQuitSignal {
    #[cfg(unix)]
    Interrupt,
    #[cfg(unix)]
    Terminate,
    #[cfg(not(unix))]
    ConsoleControl,
}

impl std::fmt::Display for ServerQuitSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(unix)]
            Self::Interrupt => "SIGINT",
            #[cfg(unix)]
            Self::Terminate => "SIGTERM",
            #[cfg(not(unix))]
            Self::ConsoleControl => "console control event",
        })
    }
}

#[cfg(not(unix))]
pub(crate) fn spawn_server_signal_monitor(
    on_quit: impl Fn(ServerQuitSignal) + Send + Sync + 'static,
) {
    if let Err(err) = ctrlc::set_handler(move || on_quit(ServerQuitSignal::ConsoleControl)) {
        tracing::warn!(%err, "failed to install server stop handler");
    }
}

#[cfg(not(unix))]
pub(crate) fn ignore_server_hangup() {}

#[cfg(not(unix))]
pub(crate) fn local_stream_peer_description(_stream: &crate::ipc::LocalStream) -> Option<String> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

/// Why a pane runtime ended, before application persistence policy is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExitReason {
    Exited,
    Interrupted,
    /// Imported runtimes have no child wait handle in the replacement server.
    #[cfg(unix)]
    Handoff,
    WaitFailed,
}

impl ChildExitReason {
    pub(crate) fn requires_session_checkpoint(self) -> bool {
        match self {
            Self::Interrupted => true,
            #[cfg(unix)]
            Self::Handoff => true,
            _ => false,
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_common::{
    classify_child_exit, poll_fd_readable, read_fd, shared_ssh_control_path,
};

#[cfg(not(any(unix, windows)))]
pub(crate) fn classify_child_exit(_status: &portable_pty::ExitStatus) -> ChildExitReason {
    ChildExitReason::Exited
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn launch_executable() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

pub(crate) fn detached_custom_command_process(command: &str) -> std::process::Command {
    let mut process = detached_custom_command_process_platform(command);
    configure_background_command(&mut process);
    process
}

/// Quotes one value for a command run by `detached_custom_command_process`.
pub(crate) fn custom_command_argument(value: &str) -> String {
    custom_command_argument_platform(value)
}

#[cfg(unix)]
fn custom_command_argument_platform(value: &str) -> String {
    unix_common::remote_reattach_argument(value)
}

#[cfg(not(unix))]
fn custom_command_argument_platform(value: &str) -> String {
    quote_windows_command_line_arg(value)
}

pub(crate) fn pane_custom_command_pty_builder(command: &str) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_platform(command)
}

pub(crate) fn apply_pane_runtime_marker(command: &mut portable_pty::CommandBuilder) {
    apply_pane_runtime_marker_platform(command);
}

pub(crate) fn prepare_paste_text_for_pty(text: String) -> String {
    prepare_paste_text_for_pty_platform(text)
}

pub(crate) fn plugin_runtime_path(path: &std::path::Path) -> std::path::PathBuf {
    plugin_runtime_path_platform(path)
}

pub(crate) fn normalize_cwd_for_launch(path: &std::path::Path) -> std::path::PathBuf {
    normalize_cwd_for_launch_platform(path)
}

#[cfg(not(windows))]
fn normalize_cwd_for_launch_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn plugin_runtime_path_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn prepare_paste_text_for_pty_platform(text: String) -> String {
    text
}

#[cfg(not(windows))]
pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title
}

#[cfg(not(windows))]
fn apply_pane_runtime_marker_platform(_command: &mut portable_pty::CommandBuilder) {}

pub(crate) fn configure_background_command(command: &mut std::process::Command) {
    configure_background_command_platform(command);
}

#[cfg(not(windows))]
fn configure_background_command_platform(_command: &mut std::process::Command) {}

/// Prepares a headless worker's command: its own process group, so the whole
/// group can be signalled without touching the server.
pub(crate) fn configure_worker_process(command: &mut std::process::Command) {
    configure_background_command(command);
    configure_worker_process_platform(command);
}

#[cfg(unix)]
fn configure_worker_process_platform(command: &mut std::process::Command) {
    std::os::unix::process::CommandExt::process_group(command, 0);
}

#[cfg(not(unix))]
fn configure_worker_process_platform(_command: &mut std::process::Command) {}

/// Whether Claude Code's Bash sandbox exists here (seatbelt on macOS,
/// bubblewrap on Linux). Headless workers are refused without it; on Linux
/// the worker's `failIfUnavailable` ends a CLI that cannot start it.
pub(crate) const WORKER_SANDBOX_SUPPORTED: bool =
    cfg!(any(target_os = "linux", target_os = "macos"));

/// Whether any process of the group led by `leader_pid` is still alive.
pub(crate) fn process_group_alive(leader_pid: u32) -> bool {
    process_group_alive_platform(leader_pid)
}

#[cfg(unix)]
fn process_group_alive_platform(leader_pid: u32) -> bool {
    unix_common::process_group_alive(leader_pid)
}

#[cfg(not(unix))]
fn process_group_alive_platform(leader_pid: u32) -> bool {
    !session_processes(leader_pid).is_empty()
}

/// Creates `path` as a new directory only its owner can open (0700 where
/// the platform has modes); fails when anything already exists there.
pub(crate) fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    create_private_dir_platform(path)
}

#[cfg(unix)]
fn create_private_dir_platform(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().mode(0o700).create(path)?;
    // The umask may have narrowed the mode; widen it back to exactly 0700.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir_platform(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

/// Signals the process group led by `leader_pid` (started through
/// [`configure_worker_process`]). Returns `Ok(false)` when the group is gone.
pub(crate) fn signal_process_group(leader_pid: u32, signal: Signal) -> std::io::Result<bool> {
    signal_process_group_platform(leader_pid, signal)
}

#[cfg(unix)]
fn signal_process_group_platform(leader_pid: u32, signal: Signal) -> std::io::Result<bool> {
    unix_common::signal_process_group(leader_pid, signal)
}

#[cfg(not(unix))]
fn signal_process_group_platform(leader_pid: u32, signal: Signal) -> std::io::Result<bool> {
    let pids = session_processes(leader_pid);
    if pids.is_empty() {
        return Ok(false);
    }
    signal_processes(&pids, signal);
    Ok(true)
}

/// The signal that ended a process, where the platform has signals.
pub(crate) fn exit_status_signal(status: &std::process::ExitStatus) -> Option<i32> {
    exit_status_signal_platform(status)
}

#[cfg(unix)]
fn exit_status_signal_platform(status: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn exit_status_signal_platform(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Sessions of `root_pid`'s descendants other than `own_session`, from a
/// `(pid, parent)` table. Shared by the platforms that have POSIX sessions.
// Only the Linux and macOS process tables call it; Windows has no sessions.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn sessions_of_descendants(
    root_pid: u32,
    parents: &[(u32, u32)],
    own_session: u32,
    session_of: impl Fn(u32) -> Option<u32>,
) -> Vec<u32> {
    let mut frontier = vec![root_pid];
    let mut seen = std::collections::BTreeSet::new();
    let mut sessions = std::collections::BTreeSet::new();
    while let Some(parent) = frontier.pop() {
        for &(pid, _) in parents
            .iter()
            .filter(|(pid, ppid)| *ppid == parent && *pid != parent)
        {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(session) = session_of(pid).filter(|session| *session != own_session) {
                sessions.insert(session);
            }
            frontier.push(pid);
        }
    }
    sessions.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformCapabilities {
    pub(crate) live_handoff: bool,
    pub(crate) direct_terminal_attach: bool,
    pub(crate) preserve_legacy_doubled_escape_input: bool,
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: cfg!(unix),
        direct_terminal_attach: cfg!(unix),
        preserve_legacy_doubled_escape_input: cfg!(target_os = "macos"),
    }
}

/// The byte the host terminal sends for Backspace according to the tty's erase
/// setting (e.g. `^H` for MobaXterm and PuTTY-style terminals), when known.
pub(crate) fn terminal_erase_byte() -> Option<u8> {
    #[cfg(unix)]
    return unix_common::terminal_erase_byte();
    #[cfg(not(unix))]
    None
}

pub(crate) fn terminal_grid_size() -> std::io::Result<(u16, u16)> {
    #[cfg(unix)]
    let (cols, rows) = unix_common::read_terminal_grid_size()?;
    #[cfg(windows)]
    let (cols, rows) = windows::read_terminal_grid_size()?;
    #[cfg(not(any(unix, windows)))]
    let (cols, rows) = fallback::read_terminal_grid_size()?;

    if cols == 0 || rows == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "terminal reported a zero-sized grid",
        ));
    }
    Ok((cols, rows))
}

#[cfg(not(windows))]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    command.spawn().map(|child| child.id())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn prepare_server_process(_handoff_import: bool) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    #[cfg(target_os = "macos")]
    macos::configure_server_daemon_context(command);

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn current_process_is_detached_server_daemon() -> bool {
    unsafe { libc::getsid(0) == libc::getpid() }
}

/// Raised by the SIGWINCH handler, consumed by the host resize watcher.
#[cfg(unix)]
static TERMINAL_RESIZE_SIGNALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_terminal_resize_signal(_signal: libc::c_int) {
    TERMINAL_RESIZE_SIGNALLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Records SIGWINCH events that size polling can miss.
#[cfg(unix)]
pub(crate) fn watch_terminal_resize_signal() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction =
        record_terminal_resize_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // Keep blocking stdin and socket reads from failing with EINTR.
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn watch_terminal_resize_signal() {}

/// Returns whether a terminal size change was signalled since the last call.
#[cfg(unix)]
pub(crate) fn take_terminal_resize_signal() -> bool {
    TERMINAL_RESIZE_SIGNALLED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Windows relies on size polling.
#[cfg(not(unix))]
pub(crate) fn take_terminal_resize_signal() -> bool {
    false
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LimitedRead {
    Empty,
    Complete(Vec<u8>),
    Oversized,
}

pub(crate) fn read_limited_reader(
    mut reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];

    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_len = remaining.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if bytes_read == 0 {
            return if bytes.is_empty() {
                Ok(LimitedRead::Empty)
            } else {
                Ok(LimitedRead::Complete(bytes))
            };
        }
        bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let mut sentinel = [0_u8; 1];
    loop {
        return match reader.read(&mut sentinel) {
            Ok(0) if bytes.is_empty() => Ok(LimitedRead::Empty),
            Ok(0) => Ok(LimitedRead::Complete(bytes)),
            Ok(_) => Ok(LimitedRead::Oversized),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
    pub(crate) multiplexing: bool,
}

pub(crate) const REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED: bool =
    cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(unix)]
mod remote_bridge;
#[cfg(all(test, unix))]
mod remote_bridge_tests;
#[cfg(unix)]
mod unix_common;
#[cfg(unix)]
pub(crate) mod unix_image_files;
#[cfg(unix)]
pub(crate) use unix_common::{
    begin_cli_output, end_cli_output, forward_remote_bridge_stdio, ignore_server_hangup,
    local_stream_peer_description, spawn_server_signal_monitor, RemoteBridgeWake,
};
/// A headless worker's broker: its own session, so it outlives the server
/// (`start_new_session`), and a guard that kills the worker when the broker
/// dies (`fork_death_guard`).
#[cfg(unix)]
pub(crate) use unix_common::{fork_death_guard, start_new_session};

mod client_state;
pub(crate) use client_state::{create_private_state_file, replace_file, sync_parent_directory};

mod secret_file;
pub(crate) use secret_file::read_secret_file;

/// Optional presentation and click behavior for a desktop notification.
///
/// Platforms ignore the parts they cannot express; see
/// [`DesktopNotificationDetails::flattened_body`] for a portable body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopNotificationDetails {
    pub subtitle: Option<String>,
    /// Notifications sharing a group replace each other.
    pub group: Option<String>,
    pub on_click: Option<NotificationClickAction>,
}

impl DesktopNotificationDetails {
    /// Body for platforms without a subtitle line: `subtitle — body`.
    pub fn flattened_body(&self, body: Option<&str>) -> Option<String> {
        match (self.subtitle.as_deref(), body) {
            (Some(subtitle), Some(body)) if !body.is_empty() => {
                Some(format!("{subtitle} — {body}"))
            }
            (Some(subtitle), _) => Some(subtitle.to_owned()),
            (None, body) => body.map(str::to_owned),
        }
    }
}

/// Commands run when a notification is clicked, tried in order until one
/// succeeds. `env` applies to every command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationClickAction {
    pub env: Vec<(String, std::ffi::OsString)>,
    pub commands: Vec<Vec<std::ffi::OsString>>,
}

#[cfg(not(unix))]
pub(crate) fn begin_cli_output() {}

#[cfg(not(unix))]
pub(crate) fn end_cli_output() {}

/// Keychain-backed credentials exist only on macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) fn read_keychain_generic_password(_service: &str) -> Option<String> {
    None
}

/// Only macOS Keychain names need it; elsewhere nothing normalizes.
#[cfg(not(target_os = "macos"))]
pub(crate) fn normalize_nfc(_text: &str) -> Option<String> {
    None
}

/// Quick Look exists only on macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) fn quick_look(_path: &std::path::Path) -> Option<std::io::Result<std::process::Child>> {
    None
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod fallback;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use fallback::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn available_pane_shell_from_job(child_pid: u32, job: ForegroundJob) -> Option<String> {
    if job.process_group_id != child_pid
        || job.processes.iter().any(|process| process.pid != child_pid)
    {
        return None;
    }
    job.processes
        .into_iter()
        .find(|process| process.pid == child_pid)
        .map(|process| process.name)
        .filter(|name| is_pane_shell_process_name(name))
}

fn normalized_process_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_powershell_process_name(name: &str) -> bool {
    matches!(
        normalized_process_name(name).as_str(),
        "pwsh" | "powershell"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interactive_unix_shell_command(
    argv: &[String],
    shell_name: &str,
    quote_posix_arg: fn(&str) -> String,
) -> Option<String> {
    let quote = if is_powershell_process_name(shell_name) {
        quote_powershell_arg
    } else {
        quote_posix_arg
    };
    let mut parts = argv.iter();
    let mut command = quote(parts.next()?);
    for part in parts {
        command.push(' ');
        command.push_str(&quote(part));
    }
    Some(command)
}

pub(crate) fn quote_powershell_arg(value: &str) -> String {
    if !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn quote_windows_command_line_arg(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = normalized_process_name(name);
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_agent_hint(_pid: u32) -> Option<crate::detect::Agent> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        return crate::detect::parse_agent_label(std::str::from_utf8(value).ok()?);
    }
    None
}

/// The value of `key` in a NUL-separated `KEY=value` environment block.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn env_record_value(environ: &[u8], key: &str) -> Option<String> {
    environ.split(|&byte| byte == 0).find_map(|record| {
        let value = record.strip_prefix(key.as_bytes())?.strip_prefix(b"=")?;
        std::str::from_utf8(value).ok().map(str::to_owned)
    })
}

/// Whether a command line resumes the agent session `session_id`
/// (`--resume <id>` or `--resume=<id>`, as `claude` takes it).
pub(crate) fn argv_resumes_session(argv: &[String], session_id: &str) -> bool {
    argv.iter().enumerate().any(|(index, arg)| {
        (arg == "--resume" && argv.get(index + 1).is_some_and(|next| next == session_id))
            || arg
                .strip_prefix("--resume=")
                .is_some_and(|value| value == session_id)
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub(crate) struct InputSourceRestore;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn pump_input_source_runloop() {}

/// Switches the host keyboard input source while prefix mode is active.
///
/// `App` drives this through a trait so the prefix-mode transitions can be
/// tested with a fake, without touching the real macOS APIs or leaking a
/// platform-specific restore type into `App`.
pub(crate) trait PrefixInputSource {
    /// Switch to an ASCII-capable input source for prefix commands. No-op if
    /// the current source is already ASCII-capable, the platform is
    /// unsupported, or the switch fails. Calling it again before `restore`
    /// keeps the source saved by the first call.
    fn switch_to_ascii(&mut self);

    /// Restore whatever `switch_to_ascii` saved. No-op if nothing was switched.
    fn restore(&mut self);
}

/// Production [`PrefixInputSource`] backed by the per-platform API.
#[derive(Default)]
pub(crate) struct RealPrefixInputSource {
    restore: Option<InputSourceRestore>,
}

impl PrefixInputSource for RealPrefixInputSource {
    fn switch_to_ascii(&mut self) {
        if self.restore.is_none() {
            // Drain pending input-source-change notifications so the read below is fresh (see
            // `pump_input_source_runloop`); a no-op on non-macOS.
            pump_input_source_runloop();
            self.restore = switch_to_ascii_input_source();
        }
    }

    fn restore(&mut self) {
        let _ = self.restore.take();
    }
}

#[cfg(all(test, any(unix, windows)))]
#[test]
fn child_exit_classification_only_checkpoints_interruptions() {
    for code in [0, 1, 130, 255, 0xC0000005] {
        let reason = classify_child_exit(&portable_pty::ExitStatus::with_exit_code(code));
        assert_eq!(reason, ChildExitReason::Exited, "exit code {code:#x}");
        assert!(!reason.requires_session_checkpoint());
    }
    #[cfg(windows)]
    let status = portable_pty::ExitStatus::with_exit_code(0xC000013A);
    #[cfg(not(windows))]
    let status = portable_pty::ExitStatus::with_signal("Terminated: 15");
    assert_eq!(classify_child_exit(&status), ChildExitReason::Interrupted);
    assert!(classify_child_exit(&status).requires_session_checkpoint());
    #[cfg(unix)]
    assert!(ChildExitReason::Handoff.requires_session_checkpoint());
    assert!(!ChildExitReason::WaitFailed.requires_session_checkpoint());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn sessions_of_descendants_skips_the_roots_session_and_unrelated_trees() {
        // 10 -> 11 (own session) -> 12 (session 12) -> 13 (session 12);
        // 10 -> 14 (session 14); 20 (session 20) is not a descendant.
        let parents = [(11, 10), (12, 11), (13, 12), (14, 10), (20, 1)];
        let session_of = |pid: u32| {
            Some(match pid {
                11 => 5,
                13 => 12,
                other => other,
            })
        };
        assert_eq!(
            sessions_of_descendants(10, &parents, 5, session_of),
            vec![12, 14]
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn env_record_value_matches_the_whole_key() {
        let environ = b"HERDR_TAKEOVER_ID_OLD=x\0HERDR_TAKEOVER_ID=tk-1\0PATH=/bin\0";
        assert_eq!(
            env_record_value(environ, "HERDR_TAKEOVER_ID").as_deref(),
            Some("tk-1")
        );
        assert_eq!(env_record_value(environ, "HOME"), None);
    }

    #[test]
    fn argv_resumes_session_takes_both_spellings_and_only_that_session() {
        let argv = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| part.to_string())
                .collect::<Vec<_>>()
        };
        assert!(argv_resumes_session(
            &argv(&["node", "/bin/claude", "--resume", "s-1"]),
            "s-1"
        ));
        assert!(argv_resumes_session(
            &argv(&["claude", "--resume=s-1"]),
            "s-1"
        ));
        assert!(!argv_resumes_session(
            &argv(&["claude", "--resume", "s-2"]),
            "s-1"
        ));
        assert!(!argv_resumes_session(&argv(&["claude", "s-1"]), "s-1"));
    }

    #[test]
    fn system_pty_usage_reads_the_live_pool() {
        let usage = system_pty_usage().expect("PTY usage is readable");
        assert!(usage.max > 0, "{usage:?}");
        assert!(usage.in_use <= usage.max, "{usage:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_pane_process_group_rejects_processes_outside_the_pane_session() {
        use std::os::unix::process::CommandExt;

        let mut detached = std::process::Command::new("sleep");
        detached.arg("30");
        // SAFETY: setsid is async-signal-safe and touches only the child.
        unsafe {
            detached.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut detached = detached.spawn().expect("spawn detached");
        let token = process_start_token(detached.id()).expect("start token");
        let mut gone = std::process::Command::new("true").spawn().expect("spawn");
        let gone_pid = gone.id();
        gone.wait().expect("reap");

        assert_eq!(
            live_pane_process_group(std::process::id(), detached.id(), token),
            None,
            "a live process in another terminal session"
        );
        assert_eq!(
            live_pane_process_group(gone_pid, detached.id(), token),
            None,
            "a pane shell that is gone"
        );
        let _ = detached.kill();
        let _ = detached.wait();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_pane_process_group_follows_the_agent_process_not_its_job() {
        use std::os::unix::process::CommandExt;

        let shell_pid = std::process::id();
        let mut wrapper = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn wrapper");
        let job = wrapper.id();
        let mut agent = std::process::Command::new("sleep")
            .arg("30")
            .process_group(job as i32)
            .spawn()
            .expect("spawn agent");
        let agent_pid = agent.id();
        let token = process_start_token(agent_pid).expect("agent start token");
        let wrapper_token = process_start_token(job).expect("wrapper start token");

        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            Some(job)
        );
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token + 1),
            None,
            "a reused pid has a different start token"
        );
        unsafe {
            libc::kill(agent_pid as libc::pid_t, libc::SIGSTOP);
        }
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            Some(job)
        );

        unsafe {
            libc::kill(agent_pid as libc::pid_t, libc::SIGKILL);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while live_pane_process_group(shell_pid, agent_pid, token).is_some()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            live_pane_process_group(shell_pid, agent_pid, token),
            None,
            "an unreaped agent must not count as alive while its wrapper lives"
        );
        assert_eq!(
            live_pane_process_group(shell_pid, job, wrapper_token),
            Some(job)
        );
        agent.wait().expect("reap agent");
        assert_eq!(live_pane_process_group(shell_pid, agent_pid, token), None);
        let _ = wrapper.kill();
        let _ = wrapper.wait();
    }

    #[test]
    fn terminal_resize_signal_is_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn pane_shell_process_names_reject_exec_replacement_programs() {
        for shell in ["bash", "-zsh", "/bin/fish", "pwsh", "powershell.exe"] {
            assert!(is_pane_shell_process_name(shell), "{shell}");
        }
        for program in ["vim", "nvim", "cargo", "test-runner", "opencode"] {
            assert!(!is_pane_shell_process_name(program), "{program}");
        }
    }

    #[test]
    fn detached_custom_command_preserves_unix_login_shell_flag() {
        let cmd = detached_custom_command_process("echo hello");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-lc"),
                std::ffi::OsStr::new("echo hello")
            ]
        );
    }

    #[test]
    fn pane_custom_command_builder_preserves_unix_shell_flag() {
        let expected: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "echo hello".into()];
        assert_eq!(
            pane_custom_command_pty_builder("echo hello").get_argv(),
            &expected
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interactive_shell_command_quotes_for_posix_and_powershell() {
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "semi;colon".into(),
            "@options".into(),
        ];
        assert_eq!(
            interactive_shell_command(&argv, "bash").as_deref(),
            Some("pi '' 'two words' 'a'\\''b' '$HOME' 'semi;colon' @options")
        );
        assert_eq!(
            interactive_shell_command(&argv, "pwsh").as_deref(),
            Some("pi '' 'two words' 'a''b' '$HOME' 'semi;colon' '@options'")
        );
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }
}

#[cfg(not(unix))]
pub(crate) fn shared_ssh_control_path(
    _namespace: &std::path::Path,
    _target: &str,
) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "interactive SSH recovery requires Unix OpenSSH multiplexing",
    ))
}
