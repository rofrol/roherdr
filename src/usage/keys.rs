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
    Kimi,
}

impl KeyedProvider {
    fn id(&self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Kimi => "kimi",
        }
    }

    fn env_var(&self) -> &'static str {
        match self {
            Self::DeepSeek => "DEEPSEEK_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            Self::Kimi => "MOONSHOT_API_KEY",
        }
    }
}

/// An API key and where it was found. Only `source` may leave this module's
/// callers: it names the variable or file, never the key.
pub(super) struct ApiKey {
    pub(super) secret: String,
    pub(super) source: String,
}

impl ApiKey {
    pub(super) fn account(&self) -> crate::api::schema::UsageAccount {
        crate::api::schema::UsageAccount {
            source: self.source.clone(),
            ..Default::default()
        }
    }
}

/// First key found in the environment, then the auth file.
pub(super) fn api_key(config: &UsageConfig, provider: &KeyedProvider) -> Result<ApiKey, String> {
    api_key_from(std::env::var(provider.env_var()).ok(), config, provider)
}

/// [`api_key`] with the environment variable's value given.
fn api_key_from(
    env_value: Option<String>,
    config: &UsageConfig,
    provider: &KeyedProvider,
) -> Result<ApiKey, String> {
    if let Some(secret) = env_value.and_then(non_empty) {
        return Ok(ApiKey {
            secret,
            source: format!("env:{}", provider.env_var()),
        });
    }
    let auth_file = config.auth_file.trim();
    if !auth_file.is_empty() {
        let path = super::expand_home(auth_file);
        if let Some(secret) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| auth_file_key(&raw, provider.id()))
        {
            return Ok(ApiKey {
                secret,
                source: format!("file:{}", path.display()),
            });
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
    fn the_account_names_where_the_key_was_found_never_the_key() {
        let dir = std::env::temp_dir().join(format!("herdr-keys-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let auth_file = dir.join("auth.json");
        std::fs::write(&auth_file, AUTH).unwrap();
        let config = UsageConfig {
            auth_file: auth_file.display().to_string(),
            ..UsageConfig::default()
        };

        let from_env = api_key_from(
            Some("sk-env-secret".into()),
            &config,
            &KeyedProvider::DeepSeek,
        )
        .unwrap();
        assert_eq!(from_env.secret, "sk-env-secret");
        assert_eq!(from_env.account().source, "env:DEEPSEEK_API_KEY");

        let from_file = api_key_from(None, &config, &KeyedProvider::OpenRouter).unwrap();
        assert_eq!(from_file.secret, "sk-or");
        assert_eq!(
            from_file.account().source,
            format!("file:{}", auth_file.display())
        );

        for (key, secret) in [(from_env, "sk-env-secret"), (from_file, "sk-or")] {
            let json = serde_json::to_string(&key.account()).unwrap();
            assert!(!json.contains(secret), "{json}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_file_skips_missing_and_empty_entries() {
        assert_eq!(auth_file_key(AUTH, "anthropic"), None);
        assert_eq!(auth_file_key(AUTH, "empty"), None);
        assert_eq!(auth_file_key("not json", "deepseek"), None);
    }
}
