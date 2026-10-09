//! `herdr todo run`'s usage gate (the user's rule, 2026-10-09): a new run,
//! or a retry's new attempt, is refused while any Claude window is fresh
//! and at least [`CLOSE_AT`] percent used, or while Claude's reading is
//! stale, failed or missing (unknown usage refuses). After a refusal the
//! gate stays closed until a fresh reading shows every Claude window below
//! [`REOPEN_BELOW`]; a passed `resets_at` makes the reading stale and asks
//! the poller for a new one, it does not reopen the gate by itself. The
//! state lives with the repository's runs in the worker store, so a
//! restart keeps it. `--ignore-usage`, on the user's word, admits anyway
//! and is recorded in the run's events. A run already started is never
//! touched, and no timer reads usage: the gate reads what `usage.read` has
//! when a run is asked for.

use serde_json::{json, Value};

use crate::api::schema::{UsageFreshness, UsageReport, UsageWindow};

/// A fresh window at or above this many percent used closes the gate.
pub(super) const CLOSE_AT: u8 = 90;
/// A closed gate reopens once every window is fresh and below this.
pub(super) const REOPEN_BELOW: u8 = 80;
/// The provider the gate reads: the driver starts only headless Claude.
const PROVIDER: &str = "claude";

/// What the gate decided for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    /// Admitted: every window fresh and below the threshold that applies.
    Admit { reopened: bool },
    /// Refused; the gate is closed after it.
    Refuse {
        /// Why, one entry per window that refuses (or one for no reading).
        blockers: Vec<Blocker>,
        /// A window is stale: the poller should read again.
        refresh: bool,
    },
}

/// One reason a run is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Blocker {
    /// The window's id and label, or none when Claude has no reading.
    pub(super) window: Option<(String, String)>,
    pub(super) used_percent: Option<u8>,
    /// Seconds since the window was observed.
    pub(super) age_secs: Option<u64>,
    /// `fresh`, `stale`, `failed` or `unknown`.
    pub(super) freshness: &'static str,
    pub(super) resets_at: Option<u64>,
    /// Why the window refuses, in words.
    pub(super) why: String,
}

fn freshness_name(freshness: Option<UsageFreshness>) -> &'static str {
    match freshness {
        Some(UsageFreshness::Fresh) => "fresh",
        Some(UsageFreshness::Stale) => "stale",
        Some(UsageFreshness::Failed) => "failed",
        Some(UsageFreshness::Unknown) | None => "unknown",
    }
}

fn blocker(window: &UsageWindow, now: u64, why: String) -> Blocker {
    Blocker {
        window: Some((window.id.clone(), window.label.clone())),
        used_percent: Some(window.used_percent),
        age_secs: window.observed_at.map(|at| now.saturating_sub(at)),
        freshness: freshness_name(window.freshness),
        resets_at: window.resets_at,
        why,
    }
}

/// The gate's decision on `report` (stamped with freshness at `now`, as
/// `usage.read` returns it; none when no poller runs) for a gate that is
/// `closed` or open.
pub(super) fn decide(report: Option<&UsageReport>, closed: bool, now: u64) -> Decision {
    let windows = report
        .and_then(|report| {
            report
                .providers
                .iter()
                .find(|usage| usage.provider == PROVIDER)
        })
        .map(|usage| usage.windows.as_slice())
        .unwrap_or_default();
    if windows.is_empty() {
        let why = match report {
            None => "herdr has no usage reading (usage polling is off or has not reported yet)",
            Some(report) if !report.enabled => "usage polling is off",
            Some(_) => "Claude's usage has no reading yet",
        };
        return Decision::Refuse {
            blockers: vec![Blocker {
                window: None,
                used_percent: None,
                age_secs: None,
                freshness: "unknown",
                resets_at: None,
                why: why.to_owned(),
            }],
            refresh: false,
        };
    }
    let threshold = if closed { REOPEN_BELOW } else { CLOSE_AT };
    let mut blockers = Vec::new();
    let mut refresh = false;
    for window in windows {
        if window.freshness != Some(UsageFreshness::Fresh) {
            refresh |= window.freshness == Some(UsageFreshness::Stale);
            blockers.push(blocker(
                window,
                now,
                "its usage is unknown: the reading is not fresh".to_owned(),
            ));
        } else if closed && window.used_percent >= REOPEN_BELOW {
            blockers.push(blocker(
                window,
                now,
                format!("the gate closed at a refusal reopens only below {threshold}%"),
            ));
        } else if !closed && window.used_percent >= CLOSE_AT {
            blockers.push(blocker(
                window,
                now,
                format!("{threshold}% or more is used"),
            ));
        }
    }
    if blockers.is_empty() {
        Decision::Admit { reopened: closed }
    } else {
        Decision::Refuse { blockers, refresh }
    }
}

