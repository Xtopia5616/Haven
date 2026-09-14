use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::ui_automation::UIA_CONTROL_TYPE_NAMES;
use super::{WindowParams, WindowTool};
use crate::{Tool, ToolConcurrency, ToolResult};

#[async_trait]
impl Tool for WindowTool {
    fn name(&self) -> String {
        "window".into()
    }
    fn description(&self) -> String {
        crate::prompts::WINDOW_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("close") => RiskLevel::High,
            // OCR uploads a full-screen capture to the vision model.
            Some("ocr") => RiskLevel::High,
            Some("observe") if input["ocr"].as_bool() == Some(true) => RiskLevel::High,
            Some("focus") => RiskLevel::Medium,
            Some("ui_tree") | Some("observe") | Some("wait") => RiskLevel::Low,
            Some("invoke") | Some("set_value") | Some("toggle") | Some("select") => {
                RiskLevel::Medium
            }
            _ => RiskLevel::Low,
        }
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        if input["operation"].as_str() == Some("wait") {
            input
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(10)
                .saturating_add(5)
        } else {
            30
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        match input["operation"].as_str() {
            Some("list") | Some("foreground") | Some("ui_tree") | Some("observe")
            | Some("wait") => ToolConcurrency::SharedResource("desktop".into()),
            _ => ToolConcurrency::Resource("desktop".into()),
        }
    }

    fn input_schema(&self) -> Value {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "foreground", "focus", "close", "screenshot", "ocr", "ui_tree", "observe", "invoke", "set_value", "toggle", "select", "wait"] },
                "title": { "type": "string" },
                "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" },
                "pid": { "type": "integer", "minimum": 1 },
                "condition": { "type": "string", "enum": ["title_contains", "foreground_contains", "ui_text"] },
                "text": { "type": "string", "minLength": 1 },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 },
                "element_token": { "type": "string", "minLength": 1 },
                "name": { "type": "string", "minLength": 1 },
                "control_type": { "type": "string", "enum": UIA_CONTROL_TYPE_NAMES },
                "index": { "type": "integer", "minimum": 0 },
                "value": { "type": "string", "maxLength": 20000 },
                "ocr": { "type": "boolean" }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "observe" },
                        "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" },
                        "title": { "type": "string", "minLength": 1 },
                        "pid": { "type": "integer", "minimum": 1 },
                        "ocr": { "type": "boolean" }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "enum": ["invoke", "toggle", "select"] },
                        "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" },
                        "title": { "type": "string", "minLength": 1 },
                        "element_token": { "type": "string", "minLength": 1 },
                        "name": { "type": "string", "minLength": 1 },
                        "control_type": { "type": "string", "enum": UIA_CONTROL_TYPE_NAMES },
                        "index": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "set_value" },
                        "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" },
                        "title": { "type": "string", "minLength": 1 },
                        "element_token": { "type": "string", "minLength": 1 },
                        "name": { "type": "string", "minLength": 1 },
                        "control_type": { "type": "string", "enum": UIA_CONTROL_TYPE_NAMES },
                        "index": { "type": "integer", "minimum": 0 },
                        "value": { "type": "string", "maxLength": 20000 }
                    },
                    "required": ["operation", "value"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "list" },
                        "pid": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "foreground" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "focus" }, "title": { "type": "string", "minLength": 1 }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation", "title"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "focus" }, "title": { "type": "string", "minLength": 1 }, "pid": { "type": "integer", "minimum": 1 } },
                    "required": ["operation", "title", "pid"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "focus" }, "pid": { "type": "integer", "minimum": 1 }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation", "pid"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "focus" }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation", "window_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "close" }, "title": { "type": "string", "minLength": 1 }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation", "title"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "close" }, "title": { "type": "string", "minLength": 1 }, "pid": { "type": "integer", "minimum": 1 } },
                    "required": ["operation", "title", "pid"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "close" }, "pid": { "type": "integer", "minimum": 1 }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation", "pid"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "close" }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation", "window_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "screenshot" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "ocr" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "ui_tree" }, "title": { "type": "string", "minLength": 1 }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "wait" },
                        "condition": { "type": "string", "enum": ["title_contains", "foreground_contains", "ui_text"] },
                        "text": { "type": "string", "minLength": 1 },
                        "title": { "type": "string", "minLength": 1 },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 }
                    },
                    "required": ["operation", "condition", "text"]
                }
            ]
        });
        let ocr_available = self
            .media_tool
            .as_ref()
            .is_some_and(|media_tool| media_tool.ocr_available());
        if !ocr_available {
            if let Some(operations) = schema["properties"]["operation"]
                .get_mut("enum")
                .and_then(Value::as_array_mut)
            {
                operations.retain(|operation| operation.as_str() != Some("ocr"));
            }
            if let Some(branches) = schema.get_mut("oneOf").and_then(Value::as_array_mut) {
                branches.retain(|branch| {
                    branch["properties"]["operation"]["const"].as_str() != Some("ocr")
                });
            }
        }
        schema
    }

    /// Entry ②: LLM JSON entry — convert/validate into `WindowParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<WindowParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}
