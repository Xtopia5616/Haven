use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use haven_common::SttClient;
use std::time::Duration;

use crate::mcp::McpManager;

/// STT client that routes transcription through an MCP server.
/// Calls the `stt.transcribe` tool with base64-encoded WAV audio.
pub struct McpSttClient {
    manager: McpManager,
    server_name: String,
    timeout: Duration,
}

impl McpSttClient {
    pub fn new(manager: McpManager, server_name: &str, timeout_secs: u64) -> Self {
        Self {
            manager,
            server_name: server_name.into(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

#[async_trait]
impl SttClient for McpSttClient {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(wav_data);

        let input = serde_json::json!({
            "audio": audio_b64,
        });

        let cancel = tokio_util::sync::CancellationToken::new();

        let result = tokio::time::timeout(self.timeout, async {
            self.manager
                .call_tool(&self.server_name, "stt.transcribe", input, cancel)
                .await
        })
        .await
        .map_err(|_| anyhow::anyhow!("STT transcription timed out after {:?}", self.timeout))??;

        if !result.success {
            let msg = result.error.unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("STT transcription failed: {}", msg);
        }

        let text = result.output["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("STT response missing 'text' field"))?
            .to_string();

        Ok(text)
    }
}

/// Fallback STT adapter that uses an LLM with multimodal input.
/// M2: skeleton only — returns an error indicating not yet implemented.
pub struct LlmSttAdapter;

#[async_trait]
impl SttClient for LlmSttAdapter {
    async fn transcribe(&self, _wav_data: &[u8]) -> Result<String> {
        Err(anyhow::anyhow!(
            "LLM-based STT is not yet implemented; configure an MCP STT server"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct MockSttClient {
        response: String,
        call_count: AtomicU64,
    }

    #[async_trait]
    impl SttClient for MockSttClient {
        async fn transcribe(&self, _wav_data: &[u8]) -> Result<String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_mock_stt_returns_text() {
        let client = MockSttClient {
            response: "hello world".into(),
            call_count: AtomicU64::new(0),
        };
        let wav = vec![0u8; 44]; // minimal WAV header
        let result = client.transcribe(&wav).await.unwrap();
        assert_eq!(result, "hello world");
        assert_eq!(client.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_returns_error() {
        let adapter = LlmSttAdapter;
        let result = adapter.transcribe(b"fake wav").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not yet implemented")
        );
    }
}
