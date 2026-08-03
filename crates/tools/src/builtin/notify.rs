use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Let the agent notify the user via the in-app toast AND a Windows desktop
/// notification, delivered together.
///
/// When this tool runs, the ReAct loop detects the `notify` signal in the
/// structured output and emits an `AgentEvent::Notification`, which the app
/// surfaces both in-app (chat toast) and as a native Windows notification.
/// Unlike `ask`, this does not pause the task — the loop keeps running.
pub struct NotifyTool;

#[async_trait]
impl Tool for NotifyTool {
    fn name(&self) -> String {
        "notify".into()
    }

    fn description(&self) -> String {
        "Send the user a notification.Use it to alert the use about something they should check without pausing the task".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        // Sending a notification is harmless and never touches the system.
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short notification title. Defaults to 'Haven'."
                },
                "body": {
                    "type": "string",
                    "description": "The notification message body shown to the user. Keep it concise."
                }
            },
            "required": ["body"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let body = input["body"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("body is required for notify"))?
            .trim()
            .to_string();
        if body.is_empty() {
            anyhow::bail!("body must not be empty");
        }
        let title = input["title"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Haven".to_string());

        // The `notify` flag is the signal the ReAct loop keys on to emit the
        // Notification event (in-app toast + Windows notification).
        Ok(ToolResult::ok(serde_json::json!({
            "notify": true,
            "title": title,
            "body": body,
            "delivered_to": ["in_app", "windows"],
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_notify_name() {
        assert_eq!(NotifyTool.name(), "notify");
    }

    #[test]
    fn test_notify_risk_is_safe() {
        assert_eq!(
            NotifyTool.risk_level(&json!({"body": "x"})),
            RiskLevel::Safe
        );
    }

    #[test]
    fn test_notify_schema_requires_body() {
        let schema = NotifyTool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "body"));
    }

    #[tokio::test]
    async fn test_notify_returns_signal_with_default_title() {
        let result = NotifyTool
            .execute(json!({"body": "Done!"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["notify"], true);
        assert_eq!(result.output["title"], "Haven");
        assert_eq!(result.output["body"], "Done!");
        assert_eq!(result.output["delivered_to"][0], "in_app");
        assert_eq!(result.output["delivered_to"][1], "windows");
    }

    #[tokio::test]
    async fn test_notify_custom_title() {
        let result = NotifyTool
            .execute(
                json!({"title": "Build", "body": "Compilation finished"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["title"], "Build");
        assert_eq!(result.output["body"], "Compilation finished");
    }

    #[tokio::test]
    async fn test_notify_rejects_missing_body() {
        let result = NotifyTool
            .execute(json!({"title": "x"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_rejects_empty_body() {
        let result = NotifyTool
            .execute(json!({"body": "   "}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = NotifyTool.execute(json!({"body": "x"}), cancel).await;
        assert!(result.is_err());
    }
}
