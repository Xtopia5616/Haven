//! Agent-owned ports for tool observations.

use async_trait::async_trait;
use haven_common::types::MessageAttachment;
use haven_tools::{ToolResult, ToolsManager};
use std::sync::Arc;

/// Formats the bounded observation text for a completed tool result.
#[async_trait]
pub(super) trait ToolObservationPort: Send + Sync {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String;
}

/// Production adapter delegating observation formatting to the shared manager.
pub(super) struct ToolsManagerToolObservationAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerToolObservationAdapter {
    pub(super) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl ToolObservationPort for ToolsManagerToolObservationAdapter {
    async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.tools.observation_text(tool_name, result).await
    }
}

/// Agent-owned boundary for registering and releasing session asset leases.
pub(super) trait ManagedAssetLeasePort: Send + Sync {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]);
    fn release_for_session(&self, session_id: &str);
}

/// Adapter that keeps managed-asset path validation and registry ownership in
/// `ToolsManager` while exposing only session lease operations to the agent.
pub(super) struct ToolsManagerManagedAssetLeaseAdapter {
    tools: Arc<ToolsManager>,
}

impl ToolsManagerManagedAssetLeaseAdapter {
    pub(super) fn new(tools: Arc<ToolsManager>) -> Self {
        Self { tools }
    }
}

impl ManagedAssetLeasePort for ToolsManagerManagedAssetLeaseAdapter {
    fn register_for_session(&self, session_id: &str, attachments: &[MessageAttachment]) {
        self.tools
            .register_managed_assets_for_session(session_id, attachments);
    }

    fn release_for_session(&self, session_id: &str) {
        self.tools.release_managed_assets_for_session(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::ToolConfig;
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;

    #[tokio::test]
    async fn manager_adapter_forwards_tool_name_and_result_to_formatter() {
        let tools = Arc::new(ToolsManager::new());
        let mut settings = HashMap::new();
        settings.insert(
            "probe.operation".into(),
            ToolConfig {
                max_output_chars: Some(4),
                ..ToolConfig::default()
            },
        );
        tools.set_tool_settings(settings).await;
        let result = ToolResult::ok(json!("012345"));
        let adapter = ToolsManagerToolObservationAdapter::new(Arc::clone(&tools));

        assert_eq!(
            adapter.observation_text("probe.operation", &result).await,
            "0123"
        );
    }

    #[test]
    fn managed_asset_adapter_releases_only_the_requested_session_lease() {
        let tools = Arc::new(ToolsManager::new());
        let assets = tools.share_services().assets;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("asset.png");
        fs::write(&path, b"asset").unwrap();

        assert!(assets.register_under_root_for_session(
            "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            directory.path(),
            "asset-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            path,
            Some("asset.png".into()),
            "image/png",
        ));
        assert!(assets.lease_for_session(
            "ses-cccccccccccccccccccccccccccccccc",
            "asset-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        ));

        let adapter = ToolsManagerManagedAssetLeaseAdapter::new(tools);
        adapter.release_for_session("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        assert_eq!(
            assets.release_session("ses-cccccccccccccccccccccccccccccccc"),
            1,
            "releasing one session must leave another session's lease intact"
        );
        assert_eq!(
            assets.release_session("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            0,
            "the adapter must release the requested session lease"
        );
    }
}
