use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

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
    Key,
    Click,
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
        let result = match params.operation {
            InputOperation::Type => {
                let text = params
                    .text
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("text is required for type"))?;
                let chars = text.chars().count();
                crate::simulate::type_text(text)?;
                serde_json::json!({ "typed": text, "chars": chars })
            }
            InputOperation::Key => {
                let key = params
                    .key
                    .as_deref()
                    .filter(|k| !k.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("key is required for key"))?;
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
        Ok(ToolResult::ok(result))
    }
}

#[async_trait]
impl Tool for InputTool {
    fn name(&self) -> String {
        "input".into()
    }

    fn description(&self) -> String {
        "Simulate keyboard/mouse input on the desktop: type, \
         key, click, \
         move, scroll. Coordinates are screen \
         pixels from the top-left."
            .into()
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
                "operation": {
                    "type": "string",
                    "enum": ["type", "key", "click", "move", "scroll"],
                    "description": "What to send"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (type only; supports any Unicode)"
                },
                "key": {
                    "type": "string",
                    "description": "Key name or chord (enter, esc, tab, ctrl+c)"
                },
                "x": {
                    "type": "integer",
                    "description": "Screen x in pixels (click/move)"
                },
                "y": {
                    "type": "integer",
                    "description": "Screen y in pixels (click/move)"
                },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Mouse button (click only; default left)"
                },
                "delta": {
                    "type": "integer",
                    "description": "Wheel steps (scroll only; positive = up/away, negative = down/toward)"
                }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `InputParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<InputParams>(&self.name(), input)?;
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
            tool.risk_level(&json!({"operation": "key"})),
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
}
