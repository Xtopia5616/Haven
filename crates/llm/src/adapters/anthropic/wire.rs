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
    pub(super) stop_reason: Option<String>,
    pub(super) usage: Option<AnthropicUsage>,
    pub(super) model: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
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
    /// Web Search result blocks link back to `server_tool_use` by this ID.
    #[serde(default)]
    pub(super) tool_use_id: Option<String>,
    /// Search result content is provider-opaque and must be echoed on the
    /// next request together with `encrypted_content`, when present.
    #[serde(default)]
    pub(super) content: Option<Value>,
    #[serde(default)]
    pub(super) encrypted_content: Option<String>,
    #[serde(default)]
    pub(super) error_code: Option<String>,
    /// Preserve fields added to provider block types without dropping them
    /// from the next-turn echo.
    #[serde(flatten)]
    pub(super) extra: serde_json::Map<String, Value>,
}

impl AnthropicResponseBlock {
    fn insert_optional<T: Serialize>(
        block: &mut serde_json::Map<String, Value>,
        key: &str,
        value: Option<&T>,
    ) {
        if let Some(value) = value {
            block.insert(
                key.into(),
                serde_json::to_value(value).unwrap_or(Value::Null),
            );
        }
    }

    pub(super) fn to_wire_value(&self) -> Value {
        let mut block = self.extra.clone();
        if let Some(value) = &self.block_type {
            block.insert("type".into(), Value::String(value.clone()));
        }
        Self::insert_optional(&mut block, "text", self.text.as_ref());
        Self::insert_optional(&mut block, "thinking", self.thinking.as_ref());
        Self::insert_optional(&mut block, "id", self.id.as_ref());
        Self::insert_optional(&mut block, "name", self.name.as_ref());
        Self::insert_optional(&mut block, "input", self.input.as_ref());
        Self::insert_optional(&mut block, "signature", self.signature.as_ref());
        Self::insert_optional(&mut block, "data", self.data.as_ref());
        Self::insert_optional(&mut block, "tool_use_id", self.tool_use_id.as_ref());
        Self::insert_optional(&mut block, "content", self.content.as_ref());
        Self::insert_optional(
            &mut block,
            "encrypted_content",
            self.encrypted_content.as_ref(),
        );
        Self::insert_optional(&mut block, "error_code", self.error_code.as_ref());
        Value::Object(block)
    }
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct AnthropicUsage {
    #[serde(default)]
    pub(super) input_tokens: u32,
    #[serde(default)]
    pub(super) output_tokens: u32,
    #[serde(default)]
    pub(super) cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    pub(super) cache_creation_input_tokens: Option<u32>,
}

impl AnthropicUsage {
    pub(super) fn cache_read_tokens_reported(&self) -> Option<u32> {
        self.cache_read_input_tokens
    }

    pub(super) fn cache_read_tokens(&self) -> u32 {
        self.cache_read_input_tokens.unwrap_or_default()
    }

    pub(super) fn cache_creation_tokens(&self) -> u32 {
        self.cache_creation_input_tokens.unwrap_or_default()
    }

    pub(super) fn cache_usage_reported(&self) -> bool {
        self.cache_read_input_tokens.is_some() || self.cache_creation_input_tokens.is_some()
    }
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
    /// Provider-specific fields such as `content`, `tool_use_id`, and
    /// `encrypted_content` on web search result blocks.
    #[serde(flatten)]
    pub(super) extra: serde_json::Map<String, Value>,
}

impl AnthropicStreamBlock {
    fn insert_optional<T: Serialize>(
        block: &mut serde_json::Map<String, Value>,
        key: &str,
        value: Option<&T>,
    ) {
        if let Some(value) = value {
            block.insert(
                key.into(),
                serde_json::to_value(value).unwrap_or(Value::Null),
            );
        }
    }

    pub(super) fn to_wire_value(&self) -> Value {
        let mut block = self.extra.clone();
        if let Some(value) = &self.block_type {
            block.insert("type".into(), Value::String(value.clone()));
        }
        Self::insert_optional(&mut block, "id", self.id.as_ref());
        Self::insert_optional(&mut block, "name", self.name.as_ref());
        Self::insert_optional(&mut block, "input", self.input.as_ref());
        Self::insert_optional(&mut block, "signature", self.signature.as_ref());
        Value::Object(block)
    }
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
