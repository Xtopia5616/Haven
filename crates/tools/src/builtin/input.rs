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

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let op = input["operation"].as_str().ok_or_else(|| {
            anyhow::anyhow!("operation is required (type, key, click, move, scroll)")
        })?;
        let result = match op {
            "type" => {
                let text = input["text"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("text is required for type"))?;
                let chars = text.chars().count();
                crate::simulate::type_text(text)?;
                serde_json::json!({ "typed": text, "chars": chars })
            }
            "key" => {
                let key = input["key"]
                    .as_str()
                    .filter(|k| !k.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("key is required for key"))?;
                crate::simulate::press_key(key)?;
                serde_json::json!({ "pressed": key })
            }
            "click" => {
                let x = input["x"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("x is required for click"))?;
                let y = input["y"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("y is required for click"))?;
                let button = input["button"].as_str().unwrap_or("left");
                crate::simulate::click(x, y, button)?;
                serde_json::json!({ "clicked": [x, y], "button": button })
            }
            "move" => {
                let x = input["x"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("x is required for move"))?;
                let y = input["y"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("y is required for move"))?;
                crate::simulate::move_to(x, y)?;
                serde_json::json!({ "moved_to": [x, y] })
            }
            "scroll" => {
                let delta = input["delta"].as_i64().unwrap_or(1).clamp(-100, 100);
                crate::simulate::scroll(delta)?;
                serde_json::json!({ "scrolled": delta })
            }
            _ => anyhow::bail!("unknown input operation: {}", op),
        };
        Ok(ToolResult::ok(result))
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
}
