use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageReadParams {
    /// Ask the server to start a refresh now; the response still returns cached values.
    #[serde(default)]
    pub refresh: bool,
}

/// Latest subscription/API allowance observed for each configured provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageReport {
    /// False when usage polling is disabled in the server config.
    pub enabled: bool,
    pub providers: Vec<ProviderUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProviderUsage {
    /// Stable provider id such as `claude`, `codex`, `gemini`, `deepseek`, or `openrouter`.
    pub provider: String,
    /// Human-readable provider name.
    pub label: String,
    pub status: ProviderUsageStatus,
    /// Error or hint for a status other than `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Unix seconds of the last successful observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<u64>,
    /// Rate-limit windows in provider order.
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    /// Prepaid balances, one per currency.
    #[serde(default)]
    pub balances: Vec<UsageBalance>,
    /// Extra provider-specific facts for detail views.
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageStatus {
    /// No observation has completed yet.
    Pending,
    Ok,
    /// The latest refresh failed; windows and balances keep the last good values.
    Error,
    /// A future status this client does not understand.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageWindow {
    /// Stable window id such as `five_hour` or `weekly`.
    pub id: String,
    /// Short label such as `5h` or `week`.
    pub label: String,
    /// Consumed share of the window allowance, 0-100.
    pub used_percent: u8,
    /// Unix seconds when the window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageBalance {
    pub currency: String,
    /// Decimal amount as reported by the provider.
    pub total: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topped_up: Option<String>,
}

impl ProviderUsage {
    pub fn pending(provider: &str, label: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            label: label.to_owned(),
            status: ProviderUsageStatus::Pending,
            message: None,
            plan: None,
            observed_at: None,
            windows: Vec::new(),
            balances: Vec::new(),
            notes: Vec::new(),
        }
    }
}
