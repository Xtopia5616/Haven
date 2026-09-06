use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{BackgroundActions, Tool, ToolConcurrency, ToolResult};

/// Explicit mutating operation supported by the background-action board.
#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionsOperation {
    Cancel,
}

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
    /// Optional mutating operation. Listing and inspection retain their
    /// compact legacy shapes; cancellation is explicit.
    #[serde(default)]
    pub operation: Option<ActionsOperation>,
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

        let session_id = params
            .session_id
            .ok_or_else(|| anyhow::anyhow!("actions requires a session context"))?;

        if matches!(params.operation, Some(ActionsOperation::Cancel)) {
            let action_id = params
                .action_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow::anyhow!("action_id is required for cancel"))?;
            let cancelled = self
                .actions
                .cancel_for_session(action_id, &session_id)
                .await;
            return Ok(ToolResult::ok(serde_json::json!({
                "operation": "cancel",
                "action_id": action_id,
                "cancelled": cancelled,
            })));
        }

        if let Some(action_id) = params
            .action_id
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            let status = self
                .actions
                .status_for_session(action_id, &session_id)
                .await;
            let mut output = status;
            if let Some(object) = output.as_object_mut() {
                object.insert("operation".into(), serde_json::json!("inspect"));
            }
            return Ok(ToolResult::ok(output));
        }

        let filter = params.status;
        let mut rows = self.actions.list_for_session(&session_id).await;
        if let Some(f) = filter.as_deref() {
            rows.retain(|r| r["status"].as_str() == Some(f));
        }
        let all_running = !rows.is_empty()
            && rows
                .iter()
                .all(|r| r.get("status").and_then(|s| s.as_str()) == Some("running"));
        if all_running {
            let mut body = haven_common::tools::background_wait_object(
                "All listed background actions are still running. END YOUR TURN if you have nothing else useful to do — do not poll. Results are auto-pushed and the session is auto-woken when they finish.",
            );
            body.insert("operation".into(), serde_json::json!("list"));
            body.insert("actions".into(), serde_json::json!(rows));
            return Ok(ToolResult::ok(serde_json::Value::Object(body)));
        }
        Ok(ToolResult::ok(
            serde_json::json!({ "operation": "list", "actions": rows }),
        ))
    }
}

#[async_trait]
impl Tool for ActionsTool {
    fn name(&self) -> String {
        "actions".into()
    }
    fn description(&self) -> String {
        "List or inspect background actions of the current session, or cancel one with operation=cancel and action_id. One-shot awareness only — never poll in a wait loop. While actions are still running and you have no other foreground work, end your turn; completion results are auto-pushed and the session is auto-woken.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        if input["operation"].as_str() == Some("cancel") {
            RiskLevel::Medium
        } else {
            RiskLevel::Safe
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        if input["operation"].as_str() == Some("cancel") {
            ToolConcurrency::Resource("actions".into())
        } else {
            ToolConcurrency::SharedResource("actions".into())
        }
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
                    "minLength": 1,
                    "description": "Inspect this single background action instead of listing"
                },
                "operation": {
                    "type": "string",
                    "enum": ["cancel"],
                    "description": "Cancel a running background action owned by this session"
                },
                "status": {
                    "type": "string",
                    "enum": ["running", "completed", "failed", "cancelled"],
                    "description": "Optional filter when listing: only actions in this state"
                }
            },
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "action_id": { "type": "string", "minLength": 1 } },
                    "required": ["action_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "cancel" },
                        "action_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["operation", "action_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled"] }
                    }
                }
            ]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<ActionsParams>(&self.name(), input)?;
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
        assert_eq!(
            tool.risk_level(&json!({"operation": "cancel"})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_actions_tool_schema() {
        let tool = ActionsTool {
            actions: Arc::new(BackgroundActions::new()),
        };
        let schema = tool.input_schema();
        assert!(schema["properties"]["action_id"].is_object());
        assert_eq!(schema["properties"]["operation"]["enum"], json!(["cancel"]));
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
                    operation: None,
                    status: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["actions"], json!([]));
    }

    #[tokio::test]
    async fn test_actions_tool_cancels_only_owned_running_action() {
        let actions = Arc::new(BackgroundActions::new());
        let tool = ActionsTool { actions };
        let result = tool
            .execute(
                json!({
                    "operation": "cancel",
                    "action_id": "act-nope",
                    "_session_id": "ses-x"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["operation"], "cancel");
        assert_eq!(result.output["cancelled"], false);
    }
}
