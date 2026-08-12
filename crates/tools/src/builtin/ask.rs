use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Let the agent ask the human a question when it is unsure how to proceed.
///
/// When this tool runs, the ReAct loop pauses the session and surfaces the
/// question to the user as a chat observation. The user's next message arrives
/// as a supplement (see `process_input` → `add_supplement` → Paused→Pending)
/// and is injected into context on resume, so the model sees both the question
/// it asked and the user's answer, then continues.
pub struct AskTool;

#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> String {
        "ask".into()
    }

    fn description(&self) -> String {
        "Ask the user a question when you need a decision or missing information".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        // Asking the user is harmless and never touches the system.
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the human. Be specific and concise."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional short suggested answers. Each becomes a quick-reply button, so keep them terse (a few words)."
                },
                "context": {
                    "type": "string",
                    "description": "Optional context: why you are asking and what you have considered so far."
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let question = input["question"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("question is required for ask"))?
            .trim()
            .to_string();
        if question.is_empty() {
            anyhow::bail!("question must not be empty");
        }
        let context = input["context"].as_str().map(|s| s.to_string());
        let options: Vec<String> = input["options"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // The `ask` flag is the signal the ReAct loop keys on to pause the
        // session and wait for the user's reply (delivered as a supplement).
        Ok(ToolResult::ok(serde_json::json!({
            "ask": true,
            "question": question,
            "context": context,
            "options": options,
            "awaiting_answer": true,
            "hint": "The session is paused. The user's next message will be used as the answer and the session will resume.",
        })))
    }

    /// Declare the question signal so the ReAct loop pauses the session without
    /// name-matching "ask" or re-parsing the output.
    fn signals(&self, output: &Value) -> crate::tool::ToolSignals {
        let (question, options) = crate::extract_ask_signal(output);
        crate::tool::ToolSignals {
            ask_question: question,
            ask_options: options,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_ask_name() {
        assert_eq!(AskTool.name(), "ask");
    }

    #[test]
    fn test_ask_risk_is_safe() {
        assert_eq!(
            AskTool.risk_level(&json!({"question": "x"})),
            RiskLevel::Safe
        );
    }

    #[test]
    fn test_ask_schema_requires_question() {
        let schema = AskTool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "question"));
    }

    #[tokio::test]
    async fn test_ask_returns_question_signal() {
        let result = AskTool
            .execute(
                json!({"question": "Which file?", "context": "two candidates"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["ask"], true);
        assert_eq!(result.output["question"], "Which file?");
        assert_eq!(result.output["context"], "two candidates");
        assert_eq!(result.output["awaiting_answer"], true);
        assert_eq!(result.output["options"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_ask_with_options() {
        let result = AskTool
            .execute(
                json!({"question": "which?", "options": ["A", "B", ""]}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["options"], serde_json::json!(["A", "B"]));
    }

    #[tokio::test]
    async fn test_ask_rejects_missing_question() {
        let result = AskTool
            .execute(json!({"context": "no question"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ask_rejects_empty_question() {
        let result = AskTool
            .execute(json!({"question": "   "}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ask_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = AskTool.execute(json!({"question": "x"}), cancel).await;
        assert!(result.is_err());
    }
}
