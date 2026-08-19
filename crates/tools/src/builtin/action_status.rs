use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundActions;
use crate::{Tool, ToolResult};

/// Report the status of a background action (spawned with `shell` +
/// `background: true`). The agent polls this tool with a action_id until the
/// result is ready, instead of blocking the ReAct loop on a long command.
pub struct ActionStatusTool {
    pub actions: Arc<BackgroundActions>,
}

/// Typed parameters for `ActionStatusTool`. Entry ① (native `run`) and entry
/// ② (`Tool::execute` with LLM JSON) both land in `ActionStatusTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ActionStatusParams {
    /// The action id returned by a shell(background: true) call.
    pub action_id: String,
}

impl ActionStatusTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ActionStatusParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let status = self.actions.status(&params.action_id).await;
        Ok(ToolResult::ok(status))
    }
}

#[async_trait]
impl Tool for ActionStatusTool {
    fn name(&self) -> String {
        "action_status".into()
    }
    fn description(&self) -> String {
        "Check a single background action's status by action_id. Results are also pushed back automatically on completion — for an overview of all actions use `actions` instead of polling one by one."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action_id": {
                    "type": "string",
                    "description": "The action id returned by a shell(background: true) call"
                }
            },
            "required": ["action_id"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `ActionStatusParams`,
    /// then land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<ActionStatusParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_status_name() {
        assert_eq!(
            ActionStatusTool {
                actions: Arc::new(BackgroundActions::new())
            }
            .name(),
            "action_status"
        );
    }

    #[test]
    fn test_status_risk_level() {
        let tool = ActionStatusTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_status_schema() {
        let tool = ActionStatusTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "action_id"));
    }

    #[tokio::test]
    async fn test_status_unknown_action() {
        let tool = ActionStatusTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool
            .execute(
                json!({"action_id": "action-nope"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], "not_found");
    }

    #[tokio::test]
    async fn test_status_requires_action_id() {
        let tool = ActionStatusTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_status_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = ActionStatusTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool.execute(json!({"action_id": "action-x"}), cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_status_native_entry_lands_in_run() {
        let tool = ActionStatusTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool
            .run(
                ActionStatusParams {
                    action_id: "action-nope".into(),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["status"], "not_found");
    }
}
