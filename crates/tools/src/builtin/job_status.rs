use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::bg::BackgroundJobs;
use crate::{Tool, ToolResult};

/// Report the status of a background job (spawned with `shell` +
/// `background: true`). The agent polls this tool with a job_id until the
/// result is ready, instead of blocking the ReAct loop on a long command.
pub struct JobStatusTool {
    pub jobs: Arc<BackgroundJobs>,
}

#[async_trait]
impl Tool for JobStatusTool {
    fn name(&self) -> String {
        "status".into()
    }
    fn description(&self) -> String {
        "Check a single background job's status by job_id. Results are also pushed back automatically on completion — for an overview of all jobs use `jobs` instead of polling one by one."
            .into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "The job id returned by a shell(background: true) call"
                }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let job_id = input["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("job_id is required for status"))?;
        let status = self.jobs.status(job_id).await;
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
            JobStatusTool {
                jobs: Arc::new(BackgroundJobs::new())
            }
            .name(),
            "status"
        );
    }

    #[test]
    fn test_status_risk_level() {
        let tool = JobStatusTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
    }

    #[test]
    fn test_status_schema() {
        let tool = JobStatusTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "job_id"));
    }

    #[tokio::test]
    async fn test_status_unknown_job() {
        let tool = JobStatusTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let result = tool
            .execute(json!({"job_id": "job-nope"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], "not_found");
    }

    #[tokio::test]
    async fn test_status_requires_job_id() {
        let tool = JobStatusTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_status_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = JobStatusTool {
            jobs: Arc::new(BackgroundJobs::new()),
        };
        let result = tool.execute(json!({"job_id": "job-x"}), cancel).await;
        assert!(result.is_err());
    }
}
