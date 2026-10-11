//! Watches files and directories for changes with inotify. A directory
//! reports a change when an entry is created, written, removed or moved; a
//! file when it is written, its attributes change, or it is deleted or
//! moved. A watch on a deleted or moved file ends: watching its path again
//! adds whatever is there now.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const WATCH_EVENTS: u32 = libc::IN_CREATE
    | libc::IN_CLOSE_WRITE
    | libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF;

/// Watched paths by watch descriptor.
type Watches = Arc<Mutex<HashMap<libc::c_int, PathBuf>>>;

pub(crate) struct PathWatcher {
    inotify: libc::c_int,
    /// Written once to stop the thread.
    stop: libc::c_int,
    watches: Watches,
}

impl PathWatcher {
    /// Starts the watch thread; `on_change` gets each watched path that
    /// changed, on that thread.
    pub(crate) fn start(on_change: Box<dyn Fn(PathBuf) + Send>) -> std::io::Result<Self> {
        let inotify = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if inotify < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stop = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if stop < 0 {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(inotify) };
            return Err(error);
        }
        let watches: Watches = Arc::default();
        let thread_watches = Arc::clone(&watches);
        let spawned = crate::thread_spawn::spawn_named("herdr-path-watch", move || {
            run(inotify, stop, &thread_watches, &*on_change);
            unsafe {
                libc::close(inotify);
                libc::close(stop);
            }
        });
        if let Err(error) = spawned {
            unsafe {
                libc::close(inotify);
                libc::close(stop);
            }
            return Err(error);
        }
        Ok(Self {
            inotify,
            stop,
            watches,
        })
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
        let wd = unsafe { libc::inotify_add_watch(self.inotify, c_path.as_ptr(), WATCH_EVENTS) };
        if wd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        watches.insert(wd, path.to_path_buf());
        Ok(())
    }
}

impl Drop for PathWatcher {
    fn drop(&mut self) {
        // The thread closes both descriptors when it stops.
        let one: u64 = 1;
        unsafe {
            libc::write(
                self.stop,
                (&one as *const u64).cast(),
                std::mem::size_of::<u64>(),
            )
        };
    }
}

fn run(inotify: libc::c_int, stop: libc::c_int, watches: &Watches, on_change: &dyn Fn(PathBuf)) {
    let mut buffer = [0u8; 4096];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: inotify,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            return;
        }
        if fds[0].revents & libc::POLLIN == 0 {
            if fds[0].revents != 0 {
                return;
            }
            continue;
        }
        let read = unsafe { libc::read(inotify, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read < 0 {
            let error = std::io::Error::last_os_error().raw_os_error();
            if matches!(error, Some(libc::EINTR) | Some(libc::EAGAIN)) {
                continue;
            }
            return;
        }
        let mut offset = 0usize;
        let header = std::mem::size_of::<libc::inotify_event>();
        while offset + header <= read as usize {
            // The buffer holds whole events; copy the header out, since its
            // alignment in the byte buffer is not guaranteed.
            let event: libc::inotify_event =
                unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast()) };
            offset += header + event.len as usize;
            let path = {
                let mut watches = watches
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if event.mask & libc::IN_IGNORED != 0 {
                    watches.remove(&event.wd);
                    continue;
                }
                let path = watches.get(&event.wd).cloned();
                if event.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                    watches.remove(&event.wd);
                    unsafe { libc::inotify_rm_watch(inotify, event.wd) };
                }
                path
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

    #[test]
    fn a_new_entry_reports_its_directory() {
        let dir = std::env::temp_dir().join(format!("herdr-path-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (sender, changes) = mpsc::channel();
        let watcher = PathWatcher::start(Box::new(move |path| {
            let _ = sender.send(path);
        }))
        .unwrap();
        watcher.watch(&dir).unwrap();
        std::fs::write(dir.join("claude"), b"2").unwrap();
        assert_eq!(changes.recv_timeout(HANG_GUARD).unwrap(), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
