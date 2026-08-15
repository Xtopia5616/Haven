use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundActions;
use crate::{Tool, ToolResult};

/// One-call board of every background action started by the current session
/// (action_id, status, timestamps, output preview), so the agent can inspect all
/// background work at once instead of polling `status` action by action.
///
/// The owning session id is injected privately by the tools manager
/// (`_session_id`), mirroring the `scheduled_action` tool, so a action board can never
/// leak other sessions' actions or outputs.
pub struct ActionsTool {
    pub actions: Arc<BackgroundActions>,
}

#[async_trait]
impl Tool for ActionsTool {
    fn name(&self) -> String {
        "actions".into()
    }
    fn description(&self) -> String {
        "List all background actions of the current session in one call: action_id, status, timestamps and a brief output preview. Use this instead of polling status action by action."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    /// Needs the private `_session_id` input so the action board is scoped to the
    /// current session.
    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["running", "completed", "failed", "cancelled"],
                    "description": "Optional filter: only list actions in this state"
                }
            }
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let session_id = input["_session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("actions requires a session context"))?;
        let filter = input["status"].as_str().map(|s| s.to_string());
        let mut rows = self.actions.list_for_session(session_id).await;
        if let Some(f) = filter.as_deref() {
            rows.retain(|r| r["status"].as_str() == Some(f));
        }
        Ok(ToolResult::ok(serde_json::json!({ "actions": rows })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_actions_tool_name() {
        assert_eq!(
            ActionsTool {
                actions: Arc::new(BackgroundActions::new())
            }
            .name(),
            "actions"
        );
    }

    #[test]
    fn test_actions_tool_risk_level() {
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_actions_tool_schema() {
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let schema = tool.input_schema();
        let filter = &schema["properties"]["status"]["enum"];
        assert!(filter.is_array());
    }

    #[tokio::test]
    async fn test_actions_tool_requires_session_context() {
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(result.is_err(), "actions without a session context must fail");
    }

    #[tokio::test]
    async fn test_actions_tool_lists_session_actions() {
        let actions = Arc::new(BackgroundActions::new());
        let tool = ActionsTool { actions };
        let result = tool
            .execute(json!({"_session_id": "ses-x"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["actions"], json!([]));
    }

    #[tokio::test]
    async fn test_actions_tool_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool.execute(json!({"_session_id": "ses-x"}), cancel).await;
        assert!(result.is_err());
    }
}
