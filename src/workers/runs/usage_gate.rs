//! `herdr todo run`'s usage gate (the user's rule, 2026-10-09; asking the
//! provider, 2026-10-10): a new run, or a retry's new attempt, reads
//! Claude's usage from the provider in that call and is refused while any
//! Claude window is at least [`CLOSE_AT`] percent used, or when the read
//! fails (unknown usage refuses). After a refusal the gate stays closed
//! until an answer shows every Claude window below [`REOPEN_BELOW`]. The
//! gate decides on that answer alone: never on the usage poller's cached
//! report, a reading's age or a window's reset time. The state lives with
//! the repository's runs in the worker store, so a restart keeps it.
//! `--ignore-usage`, on the user's word, admits anyway and is recorded in
//! the run's events. A run already started is never touched.

use serde_json::{json, Value};

use crate::api::schema::ProviderUsage;

/// A window at or above this many percent used closes the gate.
pub(super) const CLOSE_AT: u8 = 90;
/// A closed gate reopens once every window is below this.
pub(super) const REOPEN_BELOW: u8 = 80;

/// What the gate decided for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    /// Admitted: every window below the threshold that applies.
    Admit { reopened: bool },
    /// Refused; the gate is closed after it.
    Refuse {
        /// Why, one entry per window that refuses (or one for no answer).
        blockers: Vec<Blocker>,
    },
}

/// One reason a run is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Blocker {
    /// The window's id and label, or none when Claude gave no answer.
    pub(super) window: Option<(String, String)>,
    pub(super) used_percent: Option<u8>,
    /// Why the window refuses, in words.
    pub(super) why: String,
}

fn unknown(why: String) -> Decision {
    Decision::Refuse {
        blockers: vec![Blocker {
            window: None,
            used_percent: None,
            why,
        }],
    }
}

/// The gate's decision on `answer`, Claude's usage as the provider gave it
/// just now (or the read's failure), for a gate that is `closed` or open.
pub(super) fn decide(answer: Result<&ProviderUsage, &str>, closed: bool) -> Decision {
    let usage = match answer {
        Ok(usage) => usage,
        Err(failure) => return unknown(format!("reading Claude's usage failed: {failure}")),
    };
    if usage.windows.is_empty() {
        return unknown("Claude's usage answer has no window".to_owned());
    }
    let threshold = if closed { REOPEN_BELOW } else { CLOSE_AT };
    let blockers = usage
        .windows
        .iter()
        .filter(|window| window.used_percent >= threshold)
        .map(|window| Blocker {
            window: Some((window.id.clone(), window.label.clone())),
            used_percent: Some(window.used_percent),
            why: if closed {
                format!("the gate closed at a refusal reopens only below {threshold}%")
            } else {
                format!("{threshold}% or more is used")
            },
        })
        .collect::<Vec<_>>();
    if blockers.is_empty() {
        Decision::Admit { reopened: closed }
    } else {
        Decision::Refuse { blockers }
    }
}

/// The refusal as the `usage_gate` error's message: each window with its
/// value, or the read's failure.
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
            text.push_str(&format!(" ({})", blocker.why));
            text
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "usage gate: no new run while Claude's usage is high or unknown: {windows}. It admits \
         again once Claude's usage, read when a run is asked for, shows every window below \
         {REOPEN_BELOW}%; --ignore-usage, on the user's word, overrides"
    )
}

