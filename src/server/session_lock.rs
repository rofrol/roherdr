//! One server per session: the running server holds an exclusive lock on a
//! file beside the session's API socket (`herdr.sock` → `herdr-server.lock`)
//! for its whole life, and a live handoff passes that lock to the new
//! server with the other descriptors. A server start takes it before any
//! other startup effect and refuses while another holds it. A client that
//! finds no server answering spawns one as before and learns from that
//! server's start report (`start_report`) whether it started or refused.
//!
//! The lock alone decides, and only a server start takes it: nothing probes
//! it, since even a shared lock taken for an instant could make a starting
//! server refuse with none left running. A small record beside it
//! (`herdr-server.pid`), written by the holder after it took the lock, only
//! names that holder in messages, and may be stale.

use std::io;
use std::path::{Path, PathBuf};

use crate::platform::ExclusiveFileLock;

/// The lock a running server holds for its session.
pub(crate) struct SessionLock {
    // Held for its drop; read only to pass it on in a live handoff (Unix).
    #[cfg_attr(windows, allow(dead_code))]
    lock: ExclusiveFileLock,
    path: PathBuf,
}

/// The session lock file for the API socket at `api_socket`. A named session
/// and a socket override each get their own, beside their socket.
pub(crate) fn lock_path_for(api_socket: &Path) -> PathBuf {
    let stem = api_socket
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("herdr");
    let parent = api_socket.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}-server.lock"))
}

/// The lock file of the active session.
pub(crate) fn active_lock_path() -> PathBuf {
    lock_path_for(&crate::api::socket_path())
}

fn record_path(lock_path: &Path) -> PathBuf {
    lock_path.with_extension("pid")
}

impl SessionLock {
    /// Takes the session lock at `path`, or refuses (`ResourceBusy`) naming
    /// the holder while another process holds it.
    pub(crate) fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = ExclusiveFileLock::open(path)?;
        #[cfg(test)]
        tests::between_open_and_lock(path);
        match file.try_lock()? {
            Some(lock) => {
                let lock = Self {
                    lock,
                    path: path.to_path_buf(),
                };
                lock.write_record();
                Ok(lock)
            }
            None => Err(held_error(path)),
        }
    }

    /// Adopts the session lock a live handoff's source passed: the
    /// descriptor must be the file at `path`, this server's own lock file.
    #[cfg(unix)]
    pub(crate) fn adopt_passed(fd: std::os::fd::OwnedFd, path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;

        let lock = ExclusiveFileLock::from_passed(fd)?;
        let passed = lock.file().metadata()?;
        let own = std::fs::metadata(path)?;
        if (passed.dev(), passed.ino()) != (own.dev(), own.ino()) {
            return Err(io::Error::other(format!(
                "the handoff passed a lock that is not {}; refusing the handoff",
                path.display()
            )));
        }
        Ok(Self {
            lock,
            path: path.to_path_buf(),
        })
    }

    #[cfg_attr(windows, allow(dead_code))] // Offered in a live handoff (Unix).
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The locked descriptor, which a live handoff passes on.
    #[cfg(unix)]
    pub(crate) fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(self.lock.file())
    }

    /// Names this process as the holder in the record beside the lock.
    /// Messages only: a failure is logged, the lock still decides.
    pub(crate) fn write_record(&self) {
        let record = record_path(&self.path);
        let staging = record.with_extension(format!("pid.{}", std::process::id()));
        let written = std::fs::write(&staging, format!("{}\n", std::process::id()))
            .and_then(|()| crate::platform::replace_file(&staging, &record));
        if let Err(err) = written {
            let _ = std::fs::remove_file(&staging);
            tracing::warn!(err = %err, path = %record.display(), "could not record the session lock's holder");
        }
    }
}

