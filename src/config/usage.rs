use serde::Deserialize;

/// `[usage]`: provider allowance polling shown in the sidebar usage footer.
///
/// On by default; set `enabled = false` to stop contacting provider services.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UsageConfig {
    /// Poll provider usage and show the sidebar usage footer.
    pub enabled: bool,
    /// Seconds between background refreshes. Values below 60 are raised to 60.
    pub refresh_interval_secs: u64,
    /// Read Claude subscription limits with the Claude Code login.
    pub claude: bool,
    /// Read Codex rate limits through the local `codex app-server`.
    pub codex: bool,
    /// Read Gemini weekly limits through the Antigravity CLI (`agy /quota`).
    pub gemini: bool,
    /// Read the DeepSeek prepaid API balance.
    pub deepseek: bool,
    /// JSON file mapping provider ids to API keys, in pi's `auth.json` layout.
    /// Used when the provider's environment variable is not set. An empty
    /// string disables it.
    pub auth_file: String,
    /// Read OpenRouter credits when an API key is found.
    pub openrouter: bool,
}

pub(crate) const MIN_USAGE_REFRESH_INTERVAL_SECS: u64 = 60;

impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_interval_secs: 300,
            claude: true,
            codex: true,
            gemini: true,
            deepseek: true,
            auth_file: crate::usage::DEFAULT_AUTH_FILE.to_owned(),
            openrouter: true,
        }
    }
}

impl UsageConfig {
    pub(crate) fn refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.refresh_interval_secs
                .max(MIN_USAGE_REFRESH_INTERVAL_SECS),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_config_defaults_to_enabled_with_all_providers_selected() {
        let config: UsageConfig = toml::from_str("").unwrap();
        assert_eq!(config, UsageConfig::default());
        assert!(config.enabled);
        assert!(config.claude && config.codex && config.gemini && config.deepseek);
    }

    #[test]
    fn usage_refresh_interval_has_a_floor() {
        let config: UsageConfig = toml::from_str("refresh_interval_secs = 5").unwrap();
        assert_eq!(config.refresh_interval().as_secs(), 60);
    }
}
