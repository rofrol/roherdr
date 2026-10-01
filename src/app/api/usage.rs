use crate::api::schema::{ResponseResult, UsageReadParams};
use crate::app::App;

use super::responses::encode_success;

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
}
