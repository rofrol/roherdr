//! Provider allowance polling for the usage footer and `usage.read`.
//!
//! One background thread refreshes every enabled provider on an interval and
//! hands the merged report to the app loop as an [`AppEvent::UsageUpdated`].
//! Credentials stay inside this module; reports carry only allowance facts.
//! Observations and rate-limit backoff are shared across herdr instances
//! through [`cache::UsageCache`].

mod cache;
mod claude;
mod codex;
mod deepseek;
mod gemini;
mod http;
mod keys;
mod kimi;
mod openai_api;
mod openrouter;

pub(crate) use keys::DEFAULT_AUTH_FILE;

use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use self::cache::{Plan, UsageCache};
use crate::api::schema::{
    ProviderUsage, ProviderUsageStatus, UsageProviderSetting, UsageReport, UsageSettings,
};
use crate::config::UsageConfig;
use crate::events::AppEvent;

/// Manual refreshes closer together than this reuse the running cycle.
const MIN_MANUAL_REFRESH_GAP: Duration = Duration::from_secs(30);

/// Why a provider refresh failed. A rate limit backs the provider off.
enum FetchError {
    RateLimited,
    Failed(String),
    /// The user must set something up first; `setup` lists the steps.
    Setup {
        message: String,
        setup: String,
    },
}

/// A failed refresh as the report shows it.
struct Failure {
    message: String,
    setup: Option<String>,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            message,
            setup: None,
        }
    }
}

impl From<String> for FetchError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

enum UsageCommand {
    Refresh,
    Reconfigure(UsageConfig),
}

/// Handle to the usage polling thread. Dropping it stops the thread.
pub(crate) struct UsagePoller {
    commands: mpsc::Sender<UsageCommand>,
}

impl UsagePoller {
    pub(crate) fn spawn(
        config: UsageConfig,
        events: tokio::sync::mpsc::Sender<AppEvent>,
    ) -> std::io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("herdr-usage".into())
            .spawn(move || run(config, receiver, events))?;
        Ok(Self { commands })
    }

    pub(crate) fn refresh(&self) {
        let _ = self.commands.send(UsageCommand::Refresh);
    }

    pub(crate) fn reconfigure(&self, config: UsageConfig) {
        let _ = self.commands.send(UsageCommand::Reconfigure(config));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Provider {
    Claude,
    Codex,
    // Same vendor as Codex, so its row sits right under it.
    OpenAiApi,
    Gemini,
    DeepSeek,
    OpenRouter,
    Kimi,
}

impl Provider {
    fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Kimi => "kimi",
            Self::OpenAiApi => "openai_api",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::Kimi => "Kimi",
            Self::OpenAiApi => "OpenAI API",
        }
    }

    fn fetch(self, config: &UsageConfig) -> Result<ProviderUsage, FetchError> {
        match self {
            Self::Claude => claude::fetch(),
            Self::Codex => Ok(codex::fetch()?),
            Self::Gemini => Ok(gemini::fetch()?),
            Self::DeepSeek => deepseek::fetch(config),
            Self::OpenRouter => openrouter::fetch(config),
            Self::Kimi => kimi::fetch(config),
            Self::OpenAiApi => openai_api::fetch(config),
        }
    }

    fn enabled_in(self, config: &UsageConfig) -> bool {
        match self {
            Self::Claude => config.claude,
            Self::Codex => config.codex,
            Self::OpenAiApi => config.openai_api,
            Self::Gemini => config.gemini,
            Self::DeepSeek => config.deepseek,
            Self::OpenRouter => config.openrouter,
            Self::Kimi => config.kimi,
        }
    }

    /// Whether the credential the provider needs is in place, without
    /// contacting the provider.
    fn credential(self, config: &UsageConfig) -> &'static str {
        let keyed = match self {
            Self::Claude | Self::Codex | Self::Gemini => return "not_required",
            Self::OpenAiApi => {
                return match openai_api::admin_key(config) {
                    Ok(_) => "found",
                    Err(problem) if problem.unsafe_file => "unsafe",
                    Err(_) => "missing",
                }
            }
            Self::DeepSeek => keys::KeyedProvider::DeepSeek,
            Self::OpenRouter => keys::KeyedProvider::OpenRouter,
            Self::Kimi => keys::KeyedProvider::Kimi,
        };
        if keys::api_key(config, &keyed).is_ok() {
            "found"
        } else {
            "missing"
        }
    }

    fn warning(self) -> Option<&'static str> {
        matches!(self, Self::OpenAiApi).then_some(
            "needs an OpenAI Admin key, which can manage the whole organization; \
             herdr only reads costs, usage and the spend limit",
        )
    }

    /// Shortest gap between fetches, whatever the configured interval.
    fn min_refresh_secs(self) -> u64 {
        match self {
            // OpenAI's cost and usage buckets update with a delay, so polling
            // them every few minutes costs Admin API calls and shows nothing new.
            Self::OpenAiApi => OPENAI_API_MIN_REFRESH_SECS,
            _ => 0,
        }
    }
}