/// The decision as the gate's stored body and the run's events record it.
pub(super) fn decision_json(decision: &Decision, ignored: bool) -> Value {
    match decision {
        Decision::Admit { reopened } => json!({
            "decision": "admit",
            "reopened": reopened,
        }),
        Decision::Refuse { blockers } => json!({
            "decision": if ignored { "ignored" } else { "refuse" },
            "windows": blockers
                .iter()
                .map(|blocker| json!({
                    "window": blocker.window.as_ref().map(|(id, _)| id),
                    "used_percent": blocker.used_percent,
                    "why": blocker.why,
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ProviderUsageStatus, UsageFreshness, UsageWindow};

    fn window(id: &str, used: u8) -> UsageWindow {
        UsageWindow {
            id: id.into(),
            label: id.into(),
            used_percent: used,
            resets_at: Some(10_000),
            observed_at: Some(9_970),
            freshness: Some(UsageFreshness::Fresh),
            error_kind: None,
        }
    }

    fn answer(five_hour: u8, weekly: u8) -> ProviderUsage {
        let mut claude = ProviderUsage::pending("claude", "Claude");
        claude.status = ProviderUsageStatus::Ok;
        claude.windows = vec![window("five_hour", five_hour), window("seven_day", weekly)];
        claude
    }

    fn refused(decision: &Decision) -> Vec<Option<String>> {
        match decision {
            Decision::Refuse { blockers } => blockers
                .iter()
                .map(|blocker| blocker.window.clone().map(|(id, _)| id))
                .collect(),
            Decision::Admit { .. } => panic!("admitted: {decision:?}"),
        }
    }

    #[test]
    fn an_open_gate_admits_89_and_refuses_90_and_91() {
        assert_eq!(
            decide(Ok(&answer(89, 89)), false),
            Decision::Admit { reopened: false }
        );
        for used in [90, 91] {
            assert_eq!(
                refused(&decide(Ok(&answer(40, used)), false)),
                [Some("seven_day".into())]
            );
        }
    }

    #[test]
    fn a_closed_gate_reopens_only_below_80() {
        for used in [80, 85, 89] {
            assert!(
                matches!(decide(Ok(&answer(used, 10)), true), Decision::Refuse { .. }),
                "{used}"
            );
        }
        assert_eq!(
            decide(Ok(&answer(79, 79)), true),
            Decision::Admit { reopened: true }
        );
    }

    #[test]
    fn a_failed_read_or_an_answer_without_windows_refuses() {
        for closed in [false, true] {
            let decision = decide(Err("Claude login expired"), closed);
            assert_eq!(refused(&decision), [None]);
            let Decision::Refuse { blockers } = decision else {
                unreachable!();
            };
            assert!(refusal_message(&blockers).contains("Claude login expired"));
        }
        let mut empty = answer(0, 0);
        empty.windows.clear();
        assert_eq!(refused(&decide(Ok(&empty), false)), [None]);
    }

    #[test]
    fn the_answer_decides_alone_whatever_its_window_times_say() {
        // A window marked stale, failed or unstamped, observed long ago
        // or past its reset, decides by its percentage only: the gate has
        // no time model.
        let mut old = answer(16, 10);
        for (window, freshness) in old
            .windows
            .iter_mut()
            .zip([Some(UsageFreshness::Stale), None])
        {
            window.freshness = freshness;
            window.observed_at = Some(1);
            window.resets_at = Some(2);
        }
        assert_eq!(decide(Ok(&old), false), Decision::Admit { reopened: false });
        old.windows[0].used_percent = 90;
        old.windows[0].freshness = Some(UsageFreshness::Failed);
        assert_eq!(
            refused(&decide(Ok(&old), false)),
            [Some("five_hour".into())]
        );
    }

    #[test]
    fn the_gate_never_reads_the_cached_report() {
        // The gate's code: this module up to its tests, and the
        // supervisor's read and gate in `runs.rs`.
        let module = include_str!("usage_gate.rs");
        let module = &module[..module.find("#[cfg(test)]").unwrap()];
        let runs = include_str!("../runs.rs");
        let start = runs.find("fn read_claude_usage(").unwrap();
        let end = runs[start..].find("fn load_run(").unwrap() + start;
        let gate = format!("{module}{}", &runs[start..end]);
        assert!(gate.contains("crate::usage::read_claude_now()"));
        for cached in [
            "UsageReport",
            "published",
            "with_freshness",
            "request_refresh",
            "observed_at",
            "freshness",
            "resets_at",
            "stale",
            "expired",
        ] {
            assert!(!gate.contains(cached), "the gate uses {cached:?}");
        }
    }

    #[test]
    fn the_message_names_the_window_and_its_value() {
        let Decision::Refuse { blockers } = decide(Ok(&answer(91, 10)), false) else {
            panic!("admitted");
        };
        let message = refusal_message(&blockers);
        for part in ["five_hour", "91% used", "--ignore-usage"] {
            assert!(message.contains(part), "{part:?} not in {message}");
        }
    }
}
