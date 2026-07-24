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

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        let op = input["operation"].as_str().unwrap_or("list");

        match op {
            "get" => {
                let name = input["name"].as_str().ok_or_else(|| anyhow::anyhow!("name is required for get"))?;
                match env::var(name) {
                    Ok(val) => Ok(ToolResult::ok(serde_json::json!({"name": name, "value": val}))),
                    Err(env::VarError::NotPresent) => {
                        Ok(ToolResult { success: true, output: serde_json::json!({"name": name, "value": null}), error: None, truncated: false })
                    }
                    Err(e) => anyhow::bail!("failed to read env var '{}': {}", name, e),
                }
            }
            "set" => {
                let name = input["name"].as_str().ok_or_else(|| anyhow::anyhow!("name is required for set"))?.to_string();
                let value = input["value"].as_str().ok_or_else(|| anyhow::anyhow!("value is required for set"))?.to_string();
                unsafe { env::set_var(&name, &value); }
                Ok(ToolResult::ok(serde_json::json!({"set": true, "name": name, "value": value})))
            }
            "unset" => {
                let name = input["name"].as_str().ok_or_else(|| anyhow::anyhow!("name is required for unset"))?;
                unsafe { env::remove_var(name); }
                Ok(ToolResult::ok(serde_json::json!({"removed": true, "name": name})))
            }
            "list" => {
                let vars: Vec<Value> = env::vars()
                    .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
                    .collect();
                Ok(ToolResult::ok(serde_json::json!({"variables": vars, "count": vars.len()})))
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
        assert_eq!(EnvTool.risk_level(&json!({"operation": "get"})), RiskLevel::Low);
        assert_eq!(EnvTool.risk_level(&json!({"operation": "set"})), RiskLevel::High);
        assert_eq!(EnvTool.risk_level(&json!({"operation": "unset"})), RiskLevel::High);
        assert_eq!(EnvTool.risk_level(&json!({"operation": "list"})), RiskLevel::High);
    }

    #[test]
    fn test_env_tool_input_schema() {
        let schema = EnvTool.input_schema();
        assert!(schema["properties"]["operation"]["enum"].as_array().is_some());
    }
}
