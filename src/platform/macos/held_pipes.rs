//! A process's children's exits, and the processes outside its tree that
//! hold the write end of a pipe it reads: what keeps a tool call's output
//! open after the tool's own process exited.
//!
//! The exits come from a kqueue: `NOTE_FORK` on the watched process says it
//! started a child, which then gets a `NOTE_EXIT` of its own. The pipes come
//! from `proc_pidfdinfo(PROC_PIDFDPIPEINFO)`: each end of a pipe has its own
//! kernel handle and names the other end's as its peer, so a process holds
//! the write end of a pipe the watched process reads when one of its pipe
//! descriptors has the handle that the reader's descriptor names as peer.

use std::collections::{HashMap, HashSet};

use super::{all_pids, comm_from_bsdinfo, process_argv, process_bsdinfo};
use crate::platform::PipeWriter;

/// `PROC_PIDFDPIPEINFO` from `<sys/proc_info.h>`; libc does not define it.
const PROC_PIDFDPIPEINFO: libc::c_int = 6;
/// `FREAD` and `FWRITE`, the open flags of a descriptor (`fi_openflags`).
const FREAD: u32 = 0x1;
const FWRITE: u32 = 0x2;

/// `struct proc_fileinfo`.
#[repr(C)]
struct ProcFileinfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

/// `struct pipe_info`.
#[repr(C)]
struct PipeInfo {
    pipe_stat: libc::vinfo_stat,
    pipe_handle: u64,
    pipe_peerhandle: u64,
    pipe_status: i32,
    rfu_1: i32,
}

/// `struct pipe_fdinfo`.
#[repr(C)]
struct PipeFdinfo {
    pfi: ProcFileinfo,
    pipeinfo: PipeInfo,
}

/// One pipe descriptor of a process: its open flags, its end's handle and
/// the other end's (0 once that end is closed everywhere).
struct PipeEnd {
    flags: u32,
    handle: u64,
    peer: u64,
}

/// The descriptors `pid` has open, or none when it cannot be read (gone,
/// another user's).
fn descriptors(pid: u32) -> Vec<libc::proc_fdinfo> {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    let needed = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            std::ptr::null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return Vec::new();
    }
    // Room for descriptors opened between the two calls.
    let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(needed as usize / entry + 32);
    let size = (fds.capacity() * entry) as libc::c_int;
    let filled = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    if filled <= 0 {
        return Vec::new();
    }
    // SAFETY: the kernel filled `filled` bytes of whole entries, at most
    // the capacity it was given.
    unsafe { fds.set_len((filled as usize / entry).min(fds.capacity())) };
    fds
}

/// The pipe descriptors of `pid`, by descriptor number.
fn pipe_ends(pid: u32) -> Vec<(i32, PipeEnd)> {
    descriptors(pid)
        .into_iter()
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_PIPE as u32)
        .filter_map(|fd| {
            let mut info: PipeFdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<PipeFdinfo>() as libc::c_int;
            let read = unsafe {
                libc::proc_pidfdinfo(
                    pid as libc::c_int,
                    fd.proc_fd,
                    PROC_PIDFDPIPEINFO,
                    &mut info as *mut _ as *mut libc::c_void,
                    size,
                )
            };
            (read == size).then_some((
                fd.proc_fd,
                PipeEnd {
                    flags: info.pfi.fi_openflags,
                    handle: info.pipeinfo.pipe_handle,
                    peer: info.pipeinfo.pipe_peerhandle,
                },
            ))
        })
        .collect()
}

/// `root_pid` and every live process below it.
fn tree_of(root_pid: u32, parents: &[(u32, u32)]) -> HashSet<u32> {
    let mut tree = HashSet::from([root_pid]);
    let mut frontier = vec![root_pid];
    while let Some(parent) = frontier.pop() {
        for &(pid, _) in parents
            .iter()
            .filter(|(pid, ppid)| *ppid == parent && *pid != parent)
        {
            if tree.insert(pid) {
                frontier.push(pid);
            }
        }
    }
    tree
}

