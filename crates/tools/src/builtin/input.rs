use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

const MAX_TYPED_CHARS: usize = 20_000;
const MAX_KEY_CHARS: usize = 128;

/// Simulate keyboard and mouse input on the local desktop: type text
/// (Unicode-safe), press named keys or chords (ctrl+c), click/move/scroll the
/// mouse. Everything goes through SendInput, which behaves like real input
/// from the OS perspective. Requires a desktop session — headless/CI runs
/// will error. The actual input primitives live in `crate::simulate`.
pub struct InputTool;

/// Input operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOperation {
    Type,
    TypeElement,
    Key,
    Click,
    ClickElement,
    Move,
    Scroll,
}

/// Mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputButton {
    Left,
    Right,
    Middle,
}

impl InputButton {
    pub fn as_str(&self) -> &'static str {
        match self {
            InputButton::Left => "left",
            InputButton::Right => "right",
            InputButton::Middle => "middle",
        }
    }
}

/// Typed parameters for `InputTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `InputTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct InputParams {
    /// What to send.
    pub operation: InputOperation,
    /// Text to type (type only; supports any Unicode).
    #[serde(default)]
    pub text: Option<String>,
    /// Key name or chord (enter, esc, tab, ctrl+c).
    #[serde(default)]
    pub key: Option<String>,
    /// Screen x in pixels (click/move).
    #[serde(default)]
    pub x: Option<i64>,
    /// Screen y in pixels (click/move).
    #[serde(default)]
    pub y: Option<i64>,
    /// Mouse button (click only; default left).
    #[serde(default)]
    pub button: Option<InputButton>,
    /// Optional window title substring for UI Automation element operations.
    #[serde(default)]
    pub title: Option<String>,
    /// UI Automation control name for element operations.
    #[serde(default)]
    pub name: Option<String>,
    /// Exact UI Automation control type name for element operations.
    #[serde(default)]
    pub control_type: Option<String>,
    /// Zero-based match index when the name is not unique.
    #[serde(default)]
    pub index: Option<usize>,
    /// Wheel steps (scroll only; positive = up/away, negative = down/toward).
    #[serde(default)]
    pub delta: Option<i64>,
}

impl InputTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: InputParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let operation = match params.operation {
            InputOperation::Type => "type",
            InputOperation::TypeElement => "type_element",
            InputOperation::Key => "key",
            InputOperation::Click => "click",
            InputOperation::ClickElement => "click_element",
            InputOperation::Move => "move",
            InputOperation::Scroll => "scroll",
        };
        let mut result = match params.operation {
            InputOperation::Type => type_text(&params, "type")?,
            InputOperation::TypeElement => {
                let text = required_text(&params, "type_element")?;
                let chars = text.chars().count();
                let query = element_query(&params)?;
                let _target = crate::builtin::window::focus_ui_element(&query)?;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                crate::simulate::type_text(text)?;
                // Never echo typed content into the tool result: it is
                // persisted in the step observation and may contain a
                // password, token, or other sensitive value.
                serde_json::json!({
                    "target": "element",
                    "typed": "[content redacted]",
                    "content_redacted": true,
                    "chars": chars
                })
            }
            InputOperation::Key => {
                let key = params
                    .key
                    .as_deref()
                    .filter(|k| !k.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("key is required for key"))?;
                if key.chars().count() > MAX_KEY_CHARS {
                    anyhow::bail!("key must be at most {MAX_KEY_CHARS} characters");
                }
                crate::simulate::press_key(key)?;
                serde_json::json!({ "pressed": key })
            }
            InputOperation::Click => {
                let x = params
                    .x
                    .ok_or_else(|| anyhow::anyhow!("x is required for click"))?;
                let y = params
                    .y
                    .ok_or_else(|| anyhow::anyhow!("y is required for click"))?;
                let button = params.button.unwrap_or(InputButton::Left);
                crate::simulate::click(x, y, button.as_str())?;
                serde_json::json!({ "clicked": [x, y], "button": button.as_str() })
            }
            InputOperation::ClickElement => {
                let query = element_query(&params)?;
                let target = crate::builtin::window::resolve_ui_element(&query)?;
                let button = params.button.unwrap_or(InputButton::Left);
                crate::simulate::click(target.center_x, target.center_y, button.as_str())?;
                serde_json::json!({
                    "target": "element",
                    "name": target.name,
                    "control_type": target.control_type,
                    "index": target.index,
                    "button": button.as_str()
                })
            }
            InputOperation::Move => {
                let x = params
                    .x
                    .ok_or_else(|| anyhow::anyhow!("x is required for move"))?;
                let y = params
                    .y
                    .ok_or_else(|| anyhow::anyhow!("y is required for move"))?;
                crate::simulate::move_to(x, y)?;
                serde_json::json!({ "moved_to": [x, y] })
            }
            InputOperation::Scroll => {
                let delta = params.delta.unwrap_or(1).clamp(-100, 100);
                crate::simulate::scroll(delta)?;
                serde_json::json!({ "scrolled": delta })
            }
        };
        if let Some(object) = result.as_object_mut() {
            object.insert("operation".into(), Value::String(operation.into()));
        }
        Ok(ToolResult::ok(result))
    }
}

