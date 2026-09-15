use async_trait::async_trait;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    StructuredToolError, Tool, ToolCancellationPolicy, ToolConcurrency, ToolErrorMetadata,
    ToolOperationMetadata, ToolOperationScope, ToolResult, TypedToolOperation,
};

const MAX_QUESTION_CHARS: usize = 2_000;
const MAX_CONTEXT_CHARS: usize = 4_000;
const MAX_OPTIONS: usize = 8;
const MAX_OPTION_CHARS: usize = 120;

/// Let the agent ask the human a question when it is unsure how to proceed.
///
/// When this tool runs, the ReAct loop pauses the session and surfaces the
/// question to the user as a chat observation. The user's next message arrives
/// as a supplement (see `process_input` → `add_supplement` → Paused→Pending)
/// and is injected into context on resume, so the model sees both the question
/// it asked and the user's answer, then continues.
pub struct AskTool;

/// Typed parameters for `AskTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `AskTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AskParams {
    /// The question to ask the human.
    pub question: String,
    /// Optional suggested answers, each becomes a quick-reply button.
    #[serde(default)]
    pub options: Vec<String>,
    /// Optional context: why you are asking and what you have considered.
    #[serde(default)]
    pub context: Option<String>,
}

impl AskTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: AskParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let question = params.question.trim().to_string();
        if question.is_empty() {
            anyhow::bail!("question must not be empty");
        }
        if question.chars().count() > MAX_QUESTION_CHARS {
            anyhow::bail!("question must be at most {MAX_QUESTION_CHARS} characters");
        }
        let context = params.context;
        if context
            .as_deref()
            .is_some_and(|value| value.chars().count() > MAX_CONTEXT_CHARS)
        {
            anyhow::bail!("context must be at most {MAX_CONTEXT_CHARS} characters");
        }
        let options: Vec<String> = params
            .options
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if options.len() > MAX_OPTIONS {
            anyhow::bail!("options must contain at most {MAX_OPTIONS} items");
        }
        if options
            .iter()
            .any(|option| option.chars().count() > MAX_OPTION_CHARS)
        {
            anyhow::bail!("each option must be at most {MAX_OPTION_CHARS} characters");
        }

        // The `ask` flag is the signal the ReAct loop keys on to pause the
        // session and wait for the user's reply (delivered as a supplement).
        Ok(ToolResult::ok(serde_json::json!({
            "ask": true,
            "question": question,
            "context": context,
            "options": options,
            "hint": "The session is paused. The user's next message will be used as the answer and the session will resume.",
        })))
    }
}

#[async_trait]
impl TypedToolOperation for AskTool {
    type Args = AskParams;
    type Output = Value;
    type Error = StructuredToolError;

    fn metadata(&self, _args: &Self::Args) -> ToolOperationMetadata {
        self.default_metadata()
    }

    fn default_metadata(&self) -> ToolOperationMetadata {
        ToolOperationMetadata {
            capability: "ask",
            operation: "ask",
            scope: ToolOperationScope::Session,
            risk_level: RiskLevel::Safe,
            idempotency: crate::OperationIdempotency::NonIdempotent,
            cancellation: ToolCancellationPolicy::Cooperative,
            timeout_secs: 30,
            concurrency: ToolConcurrency::Exclusive,
        }
    }

    fn input_schema(&self) -> Value {
        <Self as Tool>::input_schema(self)
    }

    fn signals(&self, output: &Value) -> crate::tool_contract::ToolSignals {
        let (question, options) = crate::extract_ask_signal(output);
        crate::tool_contract::ToolSignals {
            ask_question: question,
            ask_options: options,
            ..Default::default()
        }
    }

    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        error.metadata()
    }

    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        self.run(args, cancel.clone())
            .await
            .map(|result| result.output)
            .map_err(|error| {
                StructuredToolError::new(
                    error.to_string(),
                    if cancel.is_cancelled() {
                        ToolErrorMetadata {
                            class: crate::ToolErrorClass::UnknownOutcome,
                            outcome: crate::ToolExecutionOutcome::Cancelled,
                            retryability: crate::ToolRetryability::Unknown,
                        }
                    } else {
                        ToolErrorMetadata::validation()
                    },
                )
            })
    }
}

