use async_trait::async_trait;
use haven_common::types::{DesktopAccess, RiskLevel, WindowAccess};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::ui_automation::{UIA_CONTROL_TYPE_NAMES, window_element_query};
use super::{WindowParams, WindowTool};
use crate::security::DeclaredResource;
use crate::{Tool, ToolConcurrency, ToolExecutionContext, ToolResult};

struct ResolvedWindowTarget {
    window_id: String,
    process_id: u32,
    element_token: Option<String>,
}

struct WindowAuthorizationResources {
    resources: Vec<DeclaredResource>,
    target: Option<ResolvedWindowTarget>,
}

fn resolve_window_authorization(
    params: &WindowParams,
) -> Result<WindowAuthorizationResources, String> {
    let operation = params.operation.unwrap_or(super::WindowOperation::List);
    let mut resources = Vec::new();
    let target_access = match operation {
        super::WindowOperation::Focus => Some(WindowAccess::Interact),
        super::WindowOperation::Close => Some(WindowAccess::Close),
        super::WindowOperation::UiTree | super::WindowOperation::Observe => {
            Some(WindowAccess::Read)
        }
        super::WindowOperation::Wait
            if params.condition == Some(super::WaitCondition::UiText)
                && params
                    .title
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()) =>
        {
            Some(WindowAccess::Read)
        }
        super::WindowOperation::Invoke
        | super::WindowOperation::SetValue
        | super::WindowOperation::Toggle
        | super::WindowOperation::Select => Some(WindowAccess::Interact),
        _ => None,
    };

    let target = if let Some(access) = target_access {
        let target = match operation {
            super::WindowOperation::Invoke
            | super::WindowOperation::SetValue
            | super::WindowOperation::Toggle
            | super::WindowOperation::Select => {
                let query = window_element_query(params).map_err(|error| error.to_string())?;
                let element =
                    super::resolve_ui_element(&query).map_err(|error| error.to_string())?;
                let (window_id, process_id) =
                    super::platform::resolve_window_identity(Some(&element.window_id), None, None)
                        .map_err(|error| error.to_string())?;
                ResolvedWindowTarget {
                    window_id,
                    process_id,
                    element_token: Some(element.element_token),
                }
            }
            super::WindowOperation::Focus | super::WindowOperation::Close => {
                let title = params
                    .title
                    .as_deref()
                    .filter(|value| !value.trim().is_empty());
                let process_id = validated_process_id(params)?;
                let (window_id, process_id) = super::platform::resolve_window_identity(
                    params.window_id.as_deref(),
                    title,
                    process_id,
                )
                .map_err(|error| error.to_string())?;
                ResolvedWindowTarget {
                    window_id,
                    process_id,
                    element_token: None,
                }
            }
            super::WindowOperation::UiTree
            | super::WindowOperation::Observe
            | super::WindowOperation::Wait => {
                let title = params
                    .title
                    .as_deref()
                    .filter(|value| !value.trim().is_empty());
                let process_id = validated_process_id(params)?;
                let (window_id, process_id) =
                    if params.window_id.is_some() || title.is_some() || process_id.is_some() {
                        super::platform::resolve_window_identity(
                            params.window_id.as_deref(),
                            title,
                            process_id,
                        )
                    } else {
                        super::platform::resolve_foreground_window_identity()
                    }
                    .map_err(|error| error.to_string())?;
                ResolvedWindowTarget {
                    window_id,
                    process_id,
                    element_token: None,
                }
            }
            _ => unreachable!("only targeted operations request a window resource"),
        };
        let element_fingerprint = target
            .element_token
            .as_deref()
            .map(fingerprint_element_token);
        resources.push(DeclaredResource::Window {
            window_id: target.window_id.clone(),
            process_id: target.process_id,
            access,
            element_fingerprint,
        });
        Some(target)
    } else {
        None
    };

    match operation {
        super::WindowOperation::List | super::WindowOperation::Foreground => {
            resources.push(DeclaredResource::Desktop {
                access: DesktopAccess::Read,
            })
        }
        super::WindowOperation::Wait if target.is_none() => {
            resources.push(DeclaredResource::Desktop {
                access: DesktopAccess::Read,
            })
        }
        super::WindowOperation::Screenshot => resources.push(DeclaredResource::Desktop {
            access: DesktopAccess::Capture,
        }),
        super::WindowOperation::Observe => resources.push(DeclaredResource::Desktop {
            access: DesktopAccess::Capture,
        }),
        _ => {}
    }
    Ok(WindowAuthorizationResources { resources, target })
}

fn validated_process_id(params: &WindowParams) -> Result<Option<u32>, String> {
    match params.pid {
        Some(pid) if (1..=u32::MAX as i64).contains(&pid) => Ok(Some(pid as u32)),
        Some(_) => Err("pid must be a positive 32-bit integer".into()),
        None => Ok(None),
    }
}

fn fingerprint_element_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

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
            Some("focus") => RiskLevel::Medium,
            Some("ui_tree") | Some("observe") | Some("wait") => RiskLevel::Low,
            Some("invoke") | Some("set_value") | Some("toggle") | Some("select") => {
                RiskLevel::Medium
            }
            _ => RiskLevel::Low,
        }
    }

    fn authorization_resources(&self, input: &Value) -> Result<Vec<DeclaredResource>, String> {
        let mut input = input.clone();
        crate::tool_contract::strip_private_tool_fields(&mut input);
        let params = crate::tool_contract::parse_tool_input::<WindowParams>(&self.name(), input)
            .map_err(|error| error.to_string())?;
        Ok(resolve_window_authorization(&params)?.resources)
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
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["list", "foreground", "focus", "close", "screenshot", "ui_tree", "observe", "invoke", "set_value", "toggle", "select", "wait"] },
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
                        "pid": { "type": "integer", "minimum": 1 }
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
                        "pid": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295u64 },
                        "condition": { "const": "title_contains" },
                        "title": { "type": "string", "minLength": 1 }
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
                    "properties": { "operation": { "const": "ui_tree" }, "title": { "type": "string", "minLength": 1 }, "window_id": { "type": "string", "pattern": "^hwnd:[0-9a-f]+$" }, "pid": { "type": "integer", "minimum": 1, "maximum": 4_294_967_295u64 } },
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
        schema
    }

    /// Entry ②: LLM JSON entry — convert/validate into `WindowParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<WindowParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }

    async fn execute_with_authorization_context(
        &self,
        input: Value,
        cancel: CancellationToken,
        target_binding: Option<&str>,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<ToolResult> {
        if target_binding.is_some() {
            anyhow::bail!("window operations do not accept a remote execution target binding");
        }
        let session_id = input
            .get("_session_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut authorization_input = input;
        crate::tool_contract::strip_private_tool_fields(&mut authorization_input);
        let mut params = crate::tool_contract::parse_tool_input::<WindowParams>(
            &self.name(),
            authorization_input,
        )?;
        params.session_id = session_id;
        let resolved = resolve_window_authorization(&params).map_err(anyhow::Error::msg)?;
        if resolved.resources != context.authorized_resources() {
            anyhow::bail!("window resources no longer match their authorization proof");
        }
        let _permit = context
            .acquire_execution_policy_permit()
            .await
            .map_err(anyhow::Error::msg)?
            .ok_or_else(|| anyhow::anyhow!("window access requires proof-derived authorization"))?;
        if let Some(target) = resolved.target {
            params.window_id = Some(target.window_id);
            params.pid = Some(target.process_id as i64);
            if let Some(element_token) = target.element_token {
                params.element_token = Some(element_token);
            }
        }
        self.run(params, cancel).await
    }
}
