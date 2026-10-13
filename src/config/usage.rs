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
    /// Read the Kimi (Moonshot) prepaid balance when an API key is found.
    pub kimi: bool,
    /// Moonshot API host: `api.moonshot.ai` (USD) or `api.moonshot.cn`
    /// (CNY, a separate account).
    pub kimi_host: String,
    /// Read OpenAI API month-to-date spend and completion tokens through the
    /// Admin API. Off by default: an admin key reads organization-wide billing.
    pub openai_api: bool,
    /// File holding the OpenAI Admin key, readable only by its owner. No
    /// environment variable is read, so the key never reaches agent panes.
    pub openai_admin_key_file: String,
    /// Read Anthropic API credits spent since the grant through the Admin
    /// API cost report. Off by default: an admin key reads organization-wide
    /// billing.
    pub anthropic_api: bool,
    /// File holding the Anthropic Admin key, readable only by its owner. No
    /// environment variable is read, so the key never reaches agent panes.
    pub anthropic_admin_key_file: String,
    /// The organization's Anthropic credit grants, as the Console's billing
    /// page lists them. Billed cost is drawn from them day by day.
    pub anthropic_credits: Vec<AnthropicCreditGrant>,
    /// `herdr todo run`'s usage gate closes when a window of the run's
    /// provider is at least this many percent used (1-100).
    pub gate_close_percent: u8,
    /// A high usage latch reopens once every window is below this many
    /// percent (1-100, below `gate_close_percent`).
    pub gate_reopen_percent: u8,
}

/// `[[usage.anthropic_credits]]`: one credit grant of the Anthropic Console
/// organization.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicCreditGrant {
    /// The granted amount in whole US dollars.
    pub usd: u32,
    /// UTC day (YYYY-MM-DD) the grant arrived ("Date granted").
    pub granted: String,
    /// UTC day (YYYY-MM-DD) it expires ("Expires on"); from that day on it
    /// is gone. Empty: it does not expire.
    #[serde(default)]
    pub expires: String,
    /// A plan's included API credits (the Max plan's monthly grant), which
    /// Anthropic spends before any other credit. The widget shows this grant
    /// as spent/granted and the others' remaining balance next to it.
    #[serde(default)]
    pub included: bool,
}

pub(crate) const MIN_USAGE_REFRESH_INTERVAL_SECS: u64 = 60;
const DEFAULT_GATE_CLOSE_PERCENT: u8 = 90;
const DEFAULT_GATE_REOPEN_PERCENT: u8 = 80;

/// The usage gate's thresholds, checked: `reopen_below` < `close_at`, both
/// within 1-100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsageGateThresholds {
    /// A window at or above this many percent used closes the gate.
    pub(crate) close_at: u8,
    /// A closed gate reopens once every window is below this.
    pub(crate) reopen_below: u8,
}

impl Default for UsageGateThresholds {
    fn default() -> Self {
        Self {
            close_at: DEFAULT_GATE_CLOSE_PERCENT,
            reopen_below: DEFAULT_GATE_REOPEN_PERCENT,
        }
    }
}

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
            kimi: true,
            kimi_host: "api.moonshot.ai".to_owned(),
            openai_api: false,
            openai_admin_key_file: "~/.config/herdr/openai-admin-key".to_owned(),
            anthropic_api: false,
            anthropic_admin_key_file: "~/.config/herdr/anthropic-admin-key".to_owned(),
            anthropic_credits: Vec::new(),
            gate_close_percent: DEFAULT_GATE_CLOSE_PERCENT,
            gate_reopen_percent: DEFAULT_GATE_REOPEN_PERCENT,
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

    /// `gate_close_percent` and `gate_reopen_percent` when both are within
    /// 1-100 and reopen is below close; otherwise why not.
    pub(crate) fn usage_gate_thresholds(&self) -> Result<UsageGateThresholds, String> {
        let (close, reopen) = (self.gate_close_percent, self.gate_reopen_percent);
        if !(1..=100).contains(&close) || !(1..=100).contains(&reopen) {
            return Err(format!(
                "usage.gate_close_percent ({close}) and usage.gate_reopen_percent ({reopen}) \
                 must be within 1-100"
            ));
        }
        if reopen >= close {
            return Err(format!(
                "usage.gate_reopen_percent ({reopen}) must be below \
                 usage.gate_close_percent ({close})"
            ));
        }
        Ok(UsageGateThresholds {
            close_at: close,
            reopen_below: reopen,
        })
    }

    pub(crate) fn diagnostics(&self) -> Option<String> {
        self.usage_gate_thresholds().err().map(|why| {
            format!(
                "{why}; the usage gate uses {DEFAULT_GATE_CLOSE_PERCENT} and \
                     {DEFAULT_GATE_REOPEN_PERCENT}"
            )
        })
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
        assert!(!config.openai_api);
        assert!(!config.anthropic_api);
        assert_eq!(
            config.usage_gate_thresholds(),
            Ok(UsageGateThresholds {
                close_at: 90,
                reopen_below: 80
            })
        );
        assert_eq!(config.diagnostics(), None);
    }

    #[test]
    fn usage_gate_thresholds_are_validated() {
        let config: UsageConfig =
            toml::from_str("gate_close_percent = 95\ngate_reopen_percent = 70").unwrap();
        assert_eq!(
            config.usage_gate_thresholds(),
            Ok(UsageGateThresholds {
                close_at: 95,
                reopen_below: 70
            })
        );
        for (close, reopen) in [(80, 80), (80, 90), (0, 0), (101, 80), (90, 0), (100, 99)] {
            let config = UsageConfig {
                gate_close_percent: close,
                gate_reopen_percent: reopen,
                ..UsageConfig::default()
            };
            let valid = (1..=100).contains(&close) && (1..=100).contains(&reopen) && reopen < close;
            assert_eq!(
                config.usage_gate_thresholds().is_ok(),
                valid,
                "{close} {reopen}"
            );
            assert_eq!(config.diagnostics().is_none(), valid, "{close} {reopen}");
        }
    }

    #[test]
    fn usage_refresh_interval_has_a_floor() {
        let config: UsageConfig = toml::from_str("refresh_interval_secs = 5").unwrap();
        assert_eq!(config.refresh_interval().as_secs(), 60);
    }
}