fn required_text<'a>(params: &'a InputParams, operation: &str) -> anyhow::Result<&'a str> {
    let text = params
        .text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("text is required for {operation}"))?;
    if text.chars().count() > MAX_TYPED_CHARS {
        anyhow::bail!("text must be at most {MAX_TYPED_CHARS} characters");
    }
    Ok(text)
}

fn type_text(params: &InputParams, operation: &str) -> anyhow::Result<Value> {
    let text = required_text(params, operation)?;
    let chars = text.chars().count();
    crate::simulate::type_text(text)?;
    // Never echo typed content into the tool result: it is persisted in the
    // step observation and may contain a password, token, or other sensitive
    // value.
    Ok(serde_json::json!({
        "typed": "[content redacted]",
        "content_redacted": true,
        "chars": chars
    }))
}

fn element_query(params: &InputParams) -> anyhow::Result<crate::builtin::window::UiElementQuery> {
    let name = params
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("name is required for element input operations"))?;
    let control_type = params
        .control_type
        .as_deref()
        .map(str::trim)
        .filter(|control_type| !control_type.is_empty())
        .map(ToOwned::to_owned);
    if let Some(control_type) = control_type.as_deref()
        && !crate::builtin::window::is_known_ui_control_type(control_type)
    {
        anyhow::bail!("control_type must be one of the supported UI Automation control type names");
    }
    Ok(crate::builtin::window::UiElementQuery {
        title: params
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned),
        name: name.to_owned(),
        control_type,
        index: params.index,
    })
}

#[async_trait]
impl Tool for InputTool {
    fn name(&self) -> String {
        "input".into()
    }

