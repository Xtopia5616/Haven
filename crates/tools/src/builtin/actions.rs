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

/// Typed parameters for `ActionsTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `ActionsTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ActionsParams {
    /// Private owning session id, injected by the tools manager.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
    /// Optional filter: only list actions in this state.
    #[serde(default)]
    pub status: Option<String>,
}

impl ActionsTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ActionsParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let session_id = params
            .session_id
            .ok_or_else(|| anyhow::anyhow!("actions requires a session context"))?;
        let filter = params.status;
        let mut rows = self.actions.list_for_session(&session_id).await;
        if let Some(f) = filter.as_deref() {
            rows.retain(|r| r["status"].as_str() == Some(f));
        }
        Ok(ToolResult::ok(serde_json::json!({ "actions": rows })))
    }
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

    /// Entry ②: LLM JSON entry — convert/validate into `ActionsParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<ActionsParams>(&self.name(), input)?;
        self.run(params, cancel).await
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
        assert!(
            result.is_err(),
            "actions without a session context must fail"
        );
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

    #[tokio::test]
    async fn test_actions_tool_native_entry_lands_in_run() {
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool
            .run(
                ActionsParams {
                    session_id: Some("ses-x".into()),
                    status: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["actions"], json!([]));
    }
}
