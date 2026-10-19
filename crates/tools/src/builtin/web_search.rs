//! Model-callable bridge to the selected provider's standalone search API.

use std::sync::Arc;

use async_trait::async_trait;
use haven_common::config::RequestKind;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    OperationIdempotency, StructuredToolError, Tool, ToolCancellationPolicy, ToolConcurrency,
    ToolErrorClass, ToolErrorMetadata, ToolExecutionOutcome, ToolOperationMetadata,
    ToolOperationScope, ToolResult, TypedToolOperation,
};

const MAX_QUERY_CHARS: usize = 512;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchParams {
    pub query: String,
    #[serde(default)]
    pub original_question: Option<String>,
}

pub struct WebSearchTool {
    router: Arc<haven_llm::LlmRouter>,
}

impl WebSearchTool {
    pub fn new(router: Arc<haven_llm::LlmRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl TypedToolOperation for WebSearchTool {
    type Args = WebSearchParams;
    type Output = Value;
    type Error = StructuredToolError;

    fn metadata(&self, _args: &Self::Args) -> ToolOperationMetadata {
        self.default_metadata()
    }

    fn default_metadata(&self) -> ToolOperationMetadata {
        ToolOperationMetadata {
            capability: "web_search_api",
            operation: "search",
            scope: ToolOperationScope::Session,
            risk_level: RiskLevel::Safe,
            idempotency: OperationIdempotency::Idempotent,
            cancellation: ToolCancellationPolicy::Cooperative,
            timeout_secs: 120,
            concurrency: ToolConcurrency::ReadOnly,
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_QUERY_CHARS,
                    "description": "A concise web search query. DeepSeek Search accepts at most 128 characters."
                },
                "original_question": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 128,
                    "description": "The user's original question that led to this search. Forwarded only when DeepSeek Search has original-question forwarding enabled."
                }
            },
            "required": ["query"]
        })
    }

    fn error_metadata(&self, error: &Self::Error) -> ToolErrorMetadata {
        error.metadata()
    }

    async fn execute_typed(
        &self,
        args: Self::Args,
        cancel: CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        let query = args.query.trim();
        if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
            return Err(StructuredToolError::new(
                format!("query must contain 1 to {MAX_QUERY_CHARS} characters"),
                ToolErrorMetadata::validation(),
            ));
        }
        let original_question = args.original_question.as_deref().map(str::trim);
        if original_question
            .is_some_and(|question| question.is_empty() || question.chars().count() > 128)
        {
            return Err(StructuredToolError::new(
                "original_question must contain 1 to 128 characters",
                ToolErrorMetadata::validation(),
            ));
        }
        match self
            .router
            .independent_web_search(RequestKind::Chat, query, original_question, cancel.clone())
            .await
        {
            Ok(output) => Ok(output),
            Err(error) => Err(map_search_error(error, cancel.is_cancelled())),
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> String {
        "web_search_api".into()
    }

    fn description(&self) -> String {
        "Search the web using the standalone API configured for the current model. Use for current facts and include returned source links in the answer. For DeepSeek, include original_question when the search query reformulates the user's request.".into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::Network
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        <Self as TypedToolOperation>::input_schema(self)
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let args = crate::tool_contract::parse_tool_input::<WebSearchParams>(&self.name(), input)?;
        self.execute_typed(args, cancel)
            .await
            .map(ToolResult::ok)
            .map_err(anyhow::Error::msg)
    }
}

fn map_search_error(error: haven_llm::LlmError, cancelled: bool) -> StructuredToolError {
    if cancelled || matches!(&error, haven_llm::LlmError::Cancelled) {
        return StructuredToolError::new(
            "Web search was cancelled",
            ToolErrorMetadata {
                class: ToolErrorClass::UnknownOutcome,
                outcome: ToolExecutionOutcome::Cancelled,
                retryability: crate::ToolRetryability::Unknown,
            },
        );
    }
    let (message, metadata) = match error {
        haven_llm::LlmError::Timeout(_) => (
            "The web search provider timed out",
            ToolErrorMetadata::transient(),
        ),
        haven_llm::LlmError::Network(_)
        | haven_llm::LlmError::RateLimit { .. }
        | haven_llm::LlmError::ServerError(_) => (
            "The web search provider is temporarily unavailable",
            ToolErrorMetadata::transient(),
        ),
        haven_llm::LlmError::UnsupportedCapability(_) => (
            "The selected model has no standalone web search API configured",
            ToolErrorMetadata::validation(),
        ),
        _ => (
            "The web search request failed; check the selected Provider configuration",
            ToolErrorMetadata::validation(),
        ),
    };
    StructuredToolError::new(message, metadata)
}

pub fn typed_adapter(router: Arc<haven_llm::LlmRouter>) -> crate::TypedToolAdapter<WebSearchTool> {
    const DESCRIPTION: &str = "Search the web using the standalone API configured for the current model. Use for current facts and include returned source links in the answer. For DeepSeek, include original_question when the search query reformulates the user's request.";
    let mut spec = crate::operation_view::root_tool_spec(
        "web_search_api",
        DESCRIPTION,
        <WebSearchTool as TypedToolOperation>::input_schema(&WebSearchTool::new(router.clone())),
        RiskLevel::Safe,
        OperationIdempotency::Idempotent,
        ToolOperationScope::Session,
        ToolConcurrency::ReadOnly,
        ToolCatalogGroup::Network,
        None,
    );
    spec.presentation.label = "独立联网搜索".into();
    spec.presentation.renderer = "web_search".into();
    spec.presentation.icon = "search".into();
    if let Some(identity) = spec.identity.as_mut() {
        identity.root_presentation.label = "联网搜索".into();
        identity.root_presentation.description = "调用已配置 Provider 的独立搜索 API".into();
        identity.root_presentation.icon = "search".into();
    }
    crate::TypedToolAdapter::new("web_search_api", DESCRIPTION, WebSearchTool::new(router))
        .with_operation_spec(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_search_tool_has_bounded_query_schema() {
        let router = Arc::new(haven_llm::LlmRouter::new(Default::default()));
        let adapter = typed_adapter(router);
        let schema = adapter.input_schema();
        assert_eq!(schema["properties"]["query"]["maxLength"], MAX_QUERY_CHARS);
        assert_eq!(schema["additionalProperties"], false);
        let manifest = adapter.tool_manifest();
        assert_eq!(manifest.identity.stable_name, "web_search_api");
        assert_eq!(manifest.presentation.renderer, "web_search");
    }

    #[test]
    fn provider_errors_are_sanitized_for_model_observations() {
        let error = map_search_error(
            haven_llm::LlmError::Auth("sensitive provider payload".into()),
            false,
        );
        assert!(!error.to_string().contains("sensitive provider payload"));
    }
}
