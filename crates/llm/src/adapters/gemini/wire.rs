//! Provider wire DTOs and event envelopes. No canonical mapping lives here.

use super::*;

// ---------------------------------------------------------------------------
// Gemini generateContent request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(super) struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<String>,
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    pub(super) inline_data: Option<Value>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    pub(super) function_call: Option<Value>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    pub(super) function_response: Option<Value>,
    /// Opaque Gemini thought signature that must be echoed on the same Part
    /// in a later stateless request.
    #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    pub(super) thought_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GeminiContent {
    pub(super) role: String,
    pub(super) parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GeminiFunctionDeclaration {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: Value,
}

/// Gemini tools are a heterogeneous list: function declarations and built-in
/// tools such as `google_search` grounding share the same `tools[]` array.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(super) enum GeminiTool {
    Functions {
        #[serde(rename = "functionDeclarations")]
        function_declarations: Vec<GeminiFunctionDeclaration>,
    },
    GoogleSearch {
        #[serde(rename = "googleSearch")]
        google_search: Value,
    },
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GeminiGenerationConfig {
    pub(super) temperature: f32,
    #[serde(rename = "maxOutputTokens")]
    pub(super) max_output_tokens: u32,
    #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
    pub(super) top_p: Option<f32>,
    #[serde(rename = "topK", skip_serializing_if = "Option::is_none")]
    pub(super) top_k: Option<u32>,
    #[serde(rename = "stopSequences", skip_serializing_if = "Option::is_none")]
    pub(super) stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GeminiRequest {
    pub(super) contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub(super) system_instruction: Option<Value>,
    /// Explicit Gemini context-cache resource. When present, Gemini requires
    /// the cached system instruction and tools to be omitted from this request.
    #[serde(rename = "cachedContent", skip_serializing_if = "Option::is_none")]
    pub(super) cached_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<GeminiTool>>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    pub(super) generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip)]
    pub(super) cache_diagnostics: CacheDiagnostics,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiCachedContentResponse {
    pub(super) name: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct GeminiCachedContentCreateRequest<'a> {
    pub(super) model: String,
    #[serde(rename = "systemInstruction")]
    pub(super) system_instruction: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<&'a [GeminiTool]>,
    pub(super) ttl: String,
    #[serde(rename = "displayName")]
    pub(super) display_name: String,
}

// Response types: `text` and `function_call` parts, plus usage metadata.
#[derive(Debug, Deserialize)]
pub(super) struct GeminiResponse {
    pub(super) candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata", alias = "usage_metadata", default)]
    pub(super) usage_metadata: Option<GeminiUsage>,
    #[serde(rename = "modelVersion", alias = "model_version", default)]
    pub(super) model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiCandidate {
    #[serde(default)]
    pub(super) content: Option<GeminiResponseContent>,
    #[serde(alias = "finishReason")]
    #[serde(alias = "finish_reason")]
    pub(super) finish_reason: Option<String>,
    #[serde(default, alias = "groundingMetadata")]
    pub(super) grounding_metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiResponseContent {
    #[serde(default)]
    pub(super) parts: Vec<GeminiResponsePart>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiResponsePart {
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(rename = "functionCall", alias = "function_call", default)]
    pub(super) function_call: Option<GeminiFunctionCall>,
    /// Gemini thinking-mode marker: parts carrying `"thought": true` hold the
    /// model's internal reasoning and MUST NOT be shown as assistant text.
    #[serde(default)]
    pub(super) thought: Option<bool>,
    /// Opaque signature returned by Gemini for thought-bearing parts and
    /// function calls. It must be echoed verbatim in the next request.
    #[serde(rename = "thoughtSignature", alias = "thought_signature", default)]
    pub(super) thought_signature: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiFunctionCall {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) args: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct GeminiUsage {
    #[serde(default, alias = "promptTokenCount")]
    pub(super) prompt_tokens: u32,
    #[serde(default, alias = "candidatesTokenCount")]
    pub(super) candidates_tokens: u32,
    #[serde(default, alias = "totalTokenCount")]
    pub(super) total_tokens: u32,
    #[serde(default, alias = "cachedContentTokenCount")]
    pub(super) cached_tokens: u32,
    #[serde(default, alias = "thoughtsTokenCount")]
    pub(super) thoughts_tokens: u32,
    #[serde(default, alias = "toolUsePromptTokenCount")]
    pub(super) tool_use_prompt_tokens: u32,
}

impl GeminiUsage {
    pub(super) fn to_usage(&self, model_name: Option<String>) -> Usage {
        let completion = self.candidates_tokens.saturating_add(self.thoughts_tokens);
        let mut prompt = self.prompt_tokens;
        let tool_use = self.tool_use_prompt_tokens;
        if tool_use > 0 {
            let folded = prompt.saturating_add(completion);
            if self.total_tokens == folded.saturating_add(tool_use) {
                prompt = prompt.saturating_add(tool_use);
            }
        }
        Usage::from_counts_with_accounting(
            prompt,
            completion,
            self.total_tokens,
            self.cached_tokens,
            0,
            CacheAccounting::Inclusive,
            model_name,
        )
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiEmbedValues {
    pub(super) values: Vec<f32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct GeminiEmbedResponse {
    #[serde(default)]
    pub(super) embeddings: Vec<GeminiEmbedValues>,
    #[serde(default)]
    pub(super) embedding: Option<GeminiEmbedValues>,
}