const OPENAI_API_MIN_REFRESH_SECS: u64 = 15 * 60;

/// Every provider in footer order.
const ALL_PROVIDERS: [Provider; 7] = [
    Provider::Claude,
    Provider::Codex,
    Provider::OpenAiApi,
    Provider::Gemini,
    Provider::DeepSeek,
    Provider::OpenRouter,
    Provider::Kimi,
];

/// The `[usage]` key that turns a provider on, which is also its id.
pub(crate) fn provider_config_key(id: &str) -> Option<&'static str> {
    ALL_PROVIDERS
        .into_iter()
        .map(Provider::id)
        .find(|candidate| *candidate == id)
}

/// The server's usage choices and credential state for every provider, read
/// from config and local files only, so it also works while polling is off.
pub(crate) fn settings(config: &UsageConfig) -> UsageSettings {
    UsageSettings {
        enabled: config.enabled,
        providers: ALL_PROVIDERS
            .into_iter()
            .map(|provider| UsageProviderSetting {
                provider: provider.id().to_owned(),
                label: provider.label().to_owned(),
                enabled: provider.enabled_in(config),
                credential: provider.credential(config).to_owned(),
                warning: provider.warning().map(str::to_owned),
            })
            .collect(),
    }
}

fn enabled_providers(config: &UsageConfig) -> Vec<Provider> {
    if !config.enabled {
        return Vec::new();
    }
    [
        (config.claude, Provider::Claude),
        (config.codex, Provider::Codex),
        (config.gemini, Provider::Gemini),
        (config.deepseek, Provider::DeepSeek),
        // Most setups have no OpenRouter key; skip it rather than show a failed row.
        (
            config.openrouter && keys::api_key(config, &keys::KeyedProvider::OpenRouter).is_ok(),
            Provider::OpenRouter,
        ),
        (
            config.kimi && keys::api_key(config, &keys::KeyedProvider::Kimi).is_ok(),
            Provider::Kimi,
        ),
        // Opt-in: shows an error row when the key file is missing or unsafe.
        (config.openai_api, Provider::OpenAiApi),
    ]
    .into_iter()
    .filter_map(|(enabled, provider)| enabled.then_some(provider))
    .collect()
}

