use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::env;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct EnvTool;

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

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let op = input["operation"].as_str().unwrap_or("list");

        match op {
            "get" => {
                let name = input["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("name is required for get"))?;
                match env::var(name) {
                    Ok(val) => Ok(ToolResult::ok(
                        serde_json::json!({"name": name, "value": val}),
                    )),
                    Err(env::VarError::NotPresent) => Ok(ToolResult {
                        success: true,
                        output: serde_json::json!({"name": name, "value": null}),
                        error: None,
                        truncated: false,
                    }),
                    Err(e) => anyhow::bail!("failed to read env var '{}': {}", name, e),
                }
            }
            "set" => {
                let name = input["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("name is required for set"))?
                    .to_string();
                let value = input["value"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("value is required for set"))?
                    .to_string();
                unsafe {
                    env::set_var(&name, &value);
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"set": true, "name": name, "value": value}),
                ))
            }
            "unset" => {
                let name = input["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("name is required for unset"))?;
                unsafe {
                    env::remove_var(name);
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"removed": true, "name": name}),
                ))
            }
            "list" => {
                let vars: Vec<Value> = env::vars()
                    .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
                    .collect();
                let count = vars.len();
                let max_chars = self.max_output_chars();
                let (mut result, truncated) =
                    crate::util::json_list_within_budget("variables", vars, count, max_chars);
                if truncated {
                    result["hint"] = serde_json::json!(
                        "Environment listing truncated to the max chars budget. Use get with a specific variable name to read its full value."
                    );
                }
                Ok(ToolResult::ok(result))
            }
            _ => anyhow::bail!("unknown env operation: {}", op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_env_tool_name() {
        assert_eq!(EnvTool.name(), "env");
    }

    #[test]
    fn test_env_tool_risk_level() {
        assert_eq!(
            EnvTool.risk_level(&json!({"operation": "get"})),
            RiskLevel::Low
        );
        assert_eq!(
            EnvTool.risk_level(&json!({"operation": "set"})),
            RiskLevel::High
        );
        assert_eq!(
            EnvTool.risk_level(&json!({"operation": "unset"})),
            RiskLevel::High
        );
        assert_eq!(
            EnvTool.risk_level(&json!({"operation": "list"})),
            RiskLevel::High
        );
    }

    #[test]
    fn test_env_tool_input_schema() {
        let schema = EnvTool.input_schema();
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
        let result = EnvTool
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
        let result = EnvTool
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
        let result = EnvTool
            .execute(json!({"operation": "get"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_set_and_get_roundtrip() {
        let name = unique_var_name("SET");
        let result = EnvTool
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
        let result = EnvTool
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
        let result = EnvTool
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
        let result = EnvTool
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
        let result = EnvTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_env_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = EnvTool.execute(json!({"operation": "list"}), cancel).await;
        assert!(result.is_err());
    }
}
