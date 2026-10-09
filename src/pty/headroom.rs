//! Refuses a new pane before the system runs out of pseudo-terminals.
//!
//! When the kernel's PTY pool is full, `openpty` fails with an opaque
//! `Device not configured` and no terminal anywhere on the machine can open a
//! new tab. Herdr checks the system count once per spawn (sampled at most once
//! a second) and refuses early with an error that names the counts, keeping a
//! reserve for the user's other terminals.

use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::platform::SystemPtyUsage;

/// API error code for a spawn refused because pseudo-terminals ran out.
pub(crate) const PTY_EXHAUSTED_CODE: &str = "pty_exhausted";

/// Free PTYs kept for terminals outside Herdr.
pub(crate) const PTY_HEADROOM: u32 = 64;

const SAMPLE_TTL: Duration = Duration::from_secs(1);

/// A spawn refused, or an `openpty` that failed, because the system's
/// pseudo-terminals are (nearly) all in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PtyExhausted {
    pub usage: Option<SystemPtyUsage>,
}

impl std::fmt::Display for PtyExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.usage {
            Some(usage) => write!(
                f,
                "{} of {} pseudo-terminals in use; close tabs or run `herdr-job clean`",
                usage.in_use, usage.max
            ),
            None => f.write_str(
                "the system has no pseudo-terminals left; close tabs or run `herdr-job clean`",
            ),
        }
    }
}

impl std::error::Error for PtyExhausted {}

impl From<PtyExhausted> for io::Error {
    fn from(err: PtyExhausted) -> Self {
        io::Error::other(err)
    }
}

/// Free PTYs a spawn must leave. Capped at a quarter of the pool so a small
/// configured limit (older macOS defaults to 127) still allows panes.
pub(crate) fn required_free(max: u32) -> u32 {
    PTY_HEADROOM.min(max / 4)
}

fn check_headroom(usage: Option<SystemPtyUsage>) -> Result<(), PtyExhausted> {
    let Some(usage) = usage else {
        return Ok(());
    };
    if usage.max.saturating_sub(usage.in_use) < required_free(usage.max) {
        return Err(PtyExhausted { usage: Some(usage) });
    }
    Ok(())
}

#[derive(Default)]
struct UsageCache {
    sampled: Option<(Instant, Option<SystemPtyUsage>)>,
}

impl UsageCache {
    fn get(
        &mut self,
        now: Instant,
        sample: impl FnOnce() -> Option<SystemPtyUsage>,
    ) -> Option<SystemPtyUsage> {
        if let Some((at, usage)) = self.sampled {
            if now.saturating_duration_since(at) < SAMPLE_TTL {
                return usage;
            }
        }
        let usage = sample();
        self.sampled = Some((now, usage));
        usage
    }

    /// Counts a PTY allocated since the sample, so a burst of spawns inside
    /// one sample window cannot overshoot the reserve.
    fn note_allocated(&mut self) {
        if let Some((_, Some(usage))) = &mut self.sampled {
            usage.in_use = usage.in_use.saturating_add(1);
        }
    }

    /// The last sample, with the PTYs allocated since counted in.
    fn current(&self) -> Option<SystemPtyUsage> {
        self.sampled.and_then(|(_, usage)| usage)
    }

    #[cfg(unix)]
    fn invalidate(&mut self) {
        self.sampled = None;
    }

    fn ensure_headroom(
        &mut self,
        now: Instant,
        sample: impl FnOnce() -> Option<SystemPtyUsage>,
    ) -> Result<(), PtyExhausted> {
        check_headroom(self.get(now, sample))?;
        self.note_allocated();
        Ok(())
    }
}

static CACHE: Mutex<UsageCache> = Mutex::new(UsageCache { sampled: None });

fn with_cache<T>(f: impl FnOnce(&mut UsageCache) -> T) -> T {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut cache)
}

/// The system's PTY usage, sampled at most once a second.
pub(crate) fn system_usage() -> Option<SystemPtyUsage> {
    let usage = with_cache(|cache| cache.get(Instant::now(), crate::platform::system_pty_usage));
    super::usage::record_sample(usage);
    usage
}

/// Called once before a pane's PTY is opened.
pub(crate) fn ensure_spawn_headroom() -> io::Result<()> {
    let (result, usage) = with_cache(|cache| {
        let result = cache.ensure_headroom(Instant::now(), crate::platform::system_pty_usage);
        (result, cache.current())
    });
    match result {
        Ok(()) => {
            super::usage::record_sample(usage);
            Ok(())
        }
        Err(err) => {
            super::usage::record_exhausted(err.usage);
            Err(err.into())
        }
    }
}

/// Maps a failed `openpty` to [`PtyExhausted`] when the OS reports the pool
/// is full, with a fresh count; any other failure keeps its message.
#[cfg(unix)]
pub(crate) fn openpty_error(err: impl std::fmt::Display) -> io::Error {
    let err = openpty_error_with(&err.to_string(), || {
        with_cache(|cache| {
            cache.invalidate();
            cache.get(Instant::now(), crate::platform::system_pty_usage)
        })
    });
    if let Some(exhausted) = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<PtyExhausted>())
    {
        super::usage::record_exhausted(exhausted.usage);
    }
    err
}

