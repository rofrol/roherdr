//! Pseudo-terminal usage the server shows its clients and warns about.
//!
//! The server owns the numbers. Herdr's own count is its live pane runtimes,
//! read when a client snapshot is built, so it follows pane create and close.
//! The system figure is the last sample: every pane spawn takes one (see
//! [`super::headroom`]), and a background thread takes one on a timer,
//! because neither macOS nor Linux reports PTY allocations as an event.
//! A new sample wakes the server, which rebuilds the snapshots and raises the
//! threshold notifications through [`PtyAlerts`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[cfg(unix)]
use serde::{Deserialize, Serialize};

use crate::platform::SystemPtyUsage;

/// The footer turns amber at this share of the system pool.
pub(crate) const AMBER_PERCENT: u32 = 70;
/// The footer turns red at this share of the system pool.
pub(crate) const RED_PERCENT: u32 = 90;
/// One notification each, highest first. Each re-arms only once usage drops
/// below the next lower threshold ([`AMBER_PERCENT`] for the lowest), so a
/// count hovering around a threshold does not repeat it.
const ALERT_PERCENTS: [u32; 2] = [90, 80];

/// The kernel sends no event when its PTY pool changes, so the server
/// samples it on a timer, faster once the pool is at least half used.
/// Spawns sample in between.
const SAMPLE_INTERVAL_BUSY: Duration = Duration::from_secs(10); // delay: external polling (the kernel's PTY count has no change event)
const SAMPLE_INTERVAL_CALM: Duration = Duration::from_secs(15); // delay: external polling (the kernel's PTY count has no change event)
const BUSY_PERCENT: u32 = 50;

/// What the server last learned of the system's PTYs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PtyUsageState {
    pub usage: Option<SystemPtyUsage>,
    /// How many times a spawn found the pool exhausted, by refusal or by a
    /// failed `openpty`.
    pub exhausted: u64,
}

static STATE: Mutex<PtyUsageState> = Mutex::new(PtyUsageState {
    usage: None,
    exhausted: 0,
});
/// Bumped on every change of [`STATE`], so the server loop can skip the lock.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static NOTIFIER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
static SAMPLER_STARTED: AtomicBool = AtomicBool::new(false);

fn update(change: impl FnOnce(&mut PtyUsageState) -> bool) {
    let changed = {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        change(&mut state)
    };
    if changed {
        GENERATION.fetch_add(1, Ordering::AcqRel);
        if let Some(notify) = NOTIFIER.get() {
            notify();
        }
    }
}

/// Stores a system sample; wakes the server when it differs from the last.
pub(crate) fn record_sample(usage: Option<SystemPtyUsage>) {
    update(|state| {
        let changed = state.usage != usage;
        state.usage = usage;
        changed
    });
}

/// Records a spawn that found the pool exhausted, which always warns.
pub(crate) fn record_exhausted(usage: Option<SystemPtyUsage>) {
    update(|state| {
        if usage.is_some() {
            state.usage = usage;
        }
        state.exhausted = state.exhausted.saturating_add(1);
        true
    });
}

/// The last system sample. Takes one uncontended lock: cheap enough for a
/// snapshot build, never a filesystem read.
pub(crate) fn latest() -> Option<SystemPtyUsage> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .usage
}

