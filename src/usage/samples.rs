//! Claude's weekly utilization over time, for the per-day use under the AN row.
//!
//! [`super::cache::UsageCache`] keeps only the latest observation per
//! provider, so every completed Claude poll also appends its weekly window
//! here: a bounded log shared by every herdr instance, written atomically
//! the way the cache is. The series builder cuts the current window into
//! local calendar days and lays them out Monday through Sunday.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::schema::{ProviderUsage, UsageDailyUse};

const SAMPLES_FILE: &str = "usage-samples.json";
const DAY_SECS: u64 = 86_400;
const WEEK_SECS: u64 = 7 * DAY_SECS;
/// About three weekly windows of history.
const RETENTION_SECS: u64 = 3 * WEEK_SECS;
/// Hard cap on the log, whatever the poll interval: three weeks of polls
/// every five minutes (the default interval) fit with room to spare.
const MAX_SAMPLES: usize = 8_192;
/// `resets_at` stamps this close together name the same window: the
/// provider reports sub-second times, which are truncated to whole seconds.
const SAME_WINDOW_SLACK_SECS: u64 = 60;
/// How many of the latest samples an append checks for an exact repeat.
const DUPLICATE_LOOKBACK: usize = 16;
/// The windows whose samples are recorded.
pub(super) const RECORDED_WINDOWS: [&str; 1] = ["weekly"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct UsageSample {
    /// The cache's key for the login, the provider id (`claude`).
    pub(super) account: String,
    /// Window id such as `weekly`.
    pub(super) window: String,
    pub(super) used_percent: u8,
    pub(super) resets_at: u64,
    pub(super) observed_at: u64,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct SampleLog {
    #[serde(default)]
    samples: Vec<UsageSample>,
}

impl SampleLog {
    pub(super) fn load() -> Self {
        Self::load_from(&path())
    }

    fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub(super) fn samples(&self) -> &[UsageSample] {
        &self.samples
    }

    /// Reload, append the observation's recorded windows, trim and store,
    /// so samples from other instances survive.
    pub(super) fn record(usage: &ProviderUsage, observed_at: u64) {
        let path = path();
        let mut log = Self::load_from(&path);
        if log.append(usage, observed_at) {
            log.store(&path);
        }
    }

    /// Whether anything was added.
    fn append(&mut self, usage: &ProviderUsage, observed_at: u64) -> bool {
        let mut added = false;
        for window in usage
            .windows
            .iter()
            .filter(|window| RECORDED_WINDOWS.contains(&window.id.as_str()))
        {
            let Some(resets_at) = window.resets_at else {
                continue;
            };
            let sample = UsageSample {
                account: usage.provider.clone(),
                window: window.id.clone(),
                used_percent: window.used_percent,
                resets_at,
                observed_at,
            };
            // The same observation recorded twice sits in the recent tail.
            let repeated = self
                .samples
                .iter()
                .rev()
                .take(DUPLICATE_LOOKBACK)
                .any(|recorded| *recorded == sample);
            if !repeated {
                self.samples.push(sample);
                added = true;
            }
        }
        if added {
            self.trim();
        }
        added
    }

    /// Drop samples older than the retention behind the newest one, then the
    /// oldest beyond the cap.
    fn trim(&mut self) {
        let Some(newest) = self.samples.iter().map(|sample| sample.observed_at).max() else {
            return;
        };
        let keep_after = newest.saturating_sub(RETENTION_SECS);
        self.samples
            .retain(|sample| sample.observed_at >= keep_after);
        if self.samples.len() > MAX_SAMPLES {
            self.samples.sort_by_key(|sample| sample.observed_at);
            let excess = self.samples.len() - MAX_SAMPLES;
            self.samples.drain(..excess);
        }
    }

    fn store(&self, path: &std::path::Path) {
        let Ok(raw) = serde_json::to_string(self) else {
            return;
        };
        let temp = path.with_extension(format!("json.{}", std::process::id()));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temp, raw))
            .and_then(|()| std::fs::rename(&temp, path));
        if let Err(error) = written {
            tracing::debug!(%error, "failed to store usage samples");
            let _ = std::fs::remove_file(&temp);
        }
    }
}