/// The process the record beside `lock_path` names, if any.
pub(crate) fn recorded_holder(lock_path: &Path) -> Option<u32> {
    std::fs::read_to_string(record_path(lock_path))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn describe_holder(holder: Option<u32>) -> String {
    match holder {
        Some(pid) => format!("a herdr server (pid {pid})"),
        None => "a herdr server".to_owned(),
    }
}

fn held_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::ResourceBusy,
        format!(
            "herdr server is already running: {} holds this session's lock ({})",
            describe_holder(recorded_holder(path)),
            path.display()
        ),
    )
}

/// Why a server start for the active session was refused: `holder` (the
/// pid it recorded) holds the session's lock, yet no server answers on
/// `client_socket`, so it is starting or handing off, or exiting after a
/// stop (which closes the sockets just before the process ends). Says how
/// to go on; nothing waits.
pub(crate) fn held_by_another_error(holder: Option<u32>, client_socket: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::ResourceBusy,
        format!(
            "{} holds this session but does not answer on {} yet: it is starting or handing \
             off (or still exiting after a stop), so no second server is started. Run `{}` again once it is ready; if it stays \
             unresponsive, check {} or stop it with `{}`.",
            describe_holder(holder),
            client_socket.display(),
            crate::session::local_attach_command(),
            crate::session::data_dir()
                .join("herdr-server.log")
                .display(),
            crate::session::local_stop_command(),
        ),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn test_dir(name: &str) -> PathBuf {
        // Short: socket paths beside it are limited to about 100 bytes.
        let dir = std::env::temp_dir().join(format!("hl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    type Hook = Box<dyn FnOnce() + Send>;

    static BETWEEN_OPEN_AND_LOCK: std::sync::Mutex<Vec<(PathBuf, Hook)>> =
        std::sync::Mutex::new(Vec::new());

    /// Runs `hook` in the next [`SessionLock::acquire`] of `path`, after it
    /// opened the lock file and before it takes the lock.
    #[cfg(unix)]
    pub(crate) fn pause_between_open_and_lock(path: &Path, hook: impl FnOnce() + Send + 'static) {
        BETWEEN_OPEN_AND_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((path.to_path_buf(), Box::new(hook)));
    }

    pub(super) fn between_open_and_lock(path: &Path) {
        let hook = {
            let mut hooks = BETWEEN_OPEN_AND_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            hooks
                .iter()
                .position(|(hooked, _)| hooked == path)
                .map(|index| hooks.remove(index).1)
        };
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Whether the lock at `path` is free, by taking it: only for tests in
    /// which nothing else contends for the file.
    #[cfg(unix)]
    fn free(path: &Path) -> bool {
        ExclusiveFileLock::try_acquire(path)
            .expect("lock file")
            .is_some()
    }

    #[test]
    fn the_lock_file_sits_beside_the_api_socket() {
        assert_eq!(
            lock_path_for(Path::new("/state/sessions/work/herdr.sock")),
            PathBuf::from("/state/sessions/work/herdr-server.lock")
        );
        assert_eq!(
            lock_path_for(Path::new("/tmp/x/test.sock")),
            PathBuf::from("/tmp/x/test-server.lock")
        );
    }

    #[test]
    fn a_second_acquire_is_refused_naming_the_holder_until_the_first_lets_go() {
        let dir = test_dir("second");
        let path = dir.join("herdr-server.lock");
        let first = SessionLock::acquire(&path).expect("the first takes the lock");
        assert_eq!(recorded_holder(&path), Some(std::process::id()));

        let err = SessionLock::acquire(&path)
            .err()
            .expect("the second is refused");
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        let message = err.to_string();
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "{message}"
        );

        drop(first);
        let _again = SessionLock::acquire(&path).expect("free again");
        // The file was never replaced or removed.
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_record_does_not_hold_the_session() {
        let dir = test_dir("stale");
        let path = dir.join("herdr-server.lock");
        std::fs::write(record_path(&path), "4242\n").expect("record");
        std::fs::write(&path, "").expect("lock file");
        let lock = SessionLock::acquire(&path).expect("the record does not hold it");
        assert_eq!(recorded_holder(lock.path()), Some(std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_refusal_names_the_holder_and_says_how_to_go_on() {
        let client_socket = Path::new("/state/herdr-client.sock");
        let err = held_by_another_error(Some(4242), client_socket);
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        let message = err.to_string();
        assert!(message.contains("starting or handing off"), "{message}");
        assert!(message.contains("pid 4242"), "{message}");
        assert!(message.contains("/state/herdr-client.sock"), "{message}");
        let message = held_by_another_error(None, client_socket).to_string();
        assert!(message.starts_with("a herdr server holds"), "{message}");
    }

    const SERVER_START_ENV: &str = "HERDR_TEST_SERVER_START";

    /// Runs `herdr server`'s entry point when started by
    /// `a_second_server_start_is_refused_before_any_startup_effect`, with
    /// that test's socket and config directories.
    #[test]
    fn server_start_process_entry() {
        if std::env::var_os(SERVER_START_ENV).is_none() {
            return;
        }
        let result = crate::server::headless::run_server();
        // Reached only when the start was not refused.
        eprintln!("run_server returned: {result:?}");
        std::process::exit(3);
    }

    #[cfg(unix)]
    #[test]
    fn a_second_server_start_is_refused_before_any_startup_effect() {
        let dir = test_dir("start");
        let api_socket = dir.join("herdr.sock");
        let client_socket =
            crate::server::socket_paths::derive_client_socket_from_api_socket(&api_socket);
        let config_home = dir.join("config");
        let held = SessionLock::acquire(&lock_path_for(&api_socket)).expect("the first server");

        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        // As a client's auto-start passes it.
        let report = crate::platform::attach_server_start_report(&mut command)
            .expect("start report pipe")
            .expect("Unix passes one");
        let output = command
            .args([
                "--exact",
                &format!("{module}::server_start_process_entry"),
                "--nocapture",
            ])
            .env(SERVER_START_ENV, "1")
            .env(crate::api::SOCKET_PATH_ENV_VAR, &api_socket)
            .env("XDG_CONFIG_HOME", &config_home)
            .env_remove(crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR)
            .env_remove(crate::session::SESSION_ENV_VAR)
            .env_remove(crate::server::autodetect::STARTUP_CWD_ENV_VAR)
            .stdin(std::process::Stdio::null())
            .output();
        let report = report.spawned();
        let output = output.expect("start the second server");

        // The client that spawned it learns the refusal and its holder from
        // the server's report, not from the lock.
        assert_eq!(
            crate::server::start_report::read_outcome(report).expect("report"),
            Some(crate::server::start_report::StartOutcome::Refused {
                holder: Some(std::process::id())
            })
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains("herdr server is already running"),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!("pid {}", std::process::id())),
            "{stderr}"
        );
        // Nothing was bound, restored or written for the session.
        assert!(!api_socket.exists());
        assert!(!client_socket.exists());
        let session_files: Vec<_> = std::fs::read_dir(config_home.join("herdr"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| !name.ends_with(".log"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(session_files.is_empty(), "{session_files:?}");
        // The lock is still the first server's.
        assert_eq!(recorded_holder(held.path()), Some(std::process::id()));
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_passed_lock_must_be_the_sessions_own_file() {
        let dir = test_dir("adopt");
        let path = dir.join("herdr-server.lock");
        let other = dir.join("other-server.lock");
        let lock = SessionLock::acquire(&path).expect("lock");
        let dup = || std::os::fd::BorrowedFd::try_clone_to_owned(&lock.as_fd()).expect("dup");

        let adopted = SessionLock::adopt_passed(dup(), &path).expect("the session's own lock");
        std::fs::write(&other, "").expect("other file");
        assert!(SessionLock::adopt_passed(dup(), &other).is_err());

        // Both copies are one lock: it stays held until the last one closes.
        drop(lock);
        assert!(!free(&path));
        drop(adopted);
        assert!(free(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