/// The state when it changed since `seen`, which it then advances.
pub(crate) fn take_change(seen: &mut u64) -> Option<PtyUsageState> {
    let generation = GENERATION.load(Ordering::Acquire);
    if generation == *seen {
        return None;
    }
    *seen = generation;
    Some(
        *STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

/// Starts the background sampler once per process; `notify` wakes the
/// server loop after a change.
pub(crate) fn start_sampler(notify: impl Fn() + Send + Sync + 'static) {
    let _ = NOTIFIER.set(Box::new(notify));
    if SAMPLER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = crate::thread_spawn::spawn_named("herdr-pty-usage", || loop {
        let usage = super::headroom::system_usage();
        // delay: external polling, see SAMPLE_INTERVAL_BUSY.
        std::thread::sleep(sample_interval(usage));
    });
    if let Err(err) = spawned {
        SAMPLER_STARTED.store(false, Ordering::Release);
        tracing::warn!(err = %err, "failed to start the PTY usage sampler");
    }
}

fn sample_interval(usage: Option<SystemPtyUsage>) -> Duration {
    if usage
        .and_then(percent)
        .is_some_and(|percent| percent >= BUSY_PERCENT)
    {
        SAMPLE_INTERVAL_BUSY
    } else {
        SAMPLE_INTERVAL_CALM
    }
}

/// Share of the pool in use, rounded down; `None` for an empty pool.
pub(crate) fn percent(usage: SystemPtyUsage) -> Option<u32> {
    (usage.max > 0).then(|| {
        u32::try_from(u64::from(usage.in_use) * 100 / u64::from(usage.max)).unwrap_or(u32::MAX)
    })
}

/// A notification the server sends about PTY usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtyAlert {
    /// Usage crossed a threshold (80 or 90 percent of the pool).
    Threshold { percent: u32, usage: SystemPtyUsage },
    /// A spawn found the pool exhausted.
    Exhausted { usage: Option<SystemPtyUsage> },
}

impl PtyAlert {
    /// Title and body of the notification; `herdr` is herdr's own count.
    pub(crate) fn text(&self, herdr: u32) -> (String, String) {
        match *self {
            Self::Threshold { percent, usage } => (
                format!("Pseudo-terminals {percent}% used"),
                format!(
                    "{} of {} in use on the system, {herdr} by herdr; close tabs or run `herdr-job clean`",
                    usage.in_use, usage.max
                ),
            ),
            Self::Exhausted { usage } => (
                "Out of pseudo-terminals".to_owned(),
                super::headroom::PtyExhausted { usage }.to_string(),
            ),
        }
    }
}

/// Which thresholds have been announced; the server keeps one.
#[derive(Debug, Default)]
pub(crate) struct PtyAlerts {
    /// The highest threshold announced and not yet re-armed.
    announced: Option<u32>,
    exhausted_seen: u64,
    /// Set after a handoff from a server that did not carry its announced
    /// thresholds: the first sample adopts its level without notifying.
    #[cfg(unix)]
    adopt_current_level: bool,
}

/// The announced thresholds a live handoff carries, so the new server goes
/// on with the same hysteresis instead of announcing the current level again
/// after every install. The exhaustion count stays behind: it counts the old
/// process's spawns. Live handoff exists only on Unix.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PtyAlertsHandoff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced: Option<u32>,
}

impl PtyAlerts {
    /// What a live handoff carries to the next server.
    #[cfg(unix)]
    pub(crate) fn handoff(&self) -> PtyAlertsHandoff {
        PtyAlertsHandoff {
            announced: self.announced,
        }
    }

    /// The state a server imported by a live handoff starts with; `None`
    /// when the old server did not send it (a build before this field).
    #[cfg(unix)]
    pub(crate) fn from_handoff(handoff: Option<PtyAlertsHandoff>) -> Self {
        match handoff {
            Some(handoff) => Self {
                announced: handoff
                    .announced
                    .filter(|level| ALERT_PERCENTS.contains(level)),
                ..Self::default()
            },
            None => Self {
                adopt_current_level: true,
                ..Self::default()
            },
        }
    }