#[cfg(unix)]
fn openpty_error_with(message: &str, sample: impl FnOnce() -> Option<SystemPtyUsage>) -> io::Error {
    if openpty_errno(message).is_some_and(is_pool_exhausted_errno) {
        PtyExhausted { usage: sample() }.into()
    } else {
        io::Error::other(message.to_string())
    }
}

/// portable-pty reports `openpty` failures as text that embeds the
/// `io::Error` debug form, `... Os { code: 6, kind: ..., message: ... }`.
#[cfg(unix)]
fn openpty_errno(message: &str) -> Option<i32> {
    let rest = &message[message.find("Os { code: ")? + "Os { code: ".len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// macOS fails a full `/dev/ptmx` with ENXIO ("Device not configured");
/// Linux devpts fails past `kernel.pty.max` with ENOSPC.
#[cfg(unix)]
fn is_pool_exhausted_errno(errno: i32) -> bool {
    errno == libc::ENXIO || errno == libc::ENOSPC
}

/// Whether a spawn error came from PTY exhaustion.
pub(crate) fn is_pty_exhausted(err: &io::Error) -> bool {
    err.get_ref()
        .is_some_and(|inner| inner.is::<PtyExhausted>())
}

/// The API error code for a failed pane spawn: [`PTY_EXHAUSTED_CODE`] when
/// pseudo-terminals ran out, `fallback` otherwise.
pub(crate) fn spawn_error_code<'a>(err: &io::Error, fallback: &'a str) -> &'a str {
    if is_pty_exhausted(err) {
        PTY_EXHAUSTED_CODE
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(in_use: u32, max: u32) -> Option<SystemPtyUsage> {
        Some(SystemPtyUsage {
            in_use,
            max,
            exact: false,
        })
    }

    #[test]
    fn spawn_below_headroom_is_refused_with_counts() {
        let mut cache = UsageCache::default();
        let err = cache
            .ensure_headroom(Instant::now(), || usage(448, 511))
            .expect_err("63 free is below the reserve");
        let err = io::Error::from(err);

        assert!(is_pty_exhausted(&err));
        assert_eq!(spawn_error_code(&err, "tab_create_failed"), "pty_exhausted");
        assert_eq!(
            err.to_string(),
            "448 of 511 pseudo-terminals in use; close tabs or run `herdr-job clean`"
        );
    }

    #[test]
    fn spawn_above_headroom_is_allowed() {
        let mut cache = UsageCache::default();
        assert!(cache
            .ensure_headroom(Instant::now(), || usage(447, 511))
            .is_ok());
    }

    #[test]
    fn unknown_usage_allows_spawn() {
        let mut cache = UsageCache::default();
        assert!(cache.ensure_headroom(Instant::now(), || None).is_ok());
    }

    #[test]
    fn small_pool_keeps_a_quarter_free() {
        assert_eq!(required_free(127), 31);
        assert!(check_headroom(usage(96, 127)).is_ok());
        assert!(check_headroom(usage(97, 127)).is_err());
    }

    #[test]
    fn burst_inside_one_sample_counts_its_own_spawns() {
        let mut cache = UsageCache::default();
        let now = Instant::now();
        let mut samples = 0;
        let mut allowed = 0;
        for _ in 0..10 {
            if cache
                .ensure_headroom(now, || {
                    samples += 1;
                    usage(440, 511)
                })
                .is_ok()
            {
                allowed += 1;
            }
        }
        assert_eq!(samples, 1, "one sample per second, not per spawn");
        assert_eq!(allowed, 8, "spawns stop once the reserve is reached");
    }

    #[test]
    fn sample_expires_after_ttl() {
        let mut cache = UsageCache::default();
        let start = Instant::now();
        assert_eq!(cache.get(start, || usage(1, 511)), usage(1, 511));
        assert_eq!(cache.get(start, || usage(2, 511)), usage(1, 511));
        assert_eq!(
            cache.get(start + SAMPLE_TTL, || usage(3, 511)),
            usage(3, 511)
        );
    }

    #[cfg(unix)]
    #[test]
    fn openpty_pool_exhaustion_is_mapped() {
        let message = format!(
            "failed to openpty: {:?}",
            io::Error::from_raw_os_error(libc::ENXIO)
        );
        let err = openpty_error_with(&message, || usage(511, 511));

        assert!(is_pty_exhausted(&err));
        assert_eq!(
            err.to_string(),
            "511 of 511 pseudo-terminals in use; close tabs or run `herdr-job clean`"
        );

        let message = format!(
            "failed to openpty: {:?}",
            io::Error::from_raw_os_error(libc::ENOSPC)
        );
        assert!(is_pty_exhausted(&openpty_error_with(&message, || None)));
    }

    #[cfg(unix)]
    #[test]
    fn other_openpty_failures_keep_their_message() {
        let message = format!(
            "failed to openpty: {:?}",
            io::Error::from_raw_os_error(libc::EMFILE)
        );
        let err = openpty_error_with(&message, || usage(511, 511));

        assert!(!is_pty_exhausted(&err));
        assert_eq!(
            spawn_error_code(&err, "pane_split_failed"),
            "pane_split_failed"
        );
        assert_eq!(err.to_string(), message);
    }
}