/// The processes outside `root_pid`'s tree that hold the write end of a
/// pipe `root_pid` reads on a descriptor above 2 (its own stdin, stdout and
/// stderr are its parent's), each once, by pid.
pub(crate) fn outside_pipe_writers(root_pid: u32) -> Vec<PipeWriter> {
    let wanted: HashSet<u64> = pipe_ends(root_pid)
        .into_iter()
        .filter(|(fd, end)| {
            *fd > 2 && end.flags & FREAD != 0 && end.flags & FWRITE == 0 && end.peer != 0
        })
        .map(|(_, end)| end.peer)
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let infos: HashMap<u32, libc::proc_bsdinfo> = all_pids()
        .into_iter()
        .filter_map(|pid| process_bsdinfo(pid).map(|info| (pid, info)))
        .collect();
    let parents: Vec<(u32, u32)> = infos
        .iter()
        .map(|(pid, info)| (*pid, info.pbi_ppid))
        .collect();
    let tree = tree_of(root_pid, &parents);
    let mut writers: Vec<PipeWriter> = infos
        .iter()
        .filter(|(pid, _)| !tree.contains(pid))
        .filter(|(pid, _)| {
            pipe_ends(**pid)
                .iter()
                .any(|(_, end)| wanted.contains(&end.handle))
        })
        .map(|(pid, info)| PipeWriter {
            pid: *pid,
            command: process_argv(*pid)
                .filter(|argv| !argv.is_empty())
                .map(|argv| argv.join(" "))
                .or_else(|| comm_from_bsdinfo(info))
                .unwrap_or_default(),
        })
        .collect();
    writers.sort_by_key(|writer| writer.pid);
    writers
}