    /// The notifications `state` calls for: an exhaustion each time one is
    /// recorded, a threshold once per crossing.
    pub(crate) fn observe(&mut self, state: PtyUsageState) -> Vec<PtyAlert> {
        let mut alerts = Vec::new();
        if state.exhausted > self.exhausted_seen {
            self.exhausted_seen = state.exhausted;
            alerts.push(PtyAlert::Exhausted { usage: state.usage });
        }
        let Some((usage, percent)) = state.usage.and_then(|usage| Some((usage, percent(usage)?)))
        else {
            return alerts;
        };
        // Re-arm what usage has dropped clearly below.
        while let Some(level) = self.announced {
            if percent >= rearm_below(level) {
                break;
            }
            self.announced = ALERT_PERCENTS.into_iter().find(|lower| *lower < level);
        }
        let reached = ALERT_PERCENTS
            .into_iter()
            .find(|threshold| percent >= *threshold);
        #[cfg(unix)]
        if std::mem::take(&mut self.adopt_current_level) {
            self.announced = self.announced.max(reached);
            return alerts;
        }
        if let Some(threshold) =
            reached.filter(|threshold| self.announced.is_none_or(|level| level < *threshold))
        {
            self.announced = Some(threshold);
            // An exhaustion notice already says it; do not stack a second one.
            if alerts.is_empty() {
                alerts.push(PtyAlert::Threshold { percent, usage });
            }
        }
        alerts
    }
}

