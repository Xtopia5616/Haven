//! Agent-owned ports for tool observations.

use async_trait::async_trait;
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

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::ToolConfig;
    use serde_json::json;
    use std::collections::HashMap;

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
}
