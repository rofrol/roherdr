//! Watches files and directories for changes with a kqueue (`EVFILT_VNODE`).
//! A directory reports a change when an entry is added, removed or renamed;
//! a file when it is written, extended, its attributes change, or it is
//! deleted or renamed. A watch on a deleted or renamed file ends: watching
//! its path again opens whatever is there now.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The `EVFILT_USER` event that tells the thread to stop.
const STOP_IDENT: usize = 0;

const VNODE_EVENTS: u32 = libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_DELETE
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;

/// Open watches by file descriptor.
type Watches = Arc<Mutex<HashMap<libc::c_int, PathBuf>>>;

pub(crate) struct PathWatcher {
    kq: libc::c_int,
    watches: Watches,
}

impl PathWatcher {
    /// Starts the watch thread; `on_change` gets each watched path that
    /// changed, on that thread.
    pub(crate) fn start(on_change: Box<dyn Fn(PathBuf) + Send>) -> std::io::Result<Self> {
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let close = |error: std::io::Error| {
            unsafe { libc::close(kq) };
            error
        };
        register(
            kq,
            STOP_IDENT,
            libc::EVFILT_USER,
            libc::EV_ADD | libc::EV_CLEAR,
            0,
        )
        .map_err(close)?;
        let watches: Watches = Arc::default();
        let thread_watches = Arc::clone(&watches);
        crate::thread_spawn::spawn_named("herdr-path-watch", move || {
            run(kq, &thread_watches, &*on_change);
            let mut watches = thread_watches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for fd in watches.keys() {
                unsafe { libc::close(*fd) };
            }
            watches.clear();
            unsafe { libc::close(kq) };
        })
        .map(drop)
        .map_err(close)?;
        Ok(Self { kq, watches })
    }

    /// Watches `path` unless it is watched already.
    pub(crate) fn watch(&self, path: &Path) -> std::io::Result<()> {
        let mut watches = self
            .watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if watches.values().any(|watched| watched == path) {
            return Ok(());
        }
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if let Err(error) = register(
            self.kq,
            fd as usize,
            libc::EVFILT_VNODE,
            libc::EV_ADD | libc::EV_CLEAR,
            VNODE_EVENTS,
        ) {
            unsafe { libc::close(fd) };
            return Err(error);
        }
        watches.insert(fd, path.to_path_buf());
        Ok(())
    }
}

impl Drop for PathWatcher {
    fn drop(&mut self) {
        // The thread closes the queue and the watched files when it stops.
        let _ = register(
            self.kq,
            STOP_IDENT,
            libc::EVFILT_USER,
            0,
            libc::NOTE_TRIGGER,
        );
    }
}

fn register(
    kq: libc::c_int,
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
) -> std::io::Result<()> {
    let change = libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let added = unsafe { libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if added < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn run(kq: libc::c_int, watches: &Watches, on_change: &dyn Fn(PathBuf)) {
    let mut events: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
    loop {
        let count = unsafe {
            libc::kevent(
                kq,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                std::ptr::null(),
            )
        };
        if count < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        for event in &events[..count as usize] {
            if event.filter == libc::EVFILT_USER {
                return;
            }
            let fd = event.ident as libc::c_int;
            let gone =
                event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE) != 0;
            let path = {
                let mut watches = watches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if gone {
                    let path = watches.remove(&fd);
                    if path.is_some() {
                        unsafe { libc::close(fd) };
                    }
                    path
                } else {
                    watches.get(&fd).cloned()
                }
            };
            if let Some(path) = path {
                on_change(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// delay: hang guard, fails a broken test instead of hanging the suite;
    /// never decides an outcome a passing test depends on.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    fn watcher() -> (PathWatcher, mpsc::Receiver<PathBuf>) {
        let (sender, changes) = mpsc::channel();
        let watcher = PathWatcher::start(Box::new(move |path| {
            let _ = sender.send(path);
        }))
        .unwrap();
        (watcher, changes)
    }

    #[test]
    fn a_new_entry_reports_its_directory() {
        let dir = tempfile_dir("dir");
        let (watcher, changes) = watcher();
        watcher.watch(&dir).unwrap();
        std::fs::write(dir.join("claude.new"), b"2").unwrap();
        std::fs::rename(dir.join("claude.new"), dir.join("claude")).unwrap();
        assert_eq!(changes.recv_timeout(HANG_GUARD).unwrap(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replaced_file_is_watched_again_by_path() {
        let dir = tempfile_dir("file");
        let file = dir.join("pi");
        std::fs::write(&file, b"1").unwrap();
        let (watcher, changes) = watcher();
        watcher.watch(&file).unwrap();
        std::fs::write(dir.join("pi.new"), b"2").unwrap();
        std::fs::rename(dir.join("pi.new"), &file).unwrap();
        assert_eq!(changes.recv_timeout(HANG_GUARD).unwrap(), file);
        // The old file's watch ended with the rename; the path is watched
        // again, and an in-place write to the new file reports it.
        watcher.watch(&file).unwrap();
        std::fs::write(&file, b"3").unwrap();
        assert_eq!(changes.recv_timeout(HANG_GUARD).unwrap(), file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-path-watch-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // kqueue reports the canonical directory the path names.
        std::fs::canonicalize(&dir).unwrap()
    }
}
