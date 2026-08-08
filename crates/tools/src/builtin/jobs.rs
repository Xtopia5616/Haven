use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundJobs;
use crate::{Tool, ToolResult};

/// One-call board of every background job started by the current task
/// (job_id, status, timestamps, output preview), so the agent can inspect all
/// background work at once instead of polling `status` job by job.
///
/// The owning task id is injected privately by the tools manager
/// (`_task_id`), mirroring the `reminder` tool, so a job board can never
/// leak other tasks' jobs or outputs.
pub struct JobsTool {
    pub jobs: Arc<BackgroundJobs>,
}

#[async_trait]
impl Tool for JobsTool {
    fn name(&self) -> String {
        "jobs".into()
    }
    fn description(&self) -> String {
        "List all background jobs of the current task in one call: job_id, status, timestamps and a brief output preview. Use this instead of polling status job by job."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["running", "completed", "failed", "cancelled"],
                    "description": "Optional filter: only list jobs in this state"
                }
            }
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let task_id = input["_task_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("jobs requires a task context"))?;
        let filter = input["status"].as_str().map(|s| s.to_string());
        let mut rows = self.jobs.list_for_task(task_id).await;
        if let Some(f) = filter.as_deref() {
            rows.retain(|r| r["status"].as_str() == Some(f));
        }
        Ok(ToolResult::ok(serde_json::json!({ "jobs": rows })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_jobs_tool_name() {
        assert_eq!(
            JobsTool {
                jobs: Arc::new(BackgroundJobs::new())
            }
            .name(),
            "jobs"
        );
    }

    #[test]
    fn test_jobs_tool_risk_level() {
        let tool = JobsTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_jobs_tool_schema() {
        let tool = JobsTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let schema = tool.input_schema();
        let filter = &schema["properties"]["status"]["enum"];
        assert!(filter.is_array());
    }

    #[tokio::test]
    async fn test_jobs_tool_requires_task_context() {
        let tool = JobsTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(result.is_err(), "jobs without a task context must fail");
    }

    #[tokio::test]
    async fn test_jobs_tool_lists_task_jobs() {
        let jobs = Arc::new(BackgroundJobs::new());
        let tool = JobsTool { jobs };
        let result = tool
            .execute(json!({"_task_id": "task-x"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["jobs"], json!([]));
    }

    #[tokio::test]
    async fn test_jobs_tool_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = JobsTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let result = tool.execute(json!({"_task_id": "task-x"}), cancel).await;
        assert!(result.is_err());
    }
}