/// Next to the shared usage cache.
fn path() -> PathBuf {
    crate::config::state_dir()
        .with_file_name("herdr")
        .join(SAMPLES_FILE)
}

/// The current window's use per local weekday at `now`, Monday first.
///
/// The current window is the seven days ending at the latest `resets_at`
/// among the account's samples of `window`; samples of an earlier window are
/// never diffed against it. Each calendar day the window touches, in the
/// local time `local_offset_at` gives for that instant, goes to its weekday's
/// position; the reset's weekday gets both the window's first and its last
/// hours unless the reset falls at local midnight. A day's use is its last
/// sample minus the day before's last sample, or 0% for the day holding the
/// window's start. When the day before has no sample, the growth across that
/// gap belongs to no day: the day shows only its own first-to-last growth,
/// marked partial from its first sample. Without a sample in the day it is
/// unknown, never 0.
/// `None` when the window has no sample, or has reset and its successor has
/// not been observed yet.
pub(super) fn daily_use(
    samples: &[UsageSample],
    account: &str,
    window: &str,
    now: u64,
    local_offset_at: impl Fn(i64) -> i64,
) -> Option<UsageDailyUse> {
    let matching = || {
        samples.iter().filter(move |sample| {
            sample.account == account && sample.window == window && sample.observed_at <= now
        })
    };
    let resets_at = matching().map(|sample| sample.resets_at).max()?;
    if resets_at <= now {
        return None;
    }
    let starts_at = resets_at.saturating_sub(WEEK_SECS);
    let mut current = matching()
        .filter(|sample| {
            sample.resets_at.saturating_add(SAME_WINDOW_SLACK_SECS) >= resets_at
                && (starts_at..resets_at).contains(&sample.observed_at)
        })
        .collect::<Vec<_>>();
    // Stable: of duplicates at one instant, the later recorded one wins.
    current.sort_by_key(|sample| sample.observed_at);
    let latest = current.last()?;
    let clock = LocalClock(&local_offset_at);
    let mut days = [DayUse::NotYet; 7];
    let mut day_starts = [None; 7];
    // The last sample of the day before, when that day has one.
    let mut previous_last: Option<&UsageSample> = None;
    // A week of local days, one more when the reset is not at midnight.
    for day in clock.day(starts_at)..=clock.day(resets_at - 1) {
        let midnight = clock.midnight(day);
        let start = midnight.max(starts_at);
        let end = clock.midnight(day + 1).min(resets_at);
        if start >= end {
            continue;
        }
        let weekday = weekday_position(day);
        day_starts[weekday].get_or_insert(midnight);
        if start > now {
            continue;
        }
        let mut in_day = current
            .iter()
            .copied()
            .filter(|sample| (start..end).contains(&sample.observed_at));
        let first = in_day.next();
        let last = in_day.next_back().or(first);
        let used = first.zip(last).map(|(first, last)| {
            let last_percent = i16::from(last.used_percent);
            if start == starts_at {
                // The window starts at 0%.
                Part::Known(last_percent)
            } else if let Some(previous) = previous_last {
                Part::Known(last_percent - i16::from(previous.used_percent))
            } else {
                // The use during the gap before belongs to no day.
                Part::Partial {
                    used: last_percent - i16::from(first.used_percent),
                    from: first.observed_at,
                }
            }
        });
        days[weekday] = days[weekday].add(used);
        previous_last = last;
    }
    let partial_from = days
        .iter()
        .map(|day| day.partial_from())
        .collect::<Vec<_>>();
    let elapsed = now.saturating_sub(starts_at).min(WEEK_SECS);
    let even_pace = 100.0 * elapsed as f64 / WEEK_SECS as f64;
    Some(UsageDailyUse {
        window: window.to_owned(),
        starts_at,
        resets_at,
        days: days.into_iter().map(DayUse::value).collect(),
        partial_from,
        day_starts: day_starts
            .into_iter()
            .map(|start| start.unwrap_or(starts_at))
            .collect(),
        current_day: (starts_at..resets_at)
            .contains(&now)
            .then(|| weekday_position(clock.day(now)) as u8),
        pace_percent: (f64::from(latest.used_percent) - even_pace).round() as i32,
    })
}

