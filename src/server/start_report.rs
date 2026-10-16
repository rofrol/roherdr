//! A server daemon's start report to the client that spawned it: one line
//! on a pipe the client passed (`platform::attach_server_start_report`),
//! `ready` once the server listens, or `refused <pid>` when another server
//! holds the session's lock (`session_lock`). A server that exits before
//! either closes the pipe with nothing written. The client blocks on that
//! line or the pipe's end, both written by the server itself: it never
//! looks at the lock, and no clock decides.

use std::io::{self, BufRead as _, Write as _};
use std::path::Path;

use tracing::warn;

const READY: &str = "ready";
const REFUSED: &str = "refused";

/// What a spawned server reported about its start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    /// It listens on its sockets.
    Ready,
    /// Another server holds the session's lock; `holder` is the pid that
    /// server recorded, if any.
    Refused { holder: Option<u32> },
}

/// The server's end of the report; a server started without one (by hand,
/// by a service manager, or as a handoff import) reports nothing.
pub(crate) struct StartReporter {
    pipe: Option<std::fs::File>,
}

impl StartReporter {
    /// Takes the report pipe this process's starter passed, if any. Call
    /// before the server starts any child.
    pub(crate) fn from_starter() -> Self {
        Self {
            pipe: crate::platform::take_server_start_report(),
        }
    }

    /// Reports that the server listens, and closes the pipe.
    pub(crate) fn ready(&mut self) {
        self.send(READY);
    }

    /// Reports that `holder` holds the session's lock, and closes the pipe.
    pub(crate) fn refused(&mut self, holder: Option<u32>) {
        match holder {
            Some(pid) => self.send(&format!("{REFUSED} {pid}")),
            None => self.send(REFUSED),
        }
    }

    fn send(&mut self, line: &str) {
        let Some(mut pipe) = self.pipe.take() else {
            return;
        };
        // A starter that is gone (its user pressed Ctrl-C) closed its end;
        // the server goes on regardless.
        if let Err(err) = pipe.write_all(format!("{line}\n").as_bytes()) {
            warn!(err = %err, "could not report the server's start to its starter");
        }
    }
}

/// Reads a server's start report: `None` when the server closed the pipe
/// (it exited) without reporting.
pub(crate) fn read_outcome(reader: impl io::Read) -> io::Result<Option<StartOutcome>> {
    let mut line = String::new();
    io::BufReader::new(reader).read_line(&mut line)?;
    let line = line.trim_end_matches('\n');
    if line.is_empty() {
        return Ok(None);
    }
    let mut words = line.split(' ');
    let outcome = match (words.next(), words.next(), words.next()) {
        (Some(READY), None, None) => StartOutcome::Ready,
        (Some(REFUSED), None, None) => StartOutcome::Refused { holder: None },
        (Some(REFUSED), Some(pid), None) => StartOutcome::Refused {
            holder: pid.parse().ok(),
        },
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the herdr server sent an unknown start report: {line:?}"),
            ))
        }
    };
    Ok(Some(outcome))
}

/// Blocks until the server that writes `reader` reported its start, and
/// turns a refusal or an exit without a report into the error the user
/// sees; `client_socket` is where it would listen.
pub(crate) fn await_server_start(reader: impl io::Read, client_socket: &Path) -> io::Result<()> {
    match read_outcome(reader)? {
        Some(StartOutcome::Ready) => Ok(()),
        Some(StartOutcome::Refused { holder }) => Err(
            crate::server::session_lock::held_by_another_error(holder, client_socket),
        ),
        None => Err(io::Error::other(format!(
            "the herdr server exited before it was ready (socket: {}); check {}",
            client_socket.display(),
            crate::session::data_dir()
                .join("herdr-server.log")
                .display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_report_reads_back_as_its_outcome() {
        let report = |write: &dyn Fn(&mut StartReporter)| {
            let (reader, writer) = std::io::pipe().expect("pipe");
            let mut reporter = StartReporter {
                pipe: Some(std::fs::File::from(std::os::fd::OwnedFd::from(writer))),
            };
            write(&mut reporter);
            // A second report is not sent: the first closed the pipe.
            reporter.ready();
            drop(reporter);
            read_outcome(reader).expect("read")
        };
        assert_eq!(report(&|r| r.ready()), Some(StartOutcome::Ready));
        assert_eq!(
            report(&|r| r.refused(Some(42))),
            Some(StartOutcome::Refused { holder: Some(42) })
        );
        assert_eq!(
            report(&|r| r.refused(None)),
            Some(StartOutcome::Refused { holder: None })
        );
        assert_eq!(read_outcome(&b""[..]).expect("read"), None);
        assert!(read_outcome(&b"started\n"[..]).is_err());
    }

    #[test]
    fn a_refusal_says_who_holds_the_session_and_how_to_go_on() {
        let socket = Path::new("/state/herdr-client.sock");
        assert!(await_server_start(&b"ready\n"[..], socket).is_ok());

        let err = await_server_start(&b"refused 4242\n"[..], socket).expect_err("refused");
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        let message = err.to_string();
        assert!(message.contains("a herdr server (pid 4242)"), "{message}");
        assert!(message.contains("starting or handing off"), "{message}");
        assert!(message.contains("/state/herdr-client.sock"), "{message}");

        let err = await_server_start(&b""[..], socket).expect_err("exited");
        assert!(err.to_string().contains("exited before it was ready"));
    }
}
