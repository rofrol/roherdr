//! `herdr todo run`'s usage gate (the user's rule, 2026-10-09; asking the
//! provider, 2026-10-10): a new run, or a retry's new attempt, reads
//! Claude's usage from the provider in that call and is refused while any
//! Claude window is at least `[usage] gate_close_percent` (default 90)
//! used, or when the read fails (unknown usage refuses). After a refusal
//! a high latch stays closed until an answer shows every Claude window below
//! `[usage] gate_reopen_percent` (default 80). Both are read from the
//! config at each check ([`Thresholds`]), for every provider's gate. The
//! gate decides on that answer alone: never on the usage poller's cached
//! report, a reading's age or a window's reset time. The state lives with
//! the repository's runs in the worker store, so a restart keeps it.
//! `--ignore-usage`, on the user's word, admits anyway and is recorded in
//! the run's events. A run already started is never touched.
//!
//! The gate reads the provider of the agent the run's worker runs
//! ([`GateProvider`]): Claude's windows for a Claude worker, the ChatGPT
//! plan's (as Codex reads them) for a pi worker on `openai-codex`. Each
//! provider has one gate state for all repositories, since the subscription
//! is the account's (the user, 2026-10-16), so a Claude gate closed at 90%
//! leaves pi runs to OpenAI's usage, and one closed from any repository
//! refuses every other.
//!
//! The gate also caps the workers of one provider admitted and not yet
//! exited **in one repository**, the scope of a coordinator (`[usage]
//! gate_max_workers`, default 2; the user, 2026-10-16, narrowed to the
//! coordinator's repository 2026-10-18), `todo run` attempts and `worker.run`
//! runs alike
//! (a headless item coordinator passes the usage check only, since its
//! runs need the slots): under the provider's lock it counts the
//! admissions the store still holds ([`crate::workers::store`]'s
//! `usage_admissions`, each derived from the worker or run it names, never
//! a counter) and records the new one ([`Holder`]) before the worker is
//! spawned. A worker's exit or loss frees its slot, and wakes the starts
//! waiting on the gate; a failed spawn ends its admission, and a server's
//! start ends those whose server went away before the worker was recorded.

use serde_json::{json, Value};

pub(in crate::workers) static CHECK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static OPENAI_CHECK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static DEEPSEEK_CHECK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn measurement_lock(provider_id: &str) -> std::sync::MutexGuard<'static, ()> {
    crate::workers::lock(match provider_id {
        "openai" => &OPENAI_CHECK,
        "deepseek" => &DEEPSEEK_CHECK,
        _ => &CHECK,
    })
}

use crate::api::schema::{ProviderUsage, WorkerAgent};
pub(super) use crate::config::UsageGateThresholds as Thresholds;

/// Whose usage the gate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::workers) enum GateProvider {
    Claude,
    /// OpenAI's ChatGPT plan, which `openai-codex` models run on.
    OpenAi,
    /// DeepSeek's prepaid API balance, which `deepseek` models spend.
    DeepSeek,
}

/// What kind of answer a provider's live usage read gives: rate-limit
/// windows to compare with a threshold, or a prepaid balance to spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Windows,
    Balance,
}

impl GateProvider {
    /// The provider of the agent's default model: Claude's windows for a
    /// Claude worker, the ChatGPT plan's for pi's default model.
    pub(in crate::workers) fn of(agent: WorkerAgent) -> Self {
        match agent {
            WorkerAgent::Claude => Self::Claude,
            WorkerAgent::Pi => Self::OpenAi,
        }
    }

