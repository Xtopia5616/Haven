use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundActions;
use crate::{Tool, ToolResult};

/// Background-action board for the current session.
///
/// - Without `action_id`: list all (optional `status` filter).
/// - With `action_id`: return that single action's status (results are also
///   pushed back automatically on completion — prefer not polling).
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
    /// When set, return this single action's status instead of the board.
    #[serde(default)]
    pub action_id: Option<String>,
    /// Optional filter when listing: only actions in this state.
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

        if let Some(action_id) = params
            .action_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let status = self.actions.status(action_id).await;
            return Ok(ToolResult::ok(status));
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
        "List background actions of the current session (action_id, status, timestamps, output preview), or pass action_id to inspect one. Completion results are pushed back automatically — do not poll.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    /// Needs the private `_session_id` input so the action board is scoped to the
    /// current session (single-id lookup does not need it).
    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action_id": {
                    "type": "string",
                    "description": "Optional: inspect this single background action instead of listing"
                },
                "status": {
                    "type": "string",
                    "enum": ["running", "completed", "failed", "cancelled"],
                    "description": "Optional filter when listing: only actions in this state"
                }
            }
        })
    }

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
        assert!(schema["properties"]["action_id"].is_object());
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
    async fn test_actions_tool_single_action_status() {
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let result = tool
            .execute(
                json!({"action_id": "act-nope", "_session_id": "ses-x"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], "not_found");
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
                    action_id: None,
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
