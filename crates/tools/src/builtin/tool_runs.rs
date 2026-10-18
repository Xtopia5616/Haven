use async_trait::async_trait;
use haven_common::lifecycle::ToolRunStatus;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::canonical_entity_id_schema;
use crate::{Tool, ToolConcurrency, ToolResult, ToolRunService};

/// Explicit operations supported by ToolRun start and management views.
#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRunsOperation {
    Start,
    List,
    Inspect,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRunStart {
    pub tool_name: String,
    pub input: Value,
}

/// Parse the model-visible payload used by `toolrun.start`.
pub fn parse_toolrun_start(input: &Value) -> Result<ToolRunStart, String> {
    let tool_name = input
        .get("tool_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or_else(|| "toolrun.start requires a valid tool_name".to_owned())?;
    if tool_name == "toolrun" || tool_name.starts_with("toolrun.") {
        return Err("toolrun cannot recursively start another ToolRun".into());
    }
    let input = input
        .get("input")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if !input.is_object() {
        return Err("toolrun.start input must be a JSON object".into());
    }
    Ok(ToolRunStart {
        tool_name: tool_name.to_owned(),
        input,
    })
}

/// Background ToolRun launcher and manager for the current session.
///
/// - Without `tool_run_id`: list all (optional `status` filter).
/// - With `tool_run_id`: return that single ToolRun's status (results are also
///   pushed back automatically on completion — prefer not polling).
pub(crate) struct ToolRunsTool {
    pub(crate) tool_runs: Arc<ToolRunService>,
}

/// Typed parameters for `ToolRunsTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `ToolRunsTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ToolRunsParams {
    /// Private owning session id, injected by the Tools facade.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
    /// When set, return this single task's status instead of the board.
    #[serde(default)]
    pub tool_run_id: Option<String>,
    /// Operation selected by the root or injected by an operation view.
    #[serde(default)]
    pub operation: Option<ToolRunsOperation>,
    /// Target for `start`; authorization is resolved against this tool call.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Target input for `start`.
    #[serde(default)]
    pub input: Option<Value>,
    /// Optional filter when listing: only tool_runs in this state.
    #[serde(default)]
    pub status: Option<ToolRunStatus>,
}

impl ToolRunsTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ToolRunsParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let session_id = params
            .session_id
            .ok_or_else(|| anyhow::anyhow!("toolrun requires a session context"))?;

        if matches!(params.operation, Some(ToolRunsOperation::Start)) {
            anyhow::bail!(
                "toolrun.start must be dispatched through the Agent authorization gateway"
            );
        }

        if let Some(tool_run_id) = params.tool_run_id.as_deref() {
            crate::tool_contract::validate_entity_id(tool_run_id, "tool_run_id", "toolrun")?;
        }

        if matches!(params.operation, Some(ToolRunsOperation::Cancel)) {
            let tool_run_id = params
                .tool_run_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("tool_run_id is required for cancel"))?;
            let cancelled = self
                .tool_runs
                .cancel_for_session(tool_run_id, &session_id)
                .await;
            return Ok(ToolResult::ok(serde_json::json!({
                "operation": "cancel",
                "tool_run_id": tool_run_id,
                "cancelled": cancelled,
            })));
        }

        if let Some(tool_run_id) = params.tool_run_id.as_deref() {
            let status = self
                .tool_runs
                .status_for_session_view(tool_run_id, &session_id)
                .await;
            let mut output = status.to_json(true);
            if let Some(object) = output.as_object_mut() {
                object.insert("operation".into(), serde_json::json!("inspect"));
            }
            let truncated = output["truncated"].as_bool().unwrap_or(false);
            return Ok(ToolResult::from_output(output, truncated));
        }

        let filter = params.status;
        let mut rows = self.tool_runs.list_for_session_views(&session_id).await;
        if let Some(f) = filter {
            rows.retain(|row| row.status == f);
        }
        let all_running_background = !rows.is_empty()
            && rows.iter().all(|row| {
                row.kind == crate::ToolRunKind::Background && row.status == ToolRunStatus::Running
            });
        let rows_json = || rows.iter().map(|row| row.to_json()).collect::<Vec<_>>();
        if all_running_background {
            let tool_run_ids = rows
                .iter()
                .filter_map(|row| match &row.projection {
                    crate::ToolRunStatusView::Background { tool_run_id, .. } => {
                        Some(tool_run_id.clone())
                    }
                    crate::ToolRunStatusView::NotFound { .. } => None,
                })
                .collect::<Vec<_>>();
            let mut body = haven_common::tools::background_wait_object(
                tool_run_ids,
                "All listed background tool_runs are still running. END YOUR TURN if you have nothing else useful to do — do not poll. Results are auto-pushed and the session is auto-woken when they finish.",
            );
            body.insert("operation".into(), serde_json::json!("list"));
            body.insert("tool_runs".into(), serde_json::json!(rows_json()));
            return Ok(ToolResult::ok(serde_json::Value::Object(body)));
        }
        Ok(ToolResult::ok(
            serde_json::json!({ "operation": "list", "tool_runs": rows_json() }),
        ))
    }
}