    fn description(&self) -> String {
        crate::prompts::INPUT_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            // move/scroll only steer the cursor/wheel — no click-through.
            Some("move") | Some("scroll") => RiskLevel::Low,
            // typing and clicking act on whatever is in focus: worth a confirm.
            _ => RiskLevel::Medium,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["type", "type_element", "key", "click", "click_element", "move", "scroll"] },
                "text": { "type": "string", "minLength": 1 },
                "key": { "type": "string", "minLength": 1 },
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "button": { "type": "string", "enum": ["left", "right", "middle"] },
                "title": { "type": "string", "minLength": 1 },
                "name": { "type": "string", "minLength": 1 },
                "control_type": { "type": "string", "enum": crate::builtin::window::UIA_CONTROL_TYPE_NAMES },
                "index": { "type": "integer", "minimum": 0 },
                "delta": { "type": "integer", "minimum": -100, "maximum": 100 }
            },
            "required": ["operation"],
            "oneOf": [
                { "type": "object", "additionalProperties": false, "properties": { "operation": { "const": "type" }, "text": { "type": "string", "minLength": 1, "maxLength": 20000 } }, "required": ["operation", "text"] },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "type_element" },
                        "text": { "type": "string", "minLength": 1, "maxLength": 20000 },
                        "title": { "type": "string", "minLength": 1 },
                        "name": { "type": "string", "minLength": 1 },
                        "control_type": { "type": "string", "enum": crate::builtin::window::UIA_CONTROL_TYPE_NAMES },
                        "index": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["operation", "text", "name"]
                },
                { "type": "object", "additionalProperties": false, "properties": { "operation": { "const": "key" }, "key": { "type": "string", "minLength": 1, "maxLength": 128 } }, "required": ["operation", "key"] },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "click" },
                        "x": { "type": "integer" },
                        "y": { "type": "integer" },
                        "button": { "type": "string", "enum": ["left", "right", "middle"] }
                    },
                    "required": ["operation", "x", "y"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "click_element" },
                        "title": { "type": "string", "minLength": 1 },
                        "name": { "type": "string", "minLength": 1 },
                        "control_type": { "type": "string", "enum": crate::builtin::window::UIA_CONTROL_TYPE_NAMES },
                        "index": { "type": "integer", "minimum": 0 },
                        "button": { "type": "string", "enum": ["left", "right", "middle"] }
                    },
                    "required": ["operation", "name"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "move" }, "x": { "type": "integer" }, "y": { "type": "integer" } },
                    "required": ["operation", "x", "y"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "scroll" }, "delta": { "type": "integer", "minimum": -100, "maximum": 100 } },
                    "required": ["operation"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `InputParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<InputParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_input_name() {
        assert_eq!(InputTool.name(), "input");
    }

    #[test]
    fn test_input_risk_levels() {
        let tool = InputTool;
        assert_eq!(
            tool.risk_level(&json!({"operation": "move"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "scroll"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "click"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "type"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "type_element"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "key"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "click_element"})),
            RiskLevel::Medium
        );
    }

    #[tokio::test]
    async fn test_type_requires_text() {
        let err = InputTool
            .execute(json!({"operation": "type"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
        let err = InputTool
            .execute(
                json!({"operation": "type", "text": "   "}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_click_requires_coordinates() {
        let err = InputTool
            .execute(json!({"operation": "click"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_unknown_operation_rejected() {
        let err = InputTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = InputTool
            .execute(json!({"operation": "move", "x": 1, "y": 1}), cancel)
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_native_entry_lands_in_run() {
        let err = InputTool
            .run(
                InputParams {
                    operation: InputOperation::Type,
                    text: None,
                    key: None,
                    x: None,
                    y: None,
                    button: None,
                    title: None,
                    name: None,
                    control_type: None,
                    index: None,
                    delta: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("text is required for type"));
    }

    #[tokio::test]
    async fn test_json_entry_rejects_unknown_operation() {
        let err = InputTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid 'input' input"), "{msg}");
        assert!(msg.contains("unknown variant `bogus`"), "{msg}");
    }

    #[test]
    fn test_element_schema_exposes_stable_control_types() {
        let schema = InputTool.input_schema();
        assert_eq!(
            schema["properties"]["control_type"]["enum"],
            serde_json::json!(crate::builtin::window::UIA_CONTROL_TYPE_NAMES)
        );
        assert!(schema["oneOf"].as_array().unwrap().iter().any(|branch| {
            branch["properties"]["operation"]["const"] == "click_element"
                && branch["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|v| v == "name"))
        }));
    }

    #[tokio::test]
    async fn test_element_operations_require_name_and_reject_unknown_control_type() {
        let err = InputTool
            .execute(
                json!({"operation": "click_element"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("name is required"));

        let err = InputTool
            .execute(
                json!({
                    "operation": "click_element",
                    "name": "Save",
                    "control_type": "NotAControlType"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("control_type must be one"));
    }
}