fn run(
    mut config: UsageConfig,
    commands: mpsc::Receiver<UsageCommand>,
    events: tokio::sync::mpsc::Sender<AppEvent>,
) {
    let mut last = BTreeMap::<Provider, ProviderUsage>::new();
    let mut forced = false;
    loop {
        let providers = enabled_providers(&config);
        last.retain(|provider, _| providers.contains(provider));
        let cache = UsageCache::load();
        for provider in &providers {
            last.entry(*provider).or_insert_with(|| {
                cache
                    .usage(provider.id())
                    .cloned()
                    .unwrap_or_else(|| ProviderUsage::pending(provider.id(), provider.label()))
            });
        }
        if !publish(&events, &config, &last) {
            return;
        }
        let fetched_at = Instant::now();
        if !providers.is_empty() {
            for (provider, result) in refresh(&providers, &config, &cache, forced) {
                let previous = last.remove(&provider);
                last.insert(provider, merge_result(provider, previous, result));
            }
            if !publish(&events, &config, &last) {
                return;
            }
        }

        let deadline = fetched_at + config.refresh_interval();
        forced = false;
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            match commands.recv_timeout(timeout) {
                Ok(UsageCommand::Refresh) => {
                    if fetched_at.elapsed() >= MIN_MANUAL_REFRESH_GAP {
                        forced = true;
                        break;
                    }
                }
                Ok(UsageCommand::Reconfigure(next)) => {
                    let changed = next != config;
                    config = next;
                    if changed {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

/// Fetch every provider the shared cache does not answer, then record the outcomes.
fn refresh(
    providers: &[Provider],
    config: &UsageConfig,
    cache: &UsageCache,
    forced: bool,
) -> Vec<(Provider, Result<ProviderUsage, Failure>)> {
    let now = now_unix();
    let interval_secs = config.refresh_interval().as_secs();
    let fetched = std::thread::scope(|scope| {
        let handles = providers
            .iter()
            .map(|provider| {
                let plan = cache.plan(
                    provider.id(),
                    now,
                    interval_secs.max(provider.min_refresh_secs()),
                    forced,
                );
                let handle = matches!(plan, Plan::Fetch)
                    .then(|| scope.spawn(move || provider.fetch(config)));
                (*provider, plan, handle)
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(provider, plan, handle)| {
                let fetched = handle.map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err(FetchError::Failed("usage fetch panicked".into())))
                });
                (provider, plan, fetched)
            })
            .collect::<Vec<_>>()
    });

    let mut results = Vec::with_capacity(fetched.len());
    UsageCache::update(|cache| {
        for (provider, plan, fetched) in fetched {
            let result = match (plan, fetched) {
                (Plan::Blocked(retry_in), _) => {
                    Err(rate_limited_message(provider, retry_in).into())
                }
                (Plan::Fresh(usage), _) => Ok(*usage),
                (Plan::Fetch, Some(Ok(mut usage))) => {
                    usage.provider = provider.id().to_owned();
                    usage.label = provider.label().to_owned();
                    usage.status = ProviderUsageStatus::Ok;
                    usage.observed_at = Some(now);
                    cache.record_success(&usage);
                    Ok(usage)
                }
                (Plan::Fetch, Some(Err(FetchError::RateLimited))) => {
                    let retry_in = cache.record_rate_limit(provider.id(), now);
                    Err(rate_limited_message(provider, retry_in).into())
                }
                (Plan::Fetch, Some(Err(FetchError::Failed(message)))) => Err(message.into()),
                (Plan::Fetch, Some(Err(FetchError::Setup { message, setup }))) => Err(Failure {
                    message,
                    setup: Some(setup),
                }),
                (Plan::Fetch, None) => Err(String::from("usage fetch did not run").into()),
            };
            results.push((provider, result));
        }
    });
    results
}

fn rate_limited_message(provider: Provider, retry_in_secs: u64) -> String {
    let minutes = retry_in_secs.div_ceil(60).max(1);
    format!(
        "{} usage endpoint is rate limited; retrying in {minutes}m",
        provider.label()
    )
}

fn publish(
    events: &tokio::sync::mpsc::Sender<AppEvent>,
    config: &UsageConfig,
    last: &BTreeMap<Provider, ProviderUsage>,
) -> bool {
    let report = UsageReport {
        enabled: config.enabled,
        providers: last.values().cloned().collect(),
    };
    events.blocking_send(AppEvent::UsageUpdated(report)).is_ok()
}

/// Keep the last good allowance when a refresh fails so the footer does not blank out.
fn merge_result(
    provider: Provider,
    previous: Option<ProviderUsage>,
    result: Result<ProviderUsage, Failure>,
) -> ProviderUsage {
    match result {
        Ok(mut usage) => {
            usage.provider = provider.id().to_owned();
            usage.label = provider.label().to_owned();
            usage.status = ProviderUsageStatus::Ok;
            usage.observed_at = usage.observed_at.or_else(|| Some(now_unix()));
            usage
        }
        Err(Failure { message, setup }) => {
            tracing::debug!(provider = provider.id(), %message, "usage refresh failed");
            let mut usage =
                previous.unwrap_or_else(|| ProviderUsage::pending(provider.id(), provider.label()));
            usage.status = ProviderUsageStatus::Error;
            usage.message = Some(message);
            usage.setup = setup;
            usage
        }
    }
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn clamp_percent(value: f64) -> u8 {
    if value.is_finite() {
        value.round().clamp(0.0, 100.0) as u8
    } else {
        0
    }
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map_or_else(|| std::path::PathBuf::from(path), |home| home.join(rest)),
        None => std::path::PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::UsageWindow;

    fn window(used_percent: u8) -> UsageWindow {
        UsageWindow {
            id: "five_hour".into(),
            label: "5h".into(),
            used_percent,
            resets_at: Some(10),
        }
    }

    #[test]
    fn failed_refresh_keeps_last_good_allowance() {
        let mut good = ProviderUsage::pending("claude", "Claude");
        good.status = ProviderUsageStatus::Ok;
        good.windows.push(window(40));
        good.observed_at = Some(5);

        let merged = merge_result(
            Provider::Claude,
            Some(good),
            Err(String::from("offline").into()),
        );

        assert_eq!(merged.status, ProviderUsageStatus::Error);
        assert_eq!(merged.message.as_deref(), Some("offline"));
        assert_eq!(merged.windows, vec![window(40)]);
        assert_eq!(merged.observed_at, Some(5));
    }

    #[test]
    fn successful_refresh_is_stamped_and_labeled() {
        let mut fresh = ProviderUsage::pending("", "");
        fresh.windows.push(window(10));

        let merged = merge_result(Provider::Codex, None, Ok(fresh));

        assert_eq!(merged.provider, "codex");
        assert_eq!(merged.label, "Codex");
        assert_eq!(merged.status, ProviderUsageStatus::Ok);
        assert!(merged.observed_at.is_some());
    }

    #[test]
    fn disabled_config_selects_no_providers() {
        let mut config = UsageConfig {
            enabled: false,
            ..UsageConfig::default()
        };
        assert!(enabled_providers(&config).is_empty());
        config.enabled = true;
        config.codex = false;
        config.gemini = false;
        config.openrouter = false;
        config.kimi = false;
        assert_eq!(
            enabled_providers(&config)
                .into_iter()
                .map(Provider::id)
                .collect::<Vec<_>>(),
            vec!["claude", "deepseek"]
        );
    }

    #[test]
    fn settings_list_every_provider_with_its_choice_and_key_state() {
        let config = UsageConfig {
            openai_api: true,
            openai_admin_key_file: "/nonexistent/herdr-openai-admin-key".into(),
            kimi: false,
            ..UsageConfig::default()
        };
        let settings = settings(&config);
        assert!(settings.enabled);
        assert_eq!(
            settings
                .providers
                .iter()
                .map(|provider| provider.provider.as_str())
                .collect::<Vec<_>>(),
            vec![
                "claude",
                "codex",
                "openai_api",
                "gemini",
                "deepseek",
                "openrouter",
                "kimi"
            ]
        );
        let openai = &settings.providers[2];
        assert!(openai.enabled);
        assert_eq!(openai.credential, "missing");
        assert!(openai.warning.is_some());
        assert_eq!(settings.providers[0].credential, "not_required");
        assert!(!settings.providers[6].enabled);
    }

    #[test]
    fn only_known_providers_have_a_config_key() {
        assert_eq!(provider_config_key("openai_api"), Some("openai_api"));
        assert_eq!(provider_config_key("auth_file"), None);
        assert_eq!(provider_config_key("enabled"), None);
    }

    #[test]
    fn a_setup_failure_keeps_its_steps_in_the_report() {
        let merged = merge_result(
            Provider::OpenAiApi,
            None,
            Err(Failure {
                message: "no key".into(),
                setup: Some("step one\nstep two".into()),
            }),
        );
        assert_eq!(merged.status, ProviderUsageStatus::Error);
        assert_eq!(merged.setup.as_deref(), Some("step one\nstep two"));
    }

    #[test]
    fn percent_is_rounded_and_clamped() {
        assert_eq!(clamp_percent(31.6), 32);
        assert_eq!(clamp_percent(140.0), 100);
        assert_eq!(clamp_percent(-3.0), 0);
        assert_eq!(clamp_percent(f64::NAN), 0);
    }
}