    /// The gate of the provider `model` belongs to, for a worker running
    /// `agent`: pi model ids are `provider/model`, and the gate must read
    /// the provider that spends, not the agent's default one. Without a
    /// model it is the agent's default ([`super::super::pi::DEFAULT_MODEL`]
    /// for pi, so the table has one source). A model whose provider herdr has
    /// no live usage read for is refused here, so no run starts under a gate
    /// that would read nothing (fail closed; a new provider is one arm of
    /// this match and one `Provider` in [`crate::usage`]). A Claude worker's
    /// model is Claude Code's own (`--model opus` and the like), so its gate
    /// is Claude's whatever the model names.
    pub(in crate::workers) fn for_worker(
        agent: WorkerAgent,
        model: Option<&str>,
    ) -> Result<Self, String> {
        if agent == WorkerAgent::Claude {
            return Ok(Self::Claude);
        }
        let model = model.unwrap_or(super::super::pi::DEFAULT_MODEL);
        let Some((provider, name)) = model.split_once('/') else {
            return Err(format!(
                "a pi model is provider/model (for example deepseek/deepseek-flash), not {model:?}"
            ));
        };
        if name.is_empty() {
            return Err(format!(
                "a pi model is provider/model (for example deepseek/deepseek-flash), not {model:?}"
            ));
        }
        match provider {
            "openai-codex" => Ok(Self::OpenAi),
            "deepseek" => Ok(Self::DeepSeek),
            "herdr-anthropic" => Err(format!(
                "{model} runs through herdr's credit proxy; start the run with --agent \
                 pi-anthropic, which pins [workers] anthropic_api_model and reads the credit cap"
            )),
            other => Err(format!(
                "the usage gate has no live usage read for provider {other:?} of model {model}; \
                 gated pi workers run openai-codex/* (the ChatGPT plan) or deepseek/* models, and \
                 a provider without a read is not admitted"
            )),
        }
    }

    pub(super) fn kind(self) -> Kind {
        match self {
            Self::Claude | Self::OpenAi => Kind::Windows,
            Self::DeepSeek => Kind::Balance,
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::OpenAi => "OpenAI (ChatGPT plan)",
            Self::DeepSeek => "DeepSeek",
        }
    }

    /// The gate's row in the store: one per provider account
    /// (`provider:claude`, `provider:openai`, `provider:deepseek`), whatever
    /// the repository.
    pub(super) fn store_key(self) -> String {
        format!("provider:{}", self.id())
    }

    /// The provider whose gate row `key` is.
    pub(super) fn of_store_key(key: &str) -> Option<Self> {
        [Self::Claude, Self::OpenAi, Self::DeepSeek]
            .into_iter()
            .find(|provider| provider.store_key() == key)
    }
}

/// What an admission of the gate is held by: a run's attempt, whose worker
/// the run's driver starts, or one `worker.run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::workers) enum Holder {
    Run { run_id: String, attempt: u32 },
    WorkerRun(String),
}

impl Holder {
    /// A new `worker.run`'s own holder.
    pub(in crate::workers) fn new_worker_run() -> Self {
        use sha2::{Digest, Sha256};
        use std::sync::atomic::{AtomicU64, Ordering};
        static RUNS: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let count = RUNS.fetch_add(1, Ordering::Relaxed);
        let digest = Sha256::digest(format!("{nanos}:{}:{count}", std::process::id()));
        Self::WorkerRun(
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// Its key in the store.
    pub(in crate::workers) fn key(&self) -> String {
        match self {
            Self::Run { run_id, attempt } => format!("run:{run_id}:{attempt}"),
            Self::WorkerRun(id) => format!("worker.run:{id}"),
        }
    }

    pub(super) fn run(&self) -> Option<(&str, u32)> {
        match self {
            Self::Run { run_id, attempt } => Some((run_id, *attempt)),
            Self::WorkerRun(_) => None,
        }
    }

    /// Whether `other` takes no slot from this one: the same admission, or
    /// another attempt of the same run, whose worker the next attempt
    /// replaces.
    pub(super) fn shares_slot_with(&self, other: &crate::workers::store::Admission) -> bool {
        other.holder == self.key()
            || matches!(self, Self::Run { run_id, .. } if other.run_id.as_deref() == Some(run_id))
    }
}

/// How an admission names itself in a refusal.
pub(super) fn holder_label(admission: &crate::workers::store::Admission) -> String {
    let what = match (&admission.run_id, admission.attempt) {
        (Some(run_id), Some(attempt)) => format!("run {run_id} attempt {attempt}"),
        _ => "worker.run".to_owned(),
    };
    match &admission.worker_id {
        Some(worker_id) => format!("{what} (worker {worker_id}, {})", admission.repo),
        None => format!("{what} (not started yet, {})", admission.repo),
    }
}

/// The refusal for the cap: the provider, the cap and every admission that
/// holds a slot in this repository.
pub(super) fn cap_refusal_message(
    provider: GateProvider,
    max_workers: u32,
    held: &[crate::workers::store::Admission],
    repo: &str,
    display_path: &str,
) -> String {
    let holders = held.iter().map(holder_label).collect::<Vec<_>>().join("; ");
    format!(
        "usage gate: no new worker while {count} {name} workers are admitted and not exited in \
         repository {repo} (path {display_path}), at most {max_workers} per repository and provider ([usage] gate_max_workers): \
         {holders}. A slot frees when one of them exits; --ignore-usage, on the user's word, \
         overrides",
        count = held.len(),
        name = provider.label(),
    )
}

/// The cap's part of the recorded decision: `held` are the slots others
/// hold (not this holder's own, nor its run's other attempts).
pub(super) fn cap_json(
    max_workers: u32,
    held: &[crate::workers::store::Admission],
    holder: &Holder,
    ignored: bool,
) -> Value {
    json!({
        "max_workers": max_workers,
        "held": held.iter().map(|admission| admission.holder.clone()).collect::<Vec<_>>(),
        "holder": holder.key(),
        "ignored": ignored,
    })
}

/// Durable state, independent for each repository and provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    Open,
    Unknown,
    High,
}

impl State {
    pub(super) fn from_record(closed: bool, body: &Value) -> Self {
        if !closed {
            Self::Open
        } else if body["state"] == "unknown" {
            Self::Unknown
        } else {
            // Older closed rows are conservatively high: their history is unknown.
            Self::High
        }
    }