/// An announced threshold re-arms below the next lower one.
fn rearm_below(level: u32) -> u32 {
    ALERT_PERCENTS
        .into_iter()
        .find(|lower| *lower < level)
        .unwrap_or(AMBER_PERCENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(in_use: u32) -> PtyUsageState {
        PtyUsageState {
            usage: Some(SystemPtyUsage {
                in_use,
                max: 100,
                exact: true,
            }),
            exhausted: 0,
        }
    }

    fn percents(alerts: Vec<PtyAlert>) -> Vec<u32> {
        alerts
            .into_iter()
            .map(|alert| match alert {
                PtyAlert::Threshold { percent, .. } => percent,
                PtyAlert::Exhausted { .. } => 0,
            })
            .collect()
    }

    #[test]
    fn each_threshold_notifies_once_per_crossing() {
        let mut alerts = PtyAlerts::default();
        assert!(alerts.observe(state(79)).is_empty());
        assert_eq!(percents(alerts.observe(state(80))), [80]);
        assert!(alerts.observe(state(85)).is_empty());
        assert_eq!(percents(alerts.observe(state(91))), [91]);
        assert!(alerts.observe(state(95)).is_empty());
    }

    #[test]
    fn a_jump_past_both_thresholds_notifies_once() {
        let mut alerts = PtyAlerts::default();
        assert_eq!(percents(alerts.observe(state(93))), [93]);
        assert!(alerts.observe(state(85)).is_empty());
    }

    #[test]
    fn hovering_around_a_threshold_does_not_repeat_it() {
        let mut alerts = PtyAlerts::default();
        assert_eq!(percents(alerts.observe(state(90))), [90]);
        // Below 90 but not below 80: 90 stays announced.
        assert!(alerts.observe(state(85)).is_empty());
        assert!(alerts.observe(state(90)).is_empty());
        // Below 80 re-arms 90, not 80.
        assert!(alerts.observe(state(79)).is_empty());
        assert!(alerts.observe(state(82)).is_empty());
        assert_eq!(percents(alerts.observe(state(90))), [90]);
        // 80 re-arms only below 70.
        assert!(alerts.observe(state(72)).is_empty());
        assert!(alerts.observe(state(80)).is_empty());
        assert!(alerts.observe(state(69)).is_empty());
        assert_eq!(percents(alerts.observe(state(80))), [80]);
    }

    #[test]
    fn exhaustion_always_warns() {
        let mut alerts = PtyAlerts::default();
        let mut exhausted = state(100);
        exhausted.exhausted = 1;
        assert_eq!(
            alerts.observe(exhausted),
            [PtyAlert::Exhausted {
                usage: exhausted.usage
            }]
        );
        assert!(alerts.observe(exhausted).is_empty(), "seen once");
        exhausted.exhausted = 2;
        assert_eq!(
            alerts.observe(exhausted).len(),
            1,
            "a second failure warns again"
        );
        // The threshold stays announced; no extra notice for it.
        assert!(alerts.observe(state(100)).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_handoff_above_the_threshold_sends_no_new_notification() {
        let mut old = PtyAlerts::default();
        assert_eq!(percents(old.observe(state(85))), [85]);

        let mut new = PtyAlerts::from_handoff(Some(old.handoff()));
        assert!(new.observe(state(85)).is_empty());
        assert!(new.observe(state(88)).is_empty());
    }

    #[test]
    fn a_cold_start_above_the_threshold_notifies_once() {
        let mut alerts = PtyAlerts::default();
        assert_eq!(percents(alerts.observe(state(85))), [85]);
        assert!(alerts.observe(state(85)).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_hysteresis_continues_across_a_handoff() {
        let mut old = PtyAlerts::default();
        assert_eq!(percents(old.observe(state(92))), [92]);
        // 90 is announced: 85 does not re-arm it, so 90 again stays quiet.
        let mut new = PtyAlerts::from_handoff(Some(old.handoff()));
        assert!(new.observe(state(85)).is_empty());
        assert!(new.observe(state(91)).is_empty());
        // Below 80 re-arms 90, as without the handoff.
        assert!(new.observe(state(79)).is_empty());
        assert_eq!(percents(new.observe(state(90))), [90]);

        // Nothing announced before the handoff: crossing 80 after it notifies.
        let mut quiet = PtyAlerts::from_handoff(Some(PtyAlerts::default().handoff()));
        assert!(quiet.observe(state(60)).is_empty());
        assert_eq!(percents(quiet.observe(state(80))), [80]);
    }

    #[cfg(unix)]
    #[test]
    fn a_handoff_from_an_older_server_adopts_the_current_level() {
        let mut alerts = PtyAlerts::from_handoff(None);
        // An unknown sample does not use up the adoption.
        assert!(alerts.observe(PtyUsageState::default()).is_empty());
        assert!(alerts.observe(state(85)).is_empty());
        assert_eq!(percents(alerts.observe(state(91))), [91]);
    }

    #[cfg(unix)]
    #[test]
    fn a_handoff_does_not_silence_an_exhaustion() {
        let mut old = PtyAlerts::default();
        assert_eq!(percents(old.observe(state(95))), [95]);
        let mut new = PtyAlerts::from_handoff(Some(old.handoff()));
        let mut exhausted = state(100);
        exhausted.exhausted = 1;
        assert_eq!(new.observe(exhausted).len(), 1);
    }

    #[test]
    fn unknown_usage_raises_nothing() {
        let mut alerts = PtyAlerts::default();
        assert!(alerts.observe(PtyUsageState::default()).is_empty());
        let empty_pool = PtyUsageState {
            usage: Some(SystemPtyUsage {
                in_use: 0,
                max: 0,
                exact: false,
            }),
            exhausted: 0,
        };
        assert!(alerts.observe(empty_pool).is_empty());
    }

    #[test]
    fn sampling_slows_down_while_the_pool_is_calm() {
        let usage = |in_use| {
            Some(SystemPtyUsage {
                in_use,
                max: 100,
                exact: false,
            })
        };
        assert_eq!(sample_interval(None), SAMPLE_INTERVAL_CALM);
        assert_eq!(sample_interval(usage(49)), SAMPLE_INTERVAL_CALM);
        assert_eq!(sample_interval(usage(50)), SAMPLE_INTERVAL_BUSY);
    }

    #[test]
    fn notification_text_names_both_counts() {
        let alert = PtyAlert::Threshold {
            percent: 82,
            usage: SystemPtyUsage {
                in_use: 419,
                max: 511,
                exact: false,
            },
        };
        assert_eq!(
            alert.text(65),
            (
                "Pseudo-terminals 82% used".to_owned(),
                "419 of 511 in use on the system, 65 by herdr; close tabs or run `herdr-job clean`"
                    .to_owned()
            )
        );
    }
}
