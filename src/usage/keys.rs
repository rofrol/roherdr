//! API keys for providers billed by key: an environment variable or the
//! shared auth file (pi's `auth.json` layout).
//!
//! The auth file maps provider ids to entries, either
//! `{"type": "api_key", "key": "..."}` or an OAuth entry whose `access` token
//! works as a bearer key (OpenRouter's login stores its API key there).

use crate::config::UsageConfig;

/// Default auth file, shared with the pi coding agent.
pub(crate) const DEFAULT_AUTH_FILE: &str = "~/.pi/agent/auth.json";

pub(super) enum KeyedProvider {
    DeepSeek,
    OpenRouter,
}

impl KeyedProvider {
    fn id(&self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
        }
    }

    fn env_var(&self) -> &'static str {
        match self {
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
        }
    }
}

/// First key found in the environment, then the auth file.
pub(super) fn api_key(config: &UsageConfig, provider: &KeyedProvider) -> Result<String, String> {
    if let Some(key) = std::env::var(provider.env_var()).ok().and_then(non_empty) {
        return Ok(key);
    }
    let auth_file = config.auth_file.trim();
    if !auth_file.is_empty() {
        let path = super::expand_home(auth_file);
        if let Some(key) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| auth_file_key(&raw, provider.id()))
        {
            return Ok(key);
        }
    }
    Err(format!(
        "no {} key: set {} or add \"{}\" to usage.auth_file",
        provider.id(),
        provider.env_var(),
        provider.id()
    ))
}

fn auth_file_key(raw: &str, provider: &str) -> Option<String> {
    let entries: serde_json::Value = serde_json::from_str(raw).ok()?;
    let entry = entries.get(provider)?;
    ["key", "access"]
        .into_iter()
        .find_map(|field| entry.get(field)?.as_str())
        .and_then(|key| non_empty(key.to_owned()))
}

fn non_empty(key: String) -> Option<String> {
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTH: &str = r#"{
        "deepseek": {"type": "api_key", "key": " sk-ds \n"},
        "openrouter": {"type": "oauth", "access": "sk-or", "refresh": "", "expires": 1},
        "empty": {"type": "api_key", "key": ""}
    }"#;

    #[test]
    fn auth_file_reads_api_keys_and_oauth_access_tokens() {
        assert_eq!(auth_file_key(AUTH, "deepseek").as_deref(), Some("sk-ds"));
        assert_eq!(auth_file_key(AUTH, "openrouter").as_deref(), Some("sk-or"));
    }

    #[test]
    fn auth_file_skips_missing_and_empty_entries() {
        assert_eq!(auth_file_key(AUTH, "anthropic"), None);
        assert_eq!(auth_file_key(AUTH, "empty"), None);
        assert_eq!(auth_file_key("not json", "deepseek"), None);
    }
}
