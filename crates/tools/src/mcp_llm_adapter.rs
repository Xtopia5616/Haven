//! Composition adapter from the LLM-owned MCP caller contract to the MCP
//! manager. Keeping this bridge in Tools prevents the generic MCP crate from
//! depending on its STT consumer.

use haven_mcp::{McpCallOutput, McpManager};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct LlmMcpToolCallerAdapter {
    manager: McpManager,
}

#[async_trait::async_trait]
impl haven_llm::McpToolCaller for LlmMcpToolCallerAdapter {
    async fn invoke_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        input: serde_json::Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<haven_llm::McpToolOutcome> {
        self.manager
            .call_tool(server_name, tool_name, input, cancel)
            .await
            .map(into_llm_outcome)
    }
}

pub(crate) fn adapt(manager: McpManager) -> Arc<dyn haven_llm::McpToolCaller> {
    Arc::new(LlmMcpToolCallerAdapter { manager })
}

fn into_llm_outcome(output: McpCallOutput) -> haven_llm::McpToolOutcome {
    haven_llm::McpToolOutcome {
        success: output.success,
        error: output.error,
        output: output.output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_llm::McpToolCaller;

    #[test]
    fn maps_mcp_call_output_to_llm_outcome_without_losing_fields() {
        let mapped = into_llm_outcome(McpCallOutput {
            success: false,
            output: serde_json::json!({"text": "transcription failed"}),
            error: Some("provider unavailable".into()),
        });

        assert!(!mapped.success);
        assert_eq!(
            mapped.output,
            serde_json::json!({"text": "transcription failed"})
        );
        assert_eq!(mapped.error.as_deref(), Some("provider unavailable"));
    }

    #[tokio::test]
    async fn caller_preserves_manager_lookup_errors() {
        let caller = LlmMcpToolCallerAdapter {
            manager: McpManager::new(),
        };

        let error = caller
            .invoke_tool(
                "missing-server",
                "transcribe",
                serde_json::json!({}),
                CancellationToken::new(),
            )
            .await
            .err()
            .expect("manager lookup should fail for an unknown server");

        assert!(error.to_string().contains("missing-server"));
    }
}
