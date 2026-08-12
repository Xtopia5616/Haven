use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundTasks;
use crate::{Tool, ToolResult};

/// One-call board of every background task started by the current session
/// (task_id, status, timestamps, output preview), so the agent can inspect all
/// background work at once instead of polling `status` task by task.
///
/// The owning session id is injected privately by the tools manager
/// (`_session_id`), mirroring the `scheduled_task` tool, so a task board can never
/// leak other sessions' tasks or outputs.
pub struct TasksTool {
    pub tasks: Arc<BackgroundTasks>,
}

#[async_trait]
impl Tool for TasksTool {
    fn name(&self) -> String {
        "tasks".into()
    }
    fn description(&self) -> String {
        "List all background tasks of the current session in one call: task_id, status, timestamps and a brief output preview. Use this instead of polling status task by task."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    /// Needs the private `_session_id` input so the task board is scoped to the
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
                    "description": "Optional filter: only list tasks in this state"
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
            .ok_or_else(|| anyhow::anyhow!("tasks requires a session context"))?;
        let filter = input["status"].as_str().map(|s| s.to_string());
        let mut rows = self.tasks.list_for_session(session_id).await;
        if let Some(f) = filter.as_deref() {
            rows.retain(|r| r["status"].as_str() == Some(f));
        }
        Ok(ToolResult::ok(serde_json::json!({ "tasks": rows })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_tasks_tool_name() {
        assert_eq!(
            TasksTool {
                tasks: Arc::new(BackgroundTasks::new())
            }
            .name(),
            "tasks"
        );
    }

    #[test]
    fn test_tasks_tool_risk_level() {
        let tool = TasksTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_tasks_tool_schema() {
        let tool = TasksTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let schema = tool.input_schema();
        let filter = &schema["properties"]["status"]["enum"];
        assert!(filter.is_array());
    }

    #[tokio::test]
    async fn test_tasks_tool_requires_session_context() {
        let tool = TasksTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(result.is_err(), "tasks without a session context must fail");
    }

    #[tokio::test]
    async fn test_tasks_tool_lists_session_tasks() {
        let tasks = Arc::new(BackgroundTasks::new());
        let tool = TasksTool { tasks };
        let result = tool
            .execute(json!({"_session_id": "ses-x"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["tasks"], json!([]));
    }

    #[tokio::test]
    async fn test_tasks_tool_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = TasksTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let result = tool.execute(json!({"_session_id": "ses-x"}), cancel).await;
        assert!(result.is_err());
    }
}