/// The refusal as the `usage_gate` error's message: each window with its
/// value, the reading's age, its freshness and `resets_at`.
pub(super) fn refusal_message(blockers: &[Blocker]) -> String {
    let windows = blockers
        .iter()
        .map(|blocker| {
            let mut text = match &blocker.window {
                Some((id, label)) => format!("Claude window {id} ({label})"),
                None => "Claude".to_owned(),
            };
            if let Some(used) = blocker.used_percent {
                text.push_str(&format!(": {used}% used"));
            }
            text.push_str(&format!(", {}", blocker.freshness));
            match blocker.age_secs {
                Some(age) => text.push_str(&format!(", read {age}s ago")),
                None => text.push_str(", never read"),
            }
            match blocker.resets_at {
                Some(at) => text.push_str(&format!(", resets_at {at}")),
                None => text.push_str(", resets_at unknown"),
            }
            text.push_str(&format!(" ({})", blocker.why));
            text
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "usage gate: no new run while Claude's usage is high or unknown: {windows}. It admits \
         again once a fresh reading shows every Claude window below {REOPEN_BELOW}%; \
         --ignore-usage, on the user's word, overrides"
    )
}

/// The decision as the gate's stored body and the run's events record it.
pub(super) fn decision_json(decision: &Decision, ignored: bool) -> Value {
    match decision {
        Decision::Admit { reopened } => json!({
            "decision": "admit",
            "reopened": reopened,
        }),
        Decision::Refuse { blockers, .. } => json!({
            "decision": if ignored { "ignored" } else { "refuse" },
            "windows": blockers
                .iter()
                .map(|blocker| json!({
                    "window": blocker.window.as_ref().map(|(id, _)| id),
                    "used_percent": blocker.used_percent,
                    "age_secs": blocker.age_secs,
                    "freshness": blocker.freshness,
                    "resets_at": blocker.resets_at,
                    "why": blocker.why,
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ProviderUsage, ProviderUsageStatus};

    const NOW: u64 = 10_000;

    fn window(id: &str, used: u8, freshness: UsageFreshness) -> UsageWindow {
        UsageWindow {
            id: id.into(),
            label: id.into(),
            used_percent: used,
            resets_at: Some(NOW + 3_600),
            observed_at: Some(NOW - 30),
            freshness: Some(freshness),
            error_kind: None,
        }
    }

    fn report(windows: Vec<UsageWindow>) -> UsageReport {
        let mut claude = ProviderUsage::pending("claude", "Claude");
        claude.status = ProviderUsageStatus::Ok;
        claude.windows = windows;
        let mut codex = ProviderUsage::pending("codex", "Codex");
        codex.windows = vec![window("five_hour", 100, UsageFreshness::Fresh)];
        UsageReport {
            enabled: true,
            providers: vec![claude, codex],
        }
    }

    fn fresh(five_hour: u8, weekly: u8) -> UsageReport {
        report(vec![
            window("five_hour", five_hour, UsageFreshness::Fresh),
            window("seven_day", weekly, UsageFreshness::Fresh),
        ])
    }

    fn refused(decision: &Decision) -> Vec<(Option<String>, &'static str)> {
        match decision {
            Decision::Refuse { blockers, .. } => blockers
                .iter()
                .map(|blocker| (blocker.window.clone().map(|(id, _)| id), blocker.freshness))
                .collect(),
            Decision::Admit { .. } => panic!("admitted: {decision:?}"),
        }
    }

    #[test]
    fn an_open_gate_admits_89_and_refuses_90_and_91() {
        assert_eq!(
            decide(Some(&fresh(89, 89)), false, NOW),
            Decision::Admit { reopened: false }
        );
        for used in [90, 91] {
            assert_eq!(
                refused(&decide(Some(&fresh(40, used)), false, NOW)),
                [(Some("seven_day".into()), "fresh")]
            );
        }
        // Another provider's 100% does not count: the driver starts Claude.
        assert!(matches!(
            decide(Some(&fresh(10, 10)), false, NOW),
            Decision::Admit { .. }
        ));
    }

    #[test]
    fn a_closed_gate_reopens_only_below_80() {
        for used in [80, 85, 89] {
            assert!(
                matches!(
                    decide(Some(&fresh(used, 10)), true, NOW),
                    Decision::Refuse { .. }
                ),
                "{used}"
            );
        }
        assert_eq!(
            decide(Some(&fresh(79, 79)), true, NOW),
            Decision::Admit { reopened: true }
        );
    }

    #[test]
    fn a_stale_or_failed_reading_refuses_and_only_stale_asks_for_a_new_one() {
        let stale = report(vec![
            window("five_hour", 5, UsageFreshness::Stale),
            window("seven_day", 5, UsageFreshness::Fresh),
        ]);
        let decision = decide(Some(&stale), false, NOW);
        assert_eq!(refused(&decision), [(Some("five_hour".into()), "stale")]);
        assert!(matches!(decision, Decision::Refuse { refresh: true, .. }));

        let failed = report(vec![window("five_hour", 5, UsageFreshness::Failed)]);
        let decision = decide(Some(&failed), true, NOW);
        assert_eq!(refused(&decision), [(Some("five_hour".into()), "failed")]);
        assert!(matches!(decision, Decision::Refuse { refresh: false, .. }));
    }

    #[test]
    fn no_reading_is_unknown_and_refuses() {
        for report in [None, Some(report(Vec::new()))] {
            assert_eq!(
                refused(&decide(report.as_ref(), false, NOW)),
                [(None, "unknown")]
            );
        }
        let mut unstamped = fresh(10, 10);
        unstamped.providers[0].windows[0].freshness = None;
        assert_eq!(
            refused(&decide(Some(&unstamped), false, NOW)),
            [(Some("five_hour".into()), "unknown")]
        );
    }

    #[test]
    fn the_message_names_the_window_value_age_freshness_and_reset() {
        let Decision::Refuse { blockers, .. } = decide(Some(&fresh(91, 10)), false, NOW) else {
            panic!("admitted");
        };
        let message = refusal_message(&blockers);
        for part in [
            "five_hour",
            "91% used",
            "fresh",
            "read 30s ago",
            &format!("resets_at {}", NOW + 3_600),
            "--ignore-usage",
        ] {
            assert!(message.contains(part), "{part:?} not in {message}");
        }
    }
}