#[async_trait]
impl Tool for ToolRunsTool {
    fn name(&self) -> String {
        "toolrun".into()
    }
    fn description(&self) -> String {
        crate::prompts::TOOL_RUNS_DESCRIPTION.into()
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
            ToolConcurrency::Resource("toolrun".into())
        } else {
            ToolConcurrency::SharedResource("toolrun".into())
        }
    }

    /// Needs the private `_session_id` input so the ToolRun board is scoped to the
    /// current session (single-id lookup does not need it).
    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "tool_run_id": canonical_entity_id_schema("toolrun", Some("Inspect this single background ToolRun instead of listing")),
                "operation": {
                    "type": "string",
                    "enum": ["start", "list", "inspect", "cancel"],
                    "description": "Start a background tool call or manage this session's ToolRuns"
                },
                "tool_name": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Enabled tool to invoke in the background" },
                "input": { "type": "object", "description": "Arguments passed unchanged to tool_name" },
                "status": {
                    "type": "string",
                    "enum": ["running", "completed", "failed", "cancelled"],
                    "description": "Optional filter when listing ToolRuns"
                },
            },
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "start" },
                        "tool_name": { "type": "string", "minLength": 1, "maxLength": 128 },
                        "input": { "type": "object" }
                    },
                    "required": ["operation", "tool_name"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "list" }, "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled"] } }
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "inspect" }, "tool_run_id": canonical_entity_id_schema("toolrun", None) },
                    "required": ["operation", "tool_run_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "cancel" },
                        "tool_run_id": canonical_entity_id_schema("toolrun", None)
                    },
                    "required": ["operation", "tool_run_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled"] } }
                }
            ]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<ToolRunsParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }

    fn registrations(&self, output: &Value) -> Vec<crate::ToolRegistration> {
        output
            .get("tool_run_id")
            .and_then(Value::as_str)
            .filter(|id| haven_common::types::is_canonical_id(id, "toolrun"))
            .map(|tool_run_id| vec![crate::ToolRegistration::ToolRun(tool_run_id.to_owned())])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn toolrun_tool_name_is_stable() {
        assert_eq!(
            ToolRunsTool {
                tool_runs: Arc::new(ToolRunService::new()),
            }
            .name(),
            "toolrun"
        );
    }

    #[test]
    fn test_tool_runs_tool_risk_level() {
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Safe);
        assert_eq!(
            tool.risk_level(&json!({"operation": "cancel"})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_tool_runs_tool_schema() {
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        let schema = tool.input_schema();
        assert!(schema["properties"]["tool_run_id"].is_object());
        assert_eq!(
            schema["properties"]["operation"]["enum"],
            json!(["start", "list", "inspect", "cancel"])
        );
        assert_eq!(
            schema["properties"]["status"]["enum"],
            json!(["running", "completed", "failed", "cancelled"])
        );
        let filter = &schema["properties"]["status"]["enum"];
        assert!(filter.is_array());
    }

    #[tokio::test]
    async fn test_tool_runs_tool_requires_session_context() {
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        let result = tool.execute(json!({}), CancellationToken::new()).await;
        assert!(
            result.is_err(),
            "toolrun without a session context must fail"
        );
    }

    #[tokio::test]
    async fn test_tool_runs_tool_lists_session_tool_runs() {
        let tool_runs = Arc::new(ToolRunService::new());
        let tool = ToolRunsTool { tool_runs };
        let result = tool
            .execute(
                json!({"_session_id": "ses-f6a1fdb2589b63c2ebaead556897b633"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["tool_runs"], json!([]));
    }

    #[tokio::test]
    async fn test_tool_runs_tool_single_tool_run_status() {
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        let result = tool
            .execute(
                json!({"tool_run_id": "toolrun-00000000000000000000000000000001", "_session_id": "ses-f6a1fdb2589b63c2ebaead556897b633"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], "not_found");
    }

    #[tokio::test]
    async fn test_tool_runs_tool_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        let result = tool
            .execute(
                json!({"_session_id": "ses-f6a1fdb2589b63c2ebaead556897b633"}),
                cancel,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_runs_tool_native_entry_lands_in_run() {
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        let result = tool
            .run(
                ToolRunsParams {
                    session_id: Some("ses-f6a1fdb2589b63c2ebaead556897b633".into()),
                    tool_run_id: None,
                    operation: None,
                    tool_name: None,
                    input: None,
                    status: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["tool_runs"], json!([]));
    }

    #[tokio::test]
    async fn test_tool_runs_tool_cancels_only_owned_running_tool_run() {
        let tool_runs = Arc::new(ToolRunService::new());
        let tool = ToolRunsTool { tool_runs };
        let result = tool
            .execute(
                json!({
                    "operation": "cancel",
                    "tool_run_id": "toolrun-00000000000000000000000000000001",
                    "_session_id": "ses-f6a1fdb2589b63c2ebaead556897b633"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["operation"], "cancel");
        assert_eq!(result.output["cancelled"], false);
    }

    #[tokio::test]
    async fn test_tool_runs_rejects_noncanonical_tool_run_id_before_lookup() {
        let tool = ToolRunsTool {
            tool_runs: Arc::new(ToolRunService::new()),
        };
        let error = tool
            .run(
                ToolRunsParams {
                    session_id: Some("ses-f6a1fdb2589b63c2ebaead556897b633".into()),
                    tool_run_id: Some("toolrun-invalid".into()),
                    operation: None,
                    tool_name: None,
                    input: None,
                    status: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("canonical toolrun- prefixed id"));
    }
}