/// The use within one local day's part of the window.
#[derive(Debug, Clone, Copy)]
enum Part {
    /// Counted from the day before's last sample, or from 0% at the reset.
    Known(i16),
    /// The day before has no sample: only the growth from the day's own
    /// first sample, at `from`.
    Partial { used: i16, from: u64 },
}

/// A weekday's use, summed over the window's parts on that weekday.
#[derive(Debug, Clone, Copy)]
enum DayUse {
    NotYet,
    Known {
        used: i16,
        /// The first partial part's first sample.
        partial_from: Option<u64>,
    },
    Unknown,
}

impl DayUse {
    fn add(self, part: Option<Part>) -> Self {
        let Some(part) = part else {
            return Self::Unknown;
        };
        let (used, from) = match part {
            Part::Known(used) => (used, None),
            Part::Partial { used, from } => (used, Some(from)),
        };
        match self {
            Self::Unknown => Self::Unknown,
            Self::NotYet => Self::Known {
                used,
                partial_from: from,
            },
            Self::Known {
                used: sum,
                partial_from,
            } => Self::Known {
                used: sum.saturating_add(used),
                partial_from: partial_from.or(from),
            },
        }
    }

    fn value(self) -> Option<i16> {
        match self {
            Self::Known { used, .. } => Some(used),
            Self::NotYet | Self::Unknown => None,
        }
    }

    fn partial_from(&self) -> Option<u64> {
        match self {
            Self::Known { partial_from, .. } => *partial_from,
            Self::NotYet | Self::Unknown => None,
        }
    }
}

/// Local calendar days, numbered from 1970-01-01 in local time.
struct LocalClock<'a, F: Fn(i64) -> i64>(&'a F);

impl<F: Fn(i64) -> i64> LocalClock<'_, F> {
    fn day(&self, at: u64) -> i64 {
        let at = i64::try_from(at).unwrap_or(i64::MAX);
        at.saturating_add((self.0)(at)).div_euclid(DAY_SECS as i64)
    }

    /// Unix seconds of the day's local midnight. The offset is read at the
    /// guess first, then at the result, so a daylight saving change on
    /// either side of it does not move it.
    fn midnight(&self, day: i64) -> u64 {
        let local = day.saturating_mul(DAY_SECS as i64);
        let guess = local.saturating_sub((self.0)(local));
        u64::try_from(local.saturating_sub((self.0)(guess))).unwrap_or(0)
    }
}

