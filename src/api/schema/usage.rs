use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageReadParams {
    /// Ask the server to start a refresh now; the response still returns cached values.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageSetEnabledParams {
    /// Whether the server polls providers at all.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageSetProviderParams {
    /// Provider id from `usage.settings`, such as `claude` or `openai_api`.
    pub provider: String,
    pub enabled: bool,
}

/// The server's own `[usage]` choices and whether each provider can run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageSettings {
    pub enabled: bool,
    pub providers: Vec<UsageProviderSetting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageProviderSetting {
    pub provider: String,
    pub label: String,
    pub enabled: bool,
    /// `not_required`, `found`, `missing` or `unsafe`; clients treat other
    /// values as unknown.
    pub credential: String,
    /// Why turning it on needs care, such as an organization-wide key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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
    /// Unredeemed one-time allowance resets, redeemed in the provider's own tool.
    /// Clients hide credits whose `expires_at` has passed, since reports can be cached.
    #[serde(default)]
    pub reset_credits: Vec<UsageResetCredit>,
    /// Pay-as-you-go spend since `since`, one per currency. Not a prepaid balance.
    #[serde(default)]
    pub spend: Vec<UsageSpend>,
    /// Text-completion tokens over the same period as `spend`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<UsageCompletionTokens>,
    /// What the user must do before the provider can be read, one step per
    /// line. Set together with `status: error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
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
pub struct UsageResetCredit {
    /// Provider id of the credit.
    pub id: String,
    /// Provider's kind of reset, such as `codexRateLimits`.
    pub kind: String,
    /// Provider's title, such as `Full reset (Weekly + 5 hr)`.
    pub title: String,
    /// Unix seconds when the unused credit expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageSpend {
    pub currency: String,
    /// Decimal amount spent since `since`.
    pub amount: String,
    /// Unix seconds where the reported period starts.
    pub since: u64,
    /// Spend limit for the same period and currency, as set at the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    /// The provider is rejecting requests because the limit was reached.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub limit_enforcing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UsageCompletionTokens {
    /// Input tokens, cached ones included.
    pub input: u64,
    /// The part of `input` served from the prompt cache.
    pub cached_input: u64,
    pub output: u64,
    pub requests: u64,
    /// Unix seconds where the reported period starts.
    pub since: u64,
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
            reset_credits: Vec::new(),
            spend: Vec::new(),
            completion_tokens: None,
            setup: None,
        }
    }
}
