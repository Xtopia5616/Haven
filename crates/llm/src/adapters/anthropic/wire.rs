//! Provider wire DTOs and event envelopes. No canonical mapping lives here.

use super::*;

// ---------------------------------------------------------------------------
// Anthropic Messages API request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct AnthropicMessage {
    pub(super) role: String,
    pub(super) content: Value,
}

#[derive(Debug, Serialize)]
pub(super) struct AnthropicRequest {
    pub(super) model: String,
    pub(super) max_tokens: u32,
    pub(super) messages: Vec<AnthropicMessage>,
    /// Plain string or array of `{type:text, text, cache_control?}` blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) system: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stop_sequences: Option<Vec<String>>,
    /// Client function tools and Anthropic server tools (`web_search_*`) share
    /// this array as raw JSON objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<Value>,
    /// Claude 4.6+ uses adaptive thinking; earlier thinking-capable Claude
    /// models use the fixed `budget_tokens` form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) output_config: Option<Value>,
    pub(super) stream: bool,
    #[serde(skip)]
    pub(super) cache_diagnostics: CacheDiagnostics,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicResponse {
    #[serde(default)]
    pub(super) content: Vec<AnthropicResponseBlock>,
    #[serde(alias = "stop_reason")]
    pub(super) stop_reason: Option<String>,
    pub(super) usage: Option<AnthropicUsage>,
    pub(super) model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicResponseBlock {
    #[serde(rename = "type")]
    pub(super) block_type: Option<String>,
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) thinking: Option<String>,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) input: Option<Value>,
    /// Thinking-block signature. Anthropic validates it against the thinking
    /// text on echo, so it must be captured and passed back verbatim.
    #[serde(default)]
    pub(super) signature: Option<String>,
    /// `redacted_thinking` payload. Redacted thinking blocks must also be
    /// echoed back verbatim on tool-use turns.
    #[serde(default)]
    pub(super) data: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct AnthropicUsage {
    #[serde(default)]
    pub(super) input_tokens: u32,
    #[serde(default)]
    pub(super) output_tokens: u32,
    #[serde(default)]
    pub(super) cache_read_input_tokens: u32,
    #[serde(default)]
    pub(super) cache_creation_input_tokens: u32,
}

// Streaming SSE events (https://docs.anthropic.com/en/api/messages-streaming)
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum AnthropicStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart {
        message: AnthropicStreamStartMessage,
    },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: AnthropicStreamBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: usize,
        delta: AnthropicStreamDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: AnthropicStreamDeltaMeta,
        usage: Option<AnthropicUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicStreamError },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicStreamStartMessage {
    pub(super) model: Option<String>,
    pub(super) usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicStreamBlock {
    #[serde(rename = "type")]
    pub(super) block_type: Option<String>,
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) input: Option<Value>,
    /// Thinking-block signature delivered on the `content_block_start` event.
    #[serde(default)]
    pub(super) signature: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum AnthropicStreamDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicStreamDeltaMeta {
    pub(super) stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AnthropicStreamError {
    #[serde(rename = "type")]
    pub(super) error_type: Option<String>,
    #[serde(default)]
    pub(super) message: Option<String>,
}
