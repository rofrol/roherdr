use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct PingParams {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerLiveHandoffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
    /// Hand off even while headless worker processes are alive; they get
    /// SIGTERM and end with this server. Worker pipes are not handed over,
    /// so without this any live worker refuses the handoff.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerSshAgentRegisterParams {
    /// Absolute remote-host agent socket. Registration lasts until this API connection closes.
    pub socket_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerCapabilities {
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
    /// Stable client-owned endpoint generation supported by this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_protocol_generation: Option<u32>,
    /// Whether this server supports explicit client-shell surface interest.
    #[serde(default)]
    pub surface_interest: bool,
    /// Whether this server supports endpoint health probes.
    #[serde(default)]
    pub health_check: bool,
    /// Supports connection-scoped `server.ssh_agent.register` on the local JSON API.
    #[serde(default)]
    pub ssh_agent_registration: bool,
}

/// System-wide pseudo-terminal usage, sampled at most once a second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SystemPtyUsageInfo {
    /// Pseudo-terminals open on the whole system, not only Herdr's panes.
    pub in_use: u32,
    /// The kernel limit (`kern.tty.ptmx_max` on macOS, `kernel.pty.max` on Linux).
    pub max: u32,
    /// Free pseudo-terminals a new pane must leave; below this Herdr refuses
    /// the spawn with `pty_exhausted`.
    pub required_free: u32,
}