pub fn typed_adapter() -> crate::TypedToolAdapter<AskTool> {
    crate::TypedToolAdapter::new(
        "ask",
        "Ask the user one question when you need a decision or missing information. One question per call — do not pack multiple questions or mixed option sets into a single ask.",
        AskTool,
    )
    .with_catalog_group(ToolCatalogGroup::Haven)
}

#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> String {
        "ask".into()
    }

    fn description(&self) -> String {
        crate::prompts::ASK_DESCRIPTION.into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Haven
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        // Asking the user is harmless and never touches the system.
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "question": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 2000,
                    "description": "A single question for the human. Be specific and concise. Never put two questions in one string — call ask again for the next question."
                },
                "options": {
                    "type": "array",
                    "maxItems": 8,
                    "items": { "type": "string", "minLength": 1, "maxLength": 120 },
                    "description": "Optional short suggested answers for THIS question only. Each becomes a selectable chip; keep them terse (a few words). Do not mix answers that belong to a different question."
                },
                "context": {
                    "type": "string",
                    "maxLength": 4000,
                    "description": "Optional context: why you are asking and what you have considered so far."
                }
            },
            "required": ["question"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `AskParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<AskParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }

    /// Declare the question signal so the ReAct loop pauses the session without
    /// name-matching "ask" or re-parsing the output.
    fn signals(&self, output: &Value) -> crate::tool_contract::ToolSignals {
        let (question, options) = crate::extract_ask_signal(output);
        crate::tool_contract::ToolSignals {
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
        let schema = <AskTool as Tool>::input_schema(&AskTool);
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "question"));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["question"]["maxLength"], 2000);
        assert_eq!(schema["properties"]["options"]["maxItems"], 8);
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
    async fn test_ask_rejects_oversized_question() {
        let result = AskTool
            .run(
                AskParams {
                    question: "x".repeat(MAX_QUESTION_CHARS + 1),
                    options: Vec::new(),
                    context: None,
                },
                CancellationToken::new(),
            )
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

    #[tokio::test]
    async fn test_ask_native_entry_lands_in_run() {
        let result = AskTool
            .run(
                AskParams {
                    question: "which?".into(),
                    options: vec!["A".into(), " B ".into(), "".into()],
                    context: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["question"], "which?");
        assert_eq!(result.output["options"], serde_json::json!(["A", "B"]));
    }

    #[tokio::test]
    async fn test_ask_json_entry_rejects_wrong_type() {
        let result = AskTool
            .execute(json!({"question": 42}), CancellationToken::new())
            .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid 'ask' input"), "{err}");
    }

    #[tokio::test]
    async fn test_ask_rejects_unknown_fields_at_the_json_boundary() {
        let result = AskTool
            .execute(
                json!({"question": "continue?", "unexpected": true}),
                CancellationToken::new(),
            )
            .await;
        let error = result.expect_err("unknown ask fields must be rejected");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[tokio::test]
    async fn test_ask_adapter_cancellation_is_unknown_and_non_replayable() {
        let adapter = crate::TypedToolAdapter::new("ask", "ask", AskTool);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = adapter
            .execute(json!({"question": "continue?"}), cancel)
            .await
            .expect_err("cancelled ask must not pause the session");
        let structured = error.downcast_ref::<StructuredToolError>().unwrap();
        assert_eq!(
            structured.metadata(),
            ToolErrorMetadata {
                class: crate::ToolErrorClass::UnknownOutcome,
                outcome: crate::ToolExecutionOutcome::Cancelled,
                retryability: crate::ToolRetryability::Unknown,
            }
        );
        assert_eq!(
            adapter.idempotency(&json!({"question": "continue?"})),
            crate::OperationIdempotency::NonIdempotent
        );
        assert_eq!(
            adapter.timeout_outcome(),
            crate::ToolExecutionOutcome::TimedOutUnknown
        );
    }
}
