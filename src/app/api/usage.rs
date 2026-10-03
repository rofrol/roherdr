use crate::api::schema::{ResponseResult, UsageReadParams};
use crate::app::App;

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_usage_read(&self, id: String, params: UsageReadParams) -> String {
        if params.refresh {
            if let Some(poller) = self.usage_poller.as_ref() {
                poller.refresh();
            }
        }
        encode_success(
            id,
            ResponseResult::UsageRead {
                usage: self.state.usage.clone(),
            },
        )
    }

    pub(super) fn handle_usage_settings(&self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::UsageSettings {
                settings: crate::usage::settings(&self.usage_config),
            },
        )
    }

    /// Writes one `[usage]` key into this server's own config, which may be
    /// on another machine than the client, then reloads it.
    pub(super) fn handle_usage_set(
        &mut self,
        id: String,
        key: &'static str,
        value: bool,
    ) -> String {
        if let Err(error) =
            crate::config::write_edit(crate::config::ConfigEdit::UsageBool(key, value))
        {
            return encode_error(id, "config_write_failed", error);
        }
        let report = self.reload_config();
        if report.status == crate::config::ConfigReloadStatus::Failed {
            return encode_error(id, "config_reload_failed", report.diagnostics.join("; "));
        }
        self.handle_usage_settings(id)
    }
}