    pub(super) fn after(self, decision: &Decision) -> Self {
        match decision {
            Decision::Admit { .. } => Self::Open,
            Decision::Refuse { blockers } => {
                if self == Self::High || blockers.iter().any(|b| b.used_percent.is_some()) {
                    Self::High
                } else {
                    Self::Unknown
                }
            }
        }
    }

    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Unknown => "unknown",
            Self::High => "high",
        }
    }
}

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

/// The gate's decision on `answer`, `provider`'s usage as it gave it just
/// now (or the read's failure), for a gate that is `closed` or open, with
/// `thresholds` as the config gives them at this check. A rate-limit
/// provider is judged by its windows against the threshold, and an answer
/// without a window is unknown usage. A provider whose usage is a prepaid
/// balance is judged by that balance alone (there is no percentage to
/// compare): the provider says it will spend, refuses, or does not say.
pub(super) fn decide(
    provider: GateProvider,
    thresholds: Thresholds,
    answer: Result<&ProviderUsage, &str>,
    closed: bool,
) -> Decision {
    let name = provider.label();
    let usage = match answer {
        Ok(usage) => usage,
        Err(failure) => {
            return unknown(format!(
                "usage read failed ({name}: {failure}), waiting for a successful measurement"
            ))
        }
    };
    if provider.kind() == Kind::Balance {
        if usage.balances.is_empty() {
            return unknown(format!("{name}'s usage answer has no balance"));
        }
        return match usage.spendable {
            Some(true) => Decision::Admit { reopened: closed },
            Some(false) => Decision::Refuse {
                blockers: vec![Blocker {
                    window: None,
                    used_percent: None,
                    why: "the provider says the balance is insufficient for API calls".into(),
                }],
            },
            None => unknown(format!(
                "{name}'s usage answer does not say whether it will spend"
            )),
        };
    }
    if usage.windows.is_empty() {
        return unknown(format!("{name}'s usage answer has no window"));
    }
    let threshold = if closed {
        thresholds.reopen_below
    } else {
        thresholds.close_at
    };
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
/// value, or the read's failure, and the thresholds that apply. A balance
/// provider has no thresholds: it is refused only while its balance cannot
/// be spent, and every dispatch reads it again.
pub(super) fn refusal_message(
    provider: GateProvider,
    thresholds: Thresholds,
    blockers: &[Blocker],
) -> String {
    let name = provider.label();
    let windows = blockers
        .iter()
        .map(|blocker| {
            let mut text = match &blocker.window {
                Some((id, label)) => format!("{name} window {id} ({label})"),
                None => name.to_owned(),
            };
            if let Some(used) = blocker.used_percent {
                text.push_str(&format!(": {used}% used"));
            }
            text.push_str(&format!(" ({})", blocker.why));
            text
        })
        .collect::<Vec<_>>()
        .join("; ");
    match provider.kind() {
        Kind::Windows => format!(
            "usage gate: no new run while {name}'s usage is high or unknown: {windows}. It closes \
             at {close}% ([usage] gate_close_percent). A high latch reopens below {reopen}% ([usage] \
             gate_reopen_percent); unknown reopens on the first successful measurement below \
             {close}% unless high was already latched. Dispatch always reads usage again; \
             --ignore-usage, on the user's word, overrides",
            close = thresholds.close_at,
            reopen = thresholds.reopen_below,
        ),
        Kind::Balance => format!(
            "usage gate: no new run while {name}'s balance cannot be spent: {windows}. Dispatch \
             always reads the balance again; --ignore-usage, on the user's word, overrides"
        ),
    }
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
    fn unknown_and_high_have_distinct_recovery_for_each_provider() {
        for provider in [GateProvider::Claude, GateProvider::OpenAi] {
            for thresholds in [
                Thresholds::default(),
                Thresholds {
                    close_at: 95,
                    reopen_below: 70,
                    ..Thresholds::default()
                },
            ] {
                let failed = decide(provider, thresholds, Err("DNS lookup failed"), false);
                assert_eq!(State::Open.after(&failed), State::Unknown);
                assert_eq!(State::High.after(&failed), State::High);
                let usage = answer(thresholds.close_at - 1, thresholds.close_at - 1);
                let recovered = decide(provider, thresholds, Ok(&usage), false);
                assert_eq!(State::Unknown.after(&recovered), State::Open);
                let still_high = decide(provider, thresholds, Ok(&usage), true);
                assert_eq!(State::High.after(&still_high), State::High);
                let low = answer(thresholds.reopen_below - 1, thresholds.reopen_below - 1);
                assert_eq!(
                    State::High.after(&decide(provider, thresholds, Ok(&low), true)),
                    State::Open
                );
                assert_eq!(
                    State::from_record(true, &json!({"state": "unknown"})),
                    State::Unknown
                );
                assert_eq!(
                    State::from_record(true, &json!({"decision": "refuse"})),
                    State::High
                );
                assert_eq!(State::from_record(false, &Value::Null), State::Open);
            }
        }
    }

    #[test]
    fn an_open_gate_admits_89_and_refuses_90_and_91() {
        assert_eq!(
            decide(
                GateProvider::Claude,
                Thresholds::default(),
                Ok(&answer(89, 89)),
                false
            ),
            Decision::Admit { reopened: false }
        );
        for used in [90, 91] {
            assert_eq!(
                refused(&decide(
                    GateProvider::Claude,
                    Thresholds::default(),
                    Ok(&answer(40, used)),
                    false
                )),
                [Some("seven_day".into())]
            );
        }
    }

    #[test]
    fn a_closed_gate_reopens_only_below_80() {
        for used in [80, 85, 89] {
            assert!(
                matches!(
                    decide(
                        GateProvider::Claude,
                        Thresholds::default(),
                        Ok(&answer(used, 10)),
                        true
                    ),
                    Decision::Refuse { .. }
                ),
                "{used}"
            );
        }
        assert_eq!(
            decide(
                GateProvider::Claude,
                Thresholds::default(),
                Ok(&answer(79, 79)),
                true
            ),
            Decision::Admit { reopened: true }
        );
    }

    #[test]
    fn a_failed_read_or_an_answer_without_windows_refuses() {
        for closed in [false, true] {
            let decision = decide(
                GateProvider::Claude,
                Thresholds::default(),
                Err("Claude login expired"),
                closed,
            );
            assert_eq!(refused(&decision), [None]);
            let Decision::Refuse { blockers } = decision else {
                unreachable!();
            };
            assert!(
                refusal_message(GateProvider::Claude, Thresholds::default(), &blockers)
                    .contains("Claude login expired")
            );
        }
        let mut empty = answer(0, 0);
        empty.windows.clear();
        assert_eq!(
            refused(&decide(
                GateProvider::Claude,
                Thresholds::default(),
                Ok(&empty),
                false
            )),
            [None]
        );
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
        assert_eq!(
            decide(GateProvider::Claude, Thresholds::default(), Ok(&old), false),
            Decision::Admit { reopened: false }
        );
        old.windows[0].used_percent = 90;
        old.windows[0].freshness = Some(UsageFreshness::Failed);
        assert_eq!(
            refused(&decide(
                GateProvider::Claude,
                Thresholds::default(),
                Ok(&old),
                false
            )),
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
        let start = runs.find("fn read_provider_usage(").unwrap();
        let end = runs[start..].find("fn load_run(").unwrap() + start;
        let gate = format!("{module}{}", &runs[start..end]);
        assert!(gate.contains("crate::usage::read_claude_now()"));
        assert!(gate.contains("crate::usage::read_chatgpt_plan_now()"));
        assert!(gate.contains("crate::usage::read_deepseek_now()"));
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
    fn a_pi_worker_reads_openai_with_a_gate_of_its_own() {
        assert_eq!(GateProvider::of(WorkerAgent::Pi), GateProvider::OpenAi);
        assert_eq!(GateProvider::of(WorkerAgent::Claude), GateProvider::Claude);
        assert_eq!(GateProvider::Claude.store_key(), "provider:claude");
        assert_eq!(GateProvider::OpenAi.store_key(), "provider:openai");
        for provider in [GateProvider::Claude, GateProvider::OpenAi] {
            assert_eq!(
                GateProvider::of_store_key(&provider.store_key()),
                Some(provider)
            );
        }
        assert_eq!(GateProvider::of_store_key("/r"), None);
        let Decision::Refuse { blockers } = decide(
            GateProvider::OpenAi,
            Thresholds::default(),
            Ok(&answer(95, 10)),
            false,
        ) else {
            panic!("admitted");
        };
        let message = refusal_message(GateProvider::OpenAi, Thresholds::default(), &blockers);
        assert!(
            message.contains("OpenAI (ChatGPT plan) window five_hour"),
            "{message}"
        );
        assert!(!message.contains("Claude"), "{message}");
        let failed = decide(
            GateProvider::OpenAi,
            Thresholds::default(),
            Err("no login"),
            false,
        );
        let Decision::Refuse { blockers } = failed else {
            panic!("admitted");
        };
        assert!(
            refusal_message(GateProvider::OpenAi, Thresholds::default(), &blockers)
                .contains("OpenAI")
        );
    }

    #[test]
    fn a_pi_model_picks_the_gate_of_its_own_provider() {
        use GateProvider as P;
        assert_eq!(P::for_worker(WorkerAgent::Claude, None), Ok(P::Claude));
        assert_eq!(
            P::for_worker(WorkerAgent::Claude, Some("opus")),
            Ok(P::Claude)
        );
        assert_eq!(P::for_worker(WorkerAgent::Pi, None), Ok(P::OpenAi));
        for model in ["openai-codex/gpt-6.1-sol", "openai-codex/gpt-6"] {
            assert_eq!(P::for_worker(WorkerAgent::Pi, Some(model)), Ok(P::OpenAi));
        }
        assert_eq!(
            P::for_worker(WorkerAgent::Pi, Some("deepseek/deepseek-flash")),
            Ok(P::DeepSeek)
        );
        // A provider herdr cannot read is refused, never gated on nothing,
        // and a malformed id says what a pi model looks like.
        for model in [
            "openrouter/some-model",
            "google/gemini-3",
            "moonshot/kimi-k2",
        ] {
            let refused = P::for_worker(WorkerAgent::Pi, Some(model)).unwrap_err();
            assert!(refused.contains("no live usage read"), "{refused}");
        }
        for malformed in ["gpt-6.1-sol", "deepseek", "deepseek/", "openai-codex/", "/"] {
            let refused = P::for_worker(WorkerAgent::Pi, Some(malformed)).unwrap_err();
            assert!(
                refused.contains("a pi model is provider/model"),
                "{refused}"
            );
        }
        let credit =
            P::for_worker(WorkerAgent::Pi, Some("herdr-anthropic/claude-sonnet-5-5")).unwrap_err();
        assert!(credit.contains("--agent pi-anthropic"), "{credit}");
        assert_eq!(P::DeepSeek.store_key(), "provider:deepseek");
        assert_eq!(P::of_store_key("provider:deepseek"), Some(P::DeepSeek));
    }

    /// A prepaid balance is not a percentage: the gate admits while the
    /// provider says it will spend, refuses a balance it refuses to spend
    /// and treats an answer that says neither as unknown. A windowless
    /// rate-limit answer keeps refusing (it is still broken).
    #[test]
    fn a_deepseek_run_reads_its_balance_not_a_chatgpt_window() {
        let mut balance = ProviderUsage::pending("deepseek", "DeepSeek");
        balance.balances = vec![crate::api::schema::UsageBalance {
            currency: "USD".into(),
            total: "13.41".into(),
            granted: None,
            topped_up: None,
        }];
        let spendable = |spendable: Option<bool>| {
            let mut usage = balance.clone();
            usage.spendable = spendable;
            usage
        };
        assert_eq!(
            decide(
                GateProvider::DeepSeek,
                Thresholds::default(),
                Ok(&spendable(Some(true))),
                false
            ),
            Decision::Admit { reopened: false }
        );
        // A closed balance gate (a failed read) reopens on a good balance,
        // whatever the percentage thresholds say.
        assert_eq!(
            decide(
                GateProvider::DeepSeek,
                Thresholds::default(),
                Ok(&spendable(Some(true))),
                true
            ),
            Decision::Admit { reopened: true }
        );
        let Decision::Refuse { blockers } = decide(
            GateProvider::DeepSeek,
            Thresholds::default(),
            Ok(&spendable(Some(false))),
            false,
        ) else {
            panic!("an unspendable balance was admitted");
        };
        assert!(blockers[0].why.contains("insufficient"), "{blockers:?}");
        let message = refusal_message(GateProvider::DeepSeek, Thresholds::default(), &blockers);
        assert!(
            message.contains("DeepSeek's balance cannot be spent"),
            "{message}"
        );
        assert!(message.contains("--ignore-usage"), "{message}");
        for unknown_answer in [spendable(None), {
            let mut empty = balance.clone();
            empty.balances.clear();
            empty.spendable = Some(true);
            empty
        }] {
            assert!(
                matches!(
                    decide(
                        GateProvider::DeepSeek,
                        Thresholds::default(),
                        Ok(&unknown_answer),
                        false
                    ),
                    Decision::Refuse { .. }
                ),
                "{unknown_answer:?}"
            );
        }
        // The ChatGPT plan's empty window is still unknown usage.
        let mut windowless = answer(10, 10);
        windowless.windows.clear();
        assert!(matches!(
            decide(
                GateProvider::OpenAi,
                Thresholds::default(),
                Ok(&windowless),
                false
            ),
            Decision::Refuse { .. }
        ));
    }

    #[test]
    fn the_message_names_the_window_and_its_value() {
        let Decision::Refuse { blockers } = decide(
            GateProvider::Claude,
            Thresholds::default(),
            Ok(&answer(91, 10)),
            false,
        ) else {
            panic!("admitted");
        };
        let message = refusal_message(GateProvider::Claude, Thresholds::default(), &blockers);
        for part in [
            "five_hour",
            "91% used",
            "--ignore-usage",
            "at 90% ([usage] gate_close_percent)",
            "below 80% ([usage] gate_reopen_percent)",
        ] {
            assert!(message.contains(part), "{part:?} not in {message}");
        }
    }

    #[test]
    fn configured_thresholds_decide_for_every_provider_and_name_themselves() {
        let thresholds = Thresholds {
            close_at: 95,
            reopen_below: 50,
            ..Thresholds::default()
        };
        for provider in [GateProvider::Claude, GateProvider::OpenAi] {
            assert_eq!(
                decide(provider, thresholds, Ok(&answer(94, 10)), false),
                Decision::Admit { reopened: false }
            );
            let Decision::Refuse { blockers } =
                decide(provider, thresholds, Ok(&answer(95, 10)), false)
            else {
                panic!("admitted");
            };
            let message = refusal_message(provider, thresholds, &blockers);
            for part in ["at 95%", "below 50%"] {
                assert!(message.contains(part), "{part:?} not in {message}");
            }
            assert!(matches!(
                decide(provider, thresholds, Ok(&answer(50, 10)), true),
                Decision::Refuse { .. }
            ));
            assert_eq!(
                decide(provider, thresholds, Ok(&answer(49, 49)), true),
                Decision::Admit { reopened: true }
            );
        }
    }
}
