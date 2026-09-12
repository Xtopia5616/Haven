use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{OperationIdempotency, Tool, ToolConcurrency, ToolResult};

const MAX_ITEMS: usize = 100;
const MAX_TEXT_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistOperation {
    List,
    Add,
    Update,
    Remove,
    Clear,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ChecklistItem {
    id: String,
    text: String,
    done: bool,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ChecklistParams {
    pub operation: ChecklistOperation,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub done: Option<bool>,
    #[serde(rename = "_session_id", default, skip_serializing)]
    pub(crate) session_id: Option<String>,
}

#[derive(Default)]
pub struct ChecklistTool {
    items: Arc<Mutex<HashMap<String, Vec<ChecklistItem>>>>,
}

impl ChecklistTool {
    pub async fn run(
        &self,
        params: ChecklistParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let session_id = params
            .session_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("checklist requires a session context"))?;
        let mut all = self
            .items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let items = all.entry(session_id.to_string()).or_default();
        let output = match params.operation {
            ChecklistOperation::List => {
                serde_json::json!({ "operation": "list", "items": items, "count": items.len() })
            }
            ChecklistOperation::Add => {
                if items.len() >= MAX_ITEMS {
                    anyhow::bail!("checklist is full (maximum {MAX_ITEMS} items)");
                }
                let text = params.text.as_deref().unwrap_or_default().trim();
                if text.is_empty() {
                    anyhow::bail!("text is required for add");
                }
                if text.chars().count() > MAX_TEXT_CHARS {
                    anyhow::bail!("text must be at most {MAX_TEXT_CHARS} characters");
                }
                let id = params
                    .item_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| haven_common::types::new_id("msg"));
                if items.iter().any(|item| item.id == id) {
                    anyhow::bail!("item_id already exists");
                }
                let item = ChecklistItem {
                    id: id.clone(),
                    text: text.to_string(),
                    done: false,
                };
                items.push(item.clone());
                serde_json::json!({ "operation": "add", "item": item })
            }
            ChecklistOperation::Update => {
                let id = params
                    .item_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("item_id is required for update"))?;
                let item = items
                    .iter_mut()
                    .find(|item| item.id == id)
                    .ok_or_else(|| anyhow::anyhow!("checklist item not found"))?;
                if let Some(text) = params.text.as_deref() {
                    let text = text.trim();
                    if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
                        anyhow::bail!("text must be 1..{MAX_TEXT_CHARS} characters");
                    }
                    item.text = text.to_string();
                }
                if let Some(done) = params.done {
                    item.done = done;
                }
                serde_json::json!({ "operation": "update", "item": item })
            }
            ChecklistOperation::Remove => {
                let id = params
                    .item_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("item_id is required for remove"))?;
                let before = items.len();
                items.retain(|item| item.id != id);
                serde_json::json!({ "operation": "remove", "item_id": id, "removed": items.len() != before })
            }
            ChecklistOperation::Clear => {
                let count = items.len();
                items.clear();
                serde_json::json!({ "operation": "clear", "removed": count })
            }
        };
        Ok(ToolResult::ok(output))
    }
}

#[async_trait]
impl Tool for ChecklistTool {
    fn name(&self) -> String {
        "checklist".into()
    }
    fn description(&self) -> String {
        "Maintain a small per-session checklist without blocking the session.".into()
    }
    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Low
    }
    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input["operation"].as_str() {
            Some("list") => OperationIdempotency::Idempotent,
            Some("add") | Some("update") | Some("remove") | Some("clear") => {
                OperationIdempotency::NonIdempotent
            }
            _ => OperationIdempotency::Unknown,
        }
    }
    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        if input["operation"].as_str() == Some("list") {
            ToolConcurrency::SharedResource("checklist".into())
        } else {
            ToolConcurrency::Resource("checklist".into())
        }
    }
    fn requires_session_id(&self) -> bool {
        true
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object", "additionalProperties": false,
            "properties": { "operation": { "type": "string", "enum": ["list", "add", "update", "remove", "clear"] }, "item_id": { "type": "string", "minLength": 1 }, "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS }, "done": { "type": "boolean" } },
            "oneOf": [
                { "properties": { "operation": { "const": "list" } }, "required": ["operation"] },
                { "properties": { "operation": { "const": "add" } }, "required": ["operation", "text"] },
                { "properties": { "operation": { "const": "update" } }, "required": ["operation", "item_id"] },
                { "properties": { "operation": { "const": "remove" } }, "required": ["operation", "item_id"] },
                { "properties": { "operation": { "const": "clear" } }, "required": ["operation"] }
            ]
        })
    }
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<ChecklistParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manages_items_per_session_without_ask_or_confirm() {
        let tool = ChecklistTool::default();
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let added = tool
            .execute(
                serde_json::json!({
                    "operation": "add",
                    "text": "Review the diff",
                    "_session_id": session_id
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let item_id = added.output["item"]["id"].as_str().unwrap();
        assert!(item_id.starts_with("msg-"));

        tool.execute(
            serde_json::json!({
                "operation": "update",
                "item_id": item_id,
                "done": true,
                "_session_id": session_id
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let listed = tool
            .execute(
                serde_json::json!({ "operation": "list", "_session_id": session_id }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(listed.output["count"], 1);
        assert_eq!(listed.output["items"][0]["done"], true);

        let removed = tool
            .execute(
                serde_json::json!({
                    "operation": "remove",
                    "item_id": item_id,
                    "_session_id": session_id
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(removed.output["removed"], true);
        assert_eq!(tool.risk_level(&serde_json::json!({})), RiskLevel::Low);
    }
}
