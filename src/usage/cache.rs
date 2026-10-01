//! Usage observations shared by every herdr instance through one JSON file.
//!
//! Provider endpoints rate-limit per credential, and stable and dev builds poll
//! the same logins. A fresh observation from another instance is reused instead
//! of refetched, a rate-limited provider backs off for all instances, and a
//! restarted server starts from the last good allowance instead of a blank row.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::schema::ProviderUsage;

const CACHE_FILE: &str = "usage-cache.json";
const BACKOFF_BASE_SECS: u64 = 300;
const BACKOFF_MAX_SECS: u64 = 3600;
/// Scheduled refreshes reuse an observation this much younger than the interval,
/// so instances polling on the same interval do not both hit the endpoint.
const FRESHNESS_SLACK_SECS: u64 = 15;

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct UsageCache {
    #[serde(default)]
    providers: BTreeMap<String, Entry>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// Last good observation, stamped with `observed_at`.
    #[serde(default)]
    usage: Option<ProviderUsage>,
    /// Unix seconds before which no instance should call the endpoint.
    #[serde(default)]
    blocked_until: Option<u64>,
    /// Consecutive rate-limited responses, driving exponential backoff.
    #[serde(default)]
    rate_limited: u32,
}

/// What a refresh cycle should do for one provider.
#[derive(Debug, PartialEq)]
pub(super) enum Plan {
    /// Backing off after a rate limit; seconds until the next attempt.
    Blocked(u64),
    /// Another instance observed it recently enough.
    Fresh(ProviderUsage),
    Fetch,
}

impl UsageCache {
    pub(super) fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Reload, apply `update`, and store, so writes from other instances survive.
    pub(super) fn update(update: impl FnOnce(&mut Self)) {
        let mut cache = Self::load();
        update(&mut cache);
        cache.store();
    }

    pub(super) fn usage(&self, provider: &str) -> Option<&ProviderUsage> {
        self.providers.get(provider)?.usage.as_ref()
    }

    /// `forced` (a manual refresh) skips the freshness reuse but never the backoff.
    pub(super) fn plan(&self, provider: &str, now: u64, interval_secs: u64, forced: bool) -> Plan {
        let Some(entry) = self.providers.get(provider) else {
            return Plan::Fetch;
        };
        if let Some(blocked_until) = entry.blocked_until.filter(|until| *until > now) {
            return Plan::Blocked(blocked_until - now);
        }
        let fresh_after = now.saturating_sub(interval_secs.saturating_sub(FRESHNESS_SLACK_SECS));
        match &entry.usage {
            Some(usage) if !forced && usage.observed_at.is_some_and(|at| at > fresh_after) => {
                Plan::Fresh(usage.clone())
            }
            _ => Plan::Fetch,
        }
    }

    pub(super) fn record_success(&mut self, usage: &ProviderUsage) {
        self.providers.insert(
            usage.provider.clone(),
            Entry {
                usage: Some(usage.clone()),
                ..Entry::default()
            },
        );
    }

    /// Returns the backoff in seconds before the provider may be called again.
    pub(super) fn record_rate_limit(&mut self, provider: &str, now: u64) -> u64 {
        let entry = self.providers.entry(provider.to_owned()).or_default();
        entry.rate_limited = entry.rate_limited.saturating_add(1);
        let backoff = backoff_secs(entry.rate_limited);
        entry.blocked_until = Some(now + backoff);
        backoff
    }

    fn store(&self) {
        let path = path();
        let Ok(raw) = serde_json::to_string(self) else {
            return;
        };
        let temp = path.with_extension(format!("json.{}", std::process::id()));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temp, raw))
            .and_then(|()| std::fs::rename(&temp, &path));
        if let Err(error) = written {
            tracing::debug!(%error, "failed to store usage cache");
            let _ = std::fs::remove_file(&temp);
        }
    }
}

fn backoff_secs(consecutive: u32) -> u64 {
    let doublings = consecutive.saturating_sub(1).min(8);
    (BACKOFF_BASE_SECS << doublings).min(BACKOFF_MAX_SECS)
}

/// The stable app's state directory, shared by stable and dev builds.
fn path() -> PathBuf {
    crate::config::state_dir()
        .with_file_name("herdr")
        .join(CACHE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(provider: &str, at: u64) -> ProviderUsage {
        let mut usage = ProviderUsage::pending(provider, provider);
        usage.observed_at = Some(at);
        usage
    }

    #[test]
    fn unknown_provider_is_fetched() {
        assert_eq!(
            UsageCache::default().plan("claude", 1000, 300, false),
            Plan::Fetch
        );
    }

    #[test]
    fn recent_observation_is_reused_unless_forced() {
        let mut cache = UsageCache::default();
        cache.record_success(&observed("claude", 1000));

        assert_eq!(
            cache.plan("claude", 1200, 300, false),
            Plan::Fresh(observed("claude", 1000))
        );
        assert_eq!(cache.plan("claude", 1200, 300, true), Plan::Fetch);
        assert_eq!(cache.plan("claude", 1290, 300, false), Plan::Fetch);
    }

    #[test]
    fn rate_limit_blocks_even_forced_refreshes() {
        let mut cache = UsageCache::default();
        cache.record_success(&observed("claude", 900));

        assert_eq!(cache.record_rate_limit("claude", 1000), 300);
        assert_eq!(cache.plan("claude", 1100, 300, true), Plan::Blocked(200));
        assert_eq!(cache.plan("claude", 1300, 300, false), Plan::Fetch);
        assert_eq!(cache.usage("claude"), Some(&observed("claude", 900)));

        assert_eq!(cache.record_rate_limit("claude", 1300), 600);
        cache.record_success(&observed("claude", 2000));
        assert_eq!(cache.record_rate_limit("claude", 2100), 300);
    }

    #[test]
    fn rate_limit_backoff_doubles_up_to_an_hour() {
        assert_eq!(backoff_secs(1), 300);
        assert_eq!(backoff_secs(2), 600);
        assert_eq!(backoff_secs(3), 1200);
        assert_eq!(backoff_secs(4), 2400);
        assert_eq!(backoff_secs(5), 3600);
        assert_eq!(backoff_secs(u32::MAX), 3600);
    }

    #[test]
    fn cache_file_round_trips() {
        let mut cache = UsageCache::default();
        cache.record_success(&observed("deepseek", 5));
        cache.record_rate_limit("claude", 10);
        let raw = serde_json::to_string(&cache).unwrap();
        assert_eq!(serde_json::from_str::<UsageCache>(&raw).unwrap(), cache);
    }
}
