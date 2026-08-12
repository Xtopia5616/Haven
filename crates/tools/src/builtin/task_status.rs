use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundTasks;
use crate::{Tool, ToolResult};

/// Report the status of a background task (spawned with `shell` +
/// `background: true`). The agent polls this tool with a task_id until the
/// result is ready, instead of blocking the ReAct loop on a long command.
pub struct TaskStatusTool {
    pub tasks: Arc<BackgroundTasks>,
}

#[async_trait]
impl Tool for TaskStatusTool {
    fn name(&self) -> String {
        "task_status".into()
    }
    fn description(&self) -> String {
        "Check a single background task's status by task_id. Results are also pushed back automatically on completion — for an overview of all tasks use `tasks` instead of polling one by one."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The task id returned by a shell(background: true) call"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let task_id = input["task_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("task_id is required for status"))?;
        let status = self.tasks.status(task_id).await;
        Ok(ToolResult::ok(status))
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
            TaskStatusTool {
                tasks: Arc::new(BackgroundTasks::new())
            }
            .name(),
            "task_status"
        );
    }

    #[test]
    fn test_status_risk_level() {
        let tool = TaskStatusTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_status_schema() {
        let tool = TaskStatusTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "task_id"));
    }

    #[tokio::test]
    async fn test_status_unknown_task() {
        let tool = TaskStatusTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let result = tool
            .execute(json!({"task_id": "task-nope"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], "not_found");
    }

    #[tokio::test]
    async fn test_status_requires_task_id() {
        let tool = TaskStatusTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_status_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = TaskStatusTool {
            tasks: Arc::new(BackgroundTasks::new()),
        };
        let result = tool.execute(json!({"task_id": "task-x"}), cancel).await;
        assert!(result.is_err());
    }
}