/// Adds an `EVFILT_PROC` watch for `fflags` on `pid`.
fn watch(kq: libc::c_int, pid: u32, fflags: u32) -> std::io::Result<()> {
    let change = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD,
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

fn children_of(root_pid: u32) -> Vec<u32> {
    all_pids()
        .into_iter()
        .filter(|pid| process_bsdinfo(*pid).is_some_and(|info| info.pbi_ppid == root_pid))
        .collect()
}

/// Watches the children of `root_pid` it does not watch yet; reports at
/// once one that is gone before its watch was added.
fn adopt_children(
    kq: libc::c_int,
    root_pid: u32,
    watched: &mut HashSet<u32>,
    on_exit: &mut dyn FnMut(u32),
) {
    for child in children_of(root_pid) {
        if watched.contains(&child) {
            continue;
        }
        match watch(kq, child, libc::NOTE_EXIT) {
            Ok(()) => {
                watched.insert(child);
            }
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => on_exit(child),
            Err(_) => {}
        }
    }
}

/// Calls `on_exit` with the pid of each child of `root_pid` that exits,
/// on a thread of its own, until `root_pid` exits. A child is watched from
/// the `NOTE_FORK` that started it; one gone by the time the children are
/// listed but not reaped yet is reported at once, and one already reaped
/// then is not seen.
pub(crate) fn watch_child_exits(
    root_pid: u32,
    mut on_exit: Box<dyn FnMut(u32) + Send>,
) -> std::io::Result<()> {
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let close = |error: std::io::Error| {
        unsafe { libc::close(kq) };
        error
    };
    watch(kq, root_pid, libc::NOTE_FORK | libc::NOTE_EXIT).map_err(close)?;
    let mut watched = HashSet::new();
    adopt_children(kq, root_pid, &mut watched, &mut *on_exit);
    crate::thread_spawn::spawn_named("herdr-child-exits", move || {
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
                break;
            }
            let mut root_exited = false;
            for event in &events[..count as usize] {
                let pid = event.ident as u32;
                if pid == root_pid {
                    if event.fflags & libc::NOTE_FORK != 0 {
                        adopt_children(kq, root_pid, &mut watched, &mut *on_exit);
                    }
                    root_exited |= event.fflags & libc::NOTE_EXIT != 0;
                } else if event.fflags & libc::NOTE_EXIT != 0 && watched.remove(&pid) {
                    on_exit(pid);
                }
            }
            if root_exited {
                break;
            }
        }
        unsafe { libc::close(kq) };
    })
    .map(drop)
    .map_err(close)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    /// delay: hang guard, fails a broken test instead of hanging the suite;
    /// never decides an outcome a passing test depends on.
    const HANG_GUARD: Duration = Duration::from_secs(60);

    /// A shell that waits for a line on its stdin, runs `tool` in a command
    /// substitution (a pipe the shell reads) and prints what it read, with
    /// a watch on its children's exits that sends each exit's outside
    /// writers.
    fn watched_shell(script: &str) -> (std::process::Child, mpsc::Receiver<Vec<PipeWriter>>) {
        let mut shell = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let root = shell.id();
        let (sender, exits) = mpsc::channel();
        watch_child_exits(
            root,
            Box::new(move |_child| {
                let _ = sender.send(outside_pipe_writers(root));
            }),
        )
        .unwrap();
        writeln!(shell.stdin.as_mut().unwrap(), "go").unwrap();
        (shell, exits)
    }

    /// The tool's detached child keeps the tool's stderr open after the
    /// tool exited: the watch sees the tool's exit, and the child is a
    /// writer outside the shell's tree.
    #[test]
    fn a_detached_child_holding_a_tool_s_stderr_is_found_at_the_tool_s_exit() {
        let (mut shell, exits) = watched_shell(
            "read go; \
             out=$(/bin/sh -c '(exec tail -f /dev/null </dev/null >/dev/null &); echo tool' 2>&1); \
             echo \"$out\"",
        );
        let mut found = Vec::new();
        while let Ok(writers) = exits.recv_timeout(HANG_GUARD) {
            if !writers.is_empty() {
                found = writers;
                break;
            }
        }
        for writer in &found {
            unsafe { libc::kill(writer.pid as libc::pid_t, libc::SIGKILL) };
        }
        let _ = shell.wait();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].command.contains("tail -f /dev/null"), "{found:?}");
    }

    /// The same through `posix_spawn`, as a native CLI starts its tools:
    /// the spawn raises `NOTE_FORK` too.
    #[test]
    fn a_posix_spawned_tool_s_exit_is_seen() {
        let mut root = Command::new("python3")
            .arg("-c")
            .arg(
                "import os, sys\n\
                 sys.stdin.readline()\n\
                 r, w = os.pipe()\n\
                 actions = [(os.POSIX_SPAWN_DUP2, w, 1), (os.POSIX_SPAWN_DUP2, w, 2)]\n\
                 os.posix_spawn('/bin/sh', ['sh', '-c', \
                 '(exec tail -f /dev/null </dev/null >/dev/null &); echo tool'], \
                 os.environ, file_actions=actions)\n\
                 os.close(w)\n\
                 while os.read(r, 4096):\n    pass\n",
            )
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = root.id();
        let (sender, exits) = mpsc::channel();
        watch_child_exits(
            pid,
            Box::new(move |_child| {
                let _ = sender.send(outside_pipe_writers(pid));
            }),
        )
        .unwrap();
        writeln!(root.stdin.as_mut().unwrap(), "go").unwrap();
        let writers = exits.recv_timeout(HANG_GUARD).unwrap_or_default();
        for writer in &writers {
            unsafe { libc::kill(writer.pid as libc::pid_t, libc::SIGKILL) };
        }
        let _ = root.kill();
        let _ = root.wait();
        assert_eq!(writers.len(), 1, "{writers:?}");
        assert!(
            writers[0].command.contains("tail -f /dev/null"),
            "{writers:?}"
        );
    }

    #[test]
    fn no_writer_once_the_tool_s_output_is_closed() {
        let (mut shell, exits) = watched_shell("read go; out=$(/bin/echo tool); echo \"$out\"");
        let writers = exits.recv_timeout(HANG_GUARD).unwrap();
        let _ = shell.wait();
        assert!(writers.is_empty(), "{writers:?}");
    }
}