/// Monday is 0: 1970-01-01 was a Thursday.
fn weekday_position(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::UsageWindow;

    /// 2026-10-19 00:00 UTC, a Monday.
    const MONDAY: u64 = 1_792_368_000;
    /// The window from that Monday's midnight to the next one, in UTC.
    const RESET: u64 = MONDAY + WEEK_SECS;
    const START: u64 = RESET - WEEK_SECS;
    const HOUR: u64 = 3_600;

    fn sample(used_percent: u8, resets_at: u64, observed_at: u64) -> UsageSample {
        UsageSample {
            account: "claude".into(),
            window: "weekly".into(),
            used_percent,
            resets_at,
            observed_at,
        }
    }

    fn utc(_at: i64) -> i64 {
        0
    }

    fn series(samples: &[UsageSample], now: u64) -> UsageDailyUse {
        daily_use(samples, "claude", "weekly", now, utc).unwrap()
    }

    fn days(samples: &[UsageSample], now: u64) -> Vec<Option<i16>> {
        series(samples, now).days
    }

    fn midnights(first: u64) -> Vec<u64> {
        (0..7).map(|day| first + day * DAY_SECS).collect()
    }

    #[test]
    fn a_reset_at_local_midnight_fills_each_weekday_once_from_zero() {
        let samples = [
            // Measured from 0% at the reset, not unknown.
            sample(2, RESET, START + 3 * HOUR),
            sample(9, RESET, START + DAY_SECS + HOUR),
            sample(12, RESET, START + 2 * DAY_SECS - 1),
            sample(20, RESET, START + 2 * DAY_SECS),
            sample(26, RESET, RESET - 3 * HOUR),
            sample(30, RESET, RESET - HOUR),
        ];
        let series = series(&samples, RESET - 30 * 60);
        assert_eq!((series.starts_at, series.resets_at), (START, RESET));
        // Saturday has no sample: Sunday shows only its own growth.
        assert_eq!(
            series.days,
            vec![Some(2), Some(10), Some(8), None, None, None, Some(4)]
        );
        assert_eq!(
            series.partial_from,
            vec![None, None, None, None, None, None, Some(RESET - 3 * HOUR)]
        );
        assert_eq!(series.day_starts, midnights(MONDAY));
        assert_eq!(series.current_day, Some(6));
        // 30% used against 100 × (167.5h / 168h) = 99.70%.
        assert_eq!(series.pace_percent, -70);
    }

    #[test]
    fn a_monday_reset_at_any_time_puts_its_first_and_last_hours_on_monday() {
        let resets_at = RESET + 10 * HOUR;
        let mut samples = vec![
            sample(5, resets_at, MONDAY + 20 * HOUR),
            sample(9, resets_at, MONDAY + DAY_SECS + 5 * HOUR),
            // Nothing on Wednesday.
            sample(12, resets_at, MONDAY + 3 * DAY_SECS + HOUR),
            sample(15, resets_at, MONDAY + 3 * DAY_SECS + 2 * HOUR),
        ];
        let thursday = series(&samples, MONDAY + 3 * DAY_SECS + 3 * HOUR);
        assert_eq!(thursday.starts_at, MONDAY + 10 * HOUR);
        // Thursday follows a day without samples: only its own growth.
        assert_eq!(
            thursday.days,
            vec![Some(5), Some(4), None, Some(3), None, None, None]
        );
        assert_eq!(thursday.partial_from[3], Some(MONDAY + 3 * DAY_SECS + HOUR));
        assert_eq!(thursday.current_day, Some(3));
        // Monday's midnight is the window's first Monday, before its start.
        assert_eq!(thursday.day_starts, midnights(MONDAY));

        samples.push(sample(30, resets_at, MONDAY + 4 * DAY_SECS + HOUR));
        samples.push(sample(40, resets_at, MONDAY + 6 * DAY_SECS + 20 * HOUR));
        // The next Monday has begun, without a sample yet: unknown.
        let next_monday = RESET + HOUR;
        assert_eq!(days(&samples, next_monday)[0], None);
        assert_eq!(series(&samples, next_monday).current_day, Some(0));

        samples.push(sample(45, resets_at, RESET + 9 * HOUR));
        // The next Monday counts from Sunday's last sample; Sunday follows
        // a Saturday without samples.
        let series = series(&samples, RESET + 9 * HOUR + 30 * 60);
        assert_eq!(
            series.days,
            vec![Some(10), Some(4), None, Some(3), Some(15), None, Some(0)]
        );
        assert_eq!(
            series.partial_from,
            vec![
                None,
                None,
                None,
                Some(MONDAY + 3 * DAY_SECS + HOUR),
                None,
                None,
                Some(MONDAY + 6 * DAY_SECS + 20 * HOUR),
            ]
        );
    }

    #[test]
    fn a_reset_on_another_weekday_keeps_monday_first() {
        // Thursday 15:00.
        let starts_at = MONDAY + 3 * DAY_SECS + 15 * HOUR;
        let resets_at = starts_at + WEEK_SECS;
        let samples = [
            sample(4, resets_at, starts_at + 2 * HOUR),
            sample(6, resets_at, MONDAY + 4 * DAY_SECS + HOUR),
            sample(9, resets_at, RESET + HOUR),
            sample(10, resets_at, RESET + 2 * HOUR),
        ];
        let series = series(&samples, RESET + 3 * HOUR);
        // Thursday counts from 0% at the reset; Monday follows a Sunday
        // without samples.
        assert_eq!(
            series.days,
            vec![Some(1), None, None, Some(4), Some(2), None, None]
        );
        assert_eq!(series.partial_from[0], Some(RESET + HOUR));
        assert_eq!(series.partial_from[3], None);
        assert_eq!(series.current_day, Some(0));
        assert_eq!(
            series.day_starts,
            vec![
                RESET,
                RESET + DAY_SECS,
                RESET + 2 * DAY_SECS,
                MONDAY + 3 * DAY_SECS,
                MONDAY + 4 * DAY_SECS,
                MONDAY + 5 * DAY_SECS,
                MONDAY + 6 * DAY_SECS,
            ]
        );
    }

    #[test]
    fn days_follow_local_midnights_across_a_dst_change() {
        // Central Europe: CEST (+2) until 2026-10-25 01:00 UTC, then CET (+1).
        let cet = |at: i64| if at < 1_792_890_000 { 7_200 } else { 3_600 };
        // Monday 2026-10-26 00:00 CET; the window starts Monday 01:00 CEST.
        let resets_at = 1_792_969_200;
        let starts_at = resets_at - WEEK_SECS;
        let saturday_midnight = MONDAY + 5 * DAY_SECS - 2 * HOUR;
        let sunday_midnight = MONDAY + 6 * DAY_SECS - 2 * HOUR;
        let samples = [
            sample(1, resets_at, starts_at + HOUR),
            // Saturday 23:30 CEST.
            sample(20, resets_at, sunday_midnight - 30 * 60),
            // Sunday 00:30 CEST.
            sample(22, resets_at, sunday_midnight + 30 * 60),
            // Sunday 23:30 CET, 25 hours after Sunday's midnight.
            sample(30, resets_at, resets_at - 30 * 60),
        ];
        let series = daily_use(&samples, "claude", "weekly", resets_at - 10 * 60, cet).unwrap();
        // Saturday follows a Friday without samples.
        assert_eq!(
            series.days,
            vec![Some(1), None, None, None, None, Some(0), Some(10)]
        );
        assert_eq!(series.partial_from[5], Some(sunday_midnight - 30 * 60));
        assert_eq!(series.day_starts[0], MONDAY - 2 * HOUR);
        assert_eq!(series.day_starts[5], saturday_midnight);
        assert_eq!(series.day_starts[6], sunday_midnight);
        assert_eq!(series.current_day, Some(6));
    }

    #[test]
    fn a_gap_is_unknown_and_its_growth_is_in_no_day() {
        let wednesday = START + 2 * DAY_SECS;
        let friday = START + 4 * DAY_SECS;
        let samples = [
            // Asleep from the reset until Wednesday.
            sample(9, RESET, wednesday + HOUR),
            sample(12, RESET, wednesday + 5 * HOUR),
            // And again on Thursday.
            sample(15, RESET, friday + HOUR),
            sample(17, RESET, friday + 6 * HOUR),
            sample(18, RESET, friday + DAY_SECS + HOUR),
        ];
        let series = series(&samples, friday + DAY_SECS + 2 * HOUR);
        assert_eq!(
            series.days,
            vec![None, None, Some(3), None, Some(2), Some(1), None]
        );
        assert_eq!(
            series.partial_from,
            vec![
                None,
                None,
                Some(wednesday + HOUR),
                None,
                Some(friday + HOUR),
                None,
                None
            ]
        );
    }

    #[test]
    fn a_reset_mid_history_starts_a_separate_series() {
        let previous = RESET - WEEK_SECS;
        let samples = [
            sample(80, previous, START - 2 * HOUR),
            sample(95, previous, START - HOUR),
            sample(3, RESET, START + HOUR),
            sample(10, RESET, START + DAY_SECS + HOUR),
        ];
        let now = START + DAY_SECS + 2 * HOUR;
        // Never 3 - 95 across the reset; the earlier window's samples do not count.
        assert_eq!(
            days(&samples, now),
            vec![Some(3), Some(7), None, None, None, None, None]
        );
        // Before the new window was observed, the old one is shown until it resets.
        assert_eq!(series(&samples, START - 30 * 60).resets_at, previous);
        // After its reset with no newer sample, nothing is shown.
        assert_eq!(
            daily_use(&samples[..2], "claude", "weekly", previous + 1, utc),
            None
        );
    }

    #[test]
    fn duplicates_and_out_of_order_samples_are_tolerated() {
        let samples = [
            sample(10, RESET, START + DAY_SECS + 5 * HOUR),
            sample(2, RESET, START + HOUR),
            sample(6, RESET, START + DAY_SECS + HOUR),
            sample(2, RESET, START + HOUR),
            sample(10, RESET, START + DAY_SECS + 5 * HOUR),
            // One second apart: the same window, truncated differently.
            sample(11, RESET + 1, START + DAY_SECS + 6 * HOUR),
        ];
        let series = series(&samples, START + DAY_SECS + 7 * HOUR);
        assert_eq!(series.resets_at, RESET + 1);
        assert_eq!(series.days[..2], [Some(2), Some(9)], "{series:?}");
    }

    #[test]
    fn other_accounts_and_windows_are_ignored() {
        let mut other = sample(50, RESET, START + DAY_SECS + HOUR);
        other.account = "other".into();
        let mut short = sample(50, RESET, START + DAY_SECS + 2 * HOUR);
        short.window = "five_hour".into();
        let samples = [sample(1, RESET, START + HOUR), other, short];
        assert_eq!(
            days(&samples, START + DAY_SECS + 3 * HOUR),
            vec![Some(1), None, None, None, None, None, None]
        );
        assert_eq!(
            daily_use(&samples, "codex", "weekly", START + HOUR, utc),
            None
        );
    }

    fn observation(used_percent: u8, resets_at: u64) -> ProviderUsage {
        let mut usage = ProviderUsage::pending("claude", "Claude");
        usage.windows = vec![
            UsageWindow {
                id: "five_hour".into(),
                label: "5h".into(),
                used_percent: 99,
                resets_at: Some(resets_at),
                ..Default::default()
            },
            UsageWindow {
                id: "weekly".into(),
                label: "week".into(),
                used_percent,
                resets_at: Some(resets_at),
                ..Default::default()
            },
        ];
        usage
    }

    #[test]
    fn only_the_weekly_window_is_recorded_once_per_observation() {
        let mut log = SampleLog::default();
        assert!(log.append(&observation(7, RESET), START + HOUR));
        assert!(!log.append(&observation(7, RESET), START + HOUR));
        assert_eq!(log.samples(), [sample(7, RESET, START + HOUR)]);
    }

    #[test]
    fn retention_keeps_about_three_windows_and_a_hard_cap() {
        let mut log = SampleLog::default();
        for day in 0..30 {
            log.append(&observation(day as u8, RESET), day * DAY_SECS);
        }
        let oldest = log.samples().iter().map(|s| s.observed_at).min();
        assert_eq!(oldest, Some(29 * DAY_SECS - RETENTION_SECS));
        assert_eq!(log.samples().len(), 22);

        let mut log = SampleLog::default();
        for at in 0..(MAX_SAMPLES as u64 + 10) {
            log.append(&observation(1, RESET), at);
        }
        assert_eq!(log.samples().len(), MAX_SAMPLES);
        assert_eq!(log.samples()[0].observed_at, 10);
    }

    #[test]
    fn the_log_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-usage-samples-{}-{}",
            std::process::id(),
            crate::usage::now_unix()
        ));
        let path = dir.join(SAMPLES_FILE);
        let mut log = SampleLog::default();
        log.append(&observation(3, RESET), START);
        log.store(&path);
        assert_eq!(SampleLog::load_from(&path), log);
        assert_eq!(
            SampleLog::load_from(&dir.join("missing.json")),
            SampleLog::default()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
