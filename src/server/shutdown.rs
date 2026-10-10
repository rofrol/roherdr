//! Why the server is stopping, recorded where the request arrives and logged
//! once when shutdown begins.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::platform::ServerQuitSignal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShutdownReason {
    Signal(ServerQuitSignal),
    ApiStop { caller: Option<String> },
    HostShutdown,
    Unknown,
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signal(signal) => write!(f, "{signal}"),
            Self::ApiStop {
                caller: Some(caller),
            } => write!(f, "server.stop request from {caller}"),
            Self::ApiStop { caller: None } => f.write_str("server.stop request"),
            Self::HostShutdown => f.write_str("host shutdown"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// The server's stop flag plus the reason recorded by the first stop request.
#[derive(Clone, Default)]
pub(crate) struct ServerStop {
    flag: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    /// Woken by every request, so the event loop runs its quit check.
    wake: Arc<tokio::sync::Notify>,
}

impl ServerStop {
    pub(crate) fn flag(&self) -> &Arc<AtomicBool> {
        &self.flag
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub(crate) fn request(&self, reason: ShutdownReason) {
        if let Ok(mut recorded) = self.reason.lock() {
            recorded.get_or_insert(reason);
        }
        self.flag.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    /// Returns once a stop was requested since the last return, including
    /// one requested before the first call.
    pub(crate) async fn requested(&self) {
        self.wake.notified().await;
    }

    pub(crate) fn take_reason(&self) -> Option<ShutdownReason> {
        self.reason.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stop_reason_wins() {
        let stop = ServerStop::default();
        assert!(!stop.is_requested());

        stop.request(ShutdownReason::HostShutdown);
        stop.request(ShutdownReason::ApiStop { caller: None });

        assert!(stop.is_requested());
        assert_eq!(stop.take_reason(), Some(ShutdownReason::HostShutdown));
        assert_eq!(stop.take_reason(), None);
    }

    #[test]
    fn a_request_before_the_wait_still_wakes_it() {
        // The signal handlers route to the stop before the event loop
        // waits on it: an early request must not be lost.
        let stop = ServerStop::default();
        stop.request(ShutdownReason::HostShutdown);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        rt.block_on(stop.requested());
        assert!(stop.is_requested());
    }

    #[test]
    fn api_stop_reason_names_the_caller() {
        let reason = ShutdownReason::ApiStop {
            caller: Some("pid 42 (herdr), parent pid 7 (zsh)".into()),
        };
        assert_eq!(
            reason.to_string(),
            "server.stop request from pid 42 (herdr), parent pid 7 (zsh)"
        );
    }
}
