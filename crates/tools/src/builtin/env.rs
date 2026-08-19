use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::env;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct EnvTool {
    /// Output cap (chars) for environment listings.
    pub max_output_chars: usize,
}

/// Environment operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvOperation {
    Get,
    Set,
    Unset,
    List,
}

/// Typed parameters for `EnvTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `EnvTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EnvParams {
    /// Operation to perform; defaults to `list`.
    #[serde(default)]
    pub operation: Option<EnvOperation>,
    /// Environment variable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Value for set operation.
    #[serde(default)]
    pub value: Option<String>,
}

impl EnvTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: EnvParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        match params.operation.unwrap_or(EnvOperation::List) {
            EnvOperation::Get => {
                let name = params
                    .name
                    .ok_or_else(|| anyhow::anyhow!("name is required for get"))?;
                match env::var(&name) {
                    Ok(val) => Ok(ToolResult::ok(
                        serde_json::json!({"name": name, "value": val}),
                    )),
                    Err(env::VarError::NotPresent) => Ok(ToolResult {
                        success: true,
                        output: serde_json::json!({"name": name, "value": null}),
                        error: None,
                        truncated: false,
                        signals: crate::tool::ToolSignals::default(),
                    }),
                    Err(e) => anyhow::bail!("failed to read env var '{}': {}", name, e),
                }
            }
            EnvOperation::Set => {
                let name = params
                    .name
                    .ok_or_else(|| anyhow::anyhow!("name is required for set"))?;
                let value = params
                    .value
                    .ok_or_else(|| anyhow::anyhow!("value is required for set"))?;
                unsafe {
                    env::set_var(&name, &value);
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"set": true, "name": name, "value": value}),
                ))
            }
            EnvOperation::Unset => {
                let name = params
                    .name
                    .ok_or_else(|| anyhow::anyhow!("name is required for unset"))?;
                unsafe {
                    env::remove_var(&name);
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"removed": true, "name": name}),
                ))
            }
            EnvOperation::List => {
                let vars: Vec<Value> = env::vars()
                    .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
                    .collect();
                let count = vars.len();
                let max_chars = self.max_output_chars;
                let (mut result, truncated) =
                    crate::util::json_list_within_budget("variables", vars, count, max_chars);
                if truncated {
                    result["hint"] = serde_json::json!(
                        "Environment listing truncated to the max chars budget. Use get with a specific variable name to read its full value."
                    );
                }
                Ok(ToolResult::ok(result))
            }
        }
    }
}

impl Default for EnvTool {
    fn default() -> Self {
        Self {
            max_output_chars: 20_000,
        }
    }
}

#[async_trait]
impl Tool for EnvTool {
    fn name(&self) -> String {
        "env".into()
    }
    fn description(&self) -> String {
        "Get or set environment variables".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("set") | Some("unset") | Some("list") => RiskLevel::High,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["get", "set", "unset", "list"] },
                "name": { "type": "string", "description": "Environment variable name" },
                "value": { "type": "string", "description": "Value for set operation" }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `EnvParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<EnvParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_env_tool_name() {
        assert_eq!(EnvTool::default().name(), "env");
    }

    #[test]
    fn test_env_tool_risk_level() {
        assert_eq!(
            EnvTool::default().risk_level(&json!({"operation": "get"})),
            RiskLevel::Low
        );
        assert_eq!(
            EnvTool::default().risk_level(&json!({"operation": "set"})),
            RiskLevel::High
        );
        assert_eq!(
            EnvTool::default().risk_level(&json!({"operation": "unset"})),
            RiskLevel::High
        );
        assert_eq!(
            EnvTool::default().risk_level(&json!({"operation": "list"})),
            RiskLevel::High
        );
    }

    #[test]
    fn test_env_tool_input_schema() {
        let schema = EnvTool::default().input_schema();
        assert!(
            schema["properties"]["operation"]["enum"]
                .as_array()
                .is_some()
        );
    }

    fn unique_var_name(tag: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("HAVEN_TEST_{}_{}_{}", tag, std::process::id(), n)
    }

    #[tokio::test]
    async fn test_env_get_existing() {
        let name = unique_var_name("GET");
        unsafe {
            env::set_var(&name, "hello");
        }
        let result = EnvTool::default()
            .execute(
                json!({"operation": "get", "name": name}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["value"], "hello");
        unsafe {
            env::remove_var(&name);
        }
    }

    #[tokio::test]
    async fn test_env_get_missing_returns_null() {
        let name = unique_var_name("MISSING");
        unsafe {
            env::remove_var(&name);
        }
        let result = EnvTool::default()
            .execute(
                json!({"operation": "get", "name": name}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["value"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_env_get_requires_name() {
        let result = EnvTool::default()
            .execute(json!({"operation": "get"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_set_and_get_roundtrip() {
        let name = unique_var_name("SET");
        let result = EnvTool::default()
            .execute(
                json!({"operation": "set", "name": name, "value": "v1"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["set"], true);
        assert_eq!(env::var(&name).unwrap(), "v1");
        unsafe {
            env::remove_var(&name);
        }
    }

    #[tokio::test]
    async fn test_env_set_requires_value() {
        let result = EnvTool::default()
            .execute(
                json!({"operation": "set", "name": unique_var_name("SET")}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_unset() {
        let name = unique_var_name("UNSET");
        unsafe {
            env::set_var(&name, "temp");
        }
        let result = EnvTool::default()
            .execute(
                json!({"operation": "unset", "name": name.clone()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["removed"], true);
        assert!(env::var_os(&name).is_none());
    }

    #[tokio::test]
    async fn test_env_list_returns_variables() {
        let result = EnvTool::default()
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        let vars = result.output["variables"].as_array().unwrap();
        assert!(!vars.is_empty());
        assert!(vars[0]["name"].as_str().is_some());
        assert!(vars[0]["value"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_env_unknown_operation() {
        let result = EnvTool::default()
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = EnvTool::default()
            .execute(json!({"operation": "list"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_native_entry_lands_in_run() {
        let name = unique_var_name("NATIVE");
        unsafe {
            env::set_var(&name, "v2");
        }
        let result = EnvTool::default()
            .run(
                EnvParams {
                    operation: Some(EnvOperation::Get),
                    name: Some(name.clone()),
                    value: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["value"], "v2");
        unsafe {
            env::remove_var(&name);
        }
    }
}
