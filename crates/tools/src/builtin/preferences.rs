use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{OperationIdempotency, Tool, ToolConcurrency, ToolResult};

const MAX_KEY_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferencesOperation {
    Get,
    Set,
    Clear,
    List,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PreferencesParams {
    pub operation: PreferencesOperation,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(rename = "_session_id", default, skip_serializing)]
    pub(crate) session_id: Option<String>,
}

#[derive(Default)]
pub struct PreferencesTool {
    values: Arc<Mutex<HashMap<String, HashMap<String, Value>>>>,
}

impl PreferencesTool {
    pub async fn run(
        &self,
        params: PreferencesParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let session_id = params
            .session_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("preferences requires a session context"))?;
        let key = params.key.as_deref().map(str::trim);
        if matches!(
            params.operation,
            PreferencesOperation::Get | PreferencesOperation::Set | PreferencesOperation::Clear
        ) && key.is_none_or(str::is_empty)
        {
            anyhow::bail!("key is required for this preferences operation");
        }
        if key.is_some_and(|key| key.chars().count() > MAX_KEY_CHARS) {
            anyhow::bail!("key must be at most {MAX_KEY_CHARS} characters");
        }

        let mut values = self
            .values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = values.entry(session_id.to_string()).or_default();
        let output = match params.operation {
            PreferencesOperation::Get => {
                let key = key.expect("validated above");
                serde_json::json!({ "operation": "get", "key": key, "value": session.get(key), "found": session.contains_key(key) })
            }
            PreferencesOperation::Set => {
                let key = key.expect("validated above");
                let value = params
                    .value
                    .ok_or_else(|| anyhow::anyhow!("value is required for set"))?;
                session.insert(key.to_string(), value.clone());
                serde_json::json!({ "operation": "set", "key": key, "value": value })
            }
            PreferencesOperation::Clear => {
                let key = key.expect("validated above");
                let removed = session.remove(key).is_some();
                serde_json::json!({ "operation": "clear", "key": key, "removed": removed })
            }
            PreferencesOperation::List => {
                serde_json::json!({ "operation": "list", "preferences": session })
            }
        };
        Ok(ToolResult::ok(output))
    }
}

#[async_trait]
impl Tool for PreferencesTool {
    fn name(&self) -> String {
        "preferences".into()
    }
    fn description(&self) -> String {
        crate::prompts::PREFERENCES_DESCRIPTION.into()
    }
    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Low
    }
    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input["operation"].as_str() {
            Some("get") | Some("list") => OperationIdempotency::Idempotent,
            Some("set") | Some("clear") => OperationIdempotency::NonIdempotent,
            _ => OperationIdempotency::Unknown,
        }
    }
    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        match input["operation"].as_str() {
            Some("get") | Some("list") => ToolConcurrency::SharedResource("preferences".into()),
            _ => ToolConcurrency::Resource("preferences".into()),
        }
    }
    fn requires_session_id(&self) -> bool {
        true
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operation": { "type": "string", "enum": ["get", "set", "clear", "list"] },
                "key": { "type": "string", "minLength": 1, "maxLength": MAX_KEY_CHARS },
                "value": {}
            },
            "oneOf": [
                { "properties": { "operation": { "const": "get" }, "key": { "type": "string", "minLength": 1, "maxLength": MAX_KEY_CHARS } }, "required": ["operation", "key"] },
                { "properties": { "operation": { "const": "set" }, "key": { "type": "string", "minLength": 1, "maxLength": MAX_KEY_CHARS }, "value": {} }, "required": ["operation", "key", "value"] },
                { "properties": { "operation": { "const": "clear" }, "key": { "type": "string", "minLength": 1, "maxLength": MAX_KEY_CHARS } }, "required": ["operation", "key"] },
                { "properties": { "operation": { "const": "list" } }, "required": ["operation"] }
            ]
        })
    }
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<PreferencesParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_values_per_session_without_pausing() {
        let tool = PreferencesTool::default();
        tool.execute(
            serde_json::json!({
                "operation": "set",
                "key": "detail",
                "value": "concise",
                "_session_id": "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let found = tool
            .execute(
                serde_json::json!({
                    "operation": "get",
                    "key": "detail",
                    "_session_id": "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(found.output["found"], true);
        assert_eq!(found.output["value"], "concise");

        let isolated = tool
            .execute(
                serde_json::json!({
                    "operation": "get",
                    "key": "detail",
                    "_session_id": "ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(isolated.output["found"], false);
        assert_eq!(tool.risk_level(&serde_json::json!({})), RiskLevel::Low);
    }
}
