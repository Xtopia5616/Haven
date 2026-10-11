mod adapters;
mod aggregated_stream_executor;
mod call_executor;
mod client;
mod endpoint_circuit_breaker;
mod image_gen;
mod media;
mod model_directory;
mod ocr;
mod registry;
mod request_descriptor;
mod request_pipeline;
mod router;
mod stream_executor;
mod stream_rules;
mod streaming;
mod stt;
#[cfg(test)]
mod test_support;
mod tool_names;
mod tts;
mod types;

pub use adapters::{
    WebSearchMode, api_style_for, is_known_api_style, is_openai_family_wire_style,
    is_stt_only_style, is_tts_only_style, normalize_api_style, parse_web_search_mode,
    resolve_web_search_mode, supports_builtin_web_search, web_search_result_of,
};
pub use client::{LlmClient, endpoint_host};
pub use image_gen::{
    GeneratedImage, ImageGenClient, ResolvedImageGenConfig, build_image_gen_client,
    resolve_image_gen_config,
};
pub use media::project_media_plan;
pub use ocr::{OcrClient, OcrResult, build_ocr_client};
pub use registry::{
    FALLBACK_CONTEXT_WINDOW, ModelDiscoveryAuthHeader, ModelInfo, ModelRegistry,
    context_window_for, model_info_from_json,
};
pub use router::{LlmRouter, StreamAttemptHooks, StreamAttemptOutputDisposition};
pub use stt::{
    McpToolCaller, McpToolOutcome, ResolvedSttConfig, SttClient, build_stt_client,
    resolve_stt_config,
};
pub use tts::{ResolvedTtsConfig, TtsClient, build_tts_client, resolve_tts_config};
pub use types::{
    CompleteRequest, Embedding, EmbeddingRequest, FinishReason, HealthCheckRequest, LlmCallUsage,
    LlmConnectionFailureReason, LlmConnectionReport, LlmConnectionStatus, LlmError, LlmResponse,
    LlmToolDefinition, PromptRequest, StreamChunk, StreamRequest, StreamToolCallUpdate, SttResult,
    ToolFunction, Usage, WebSearchPhase, WebSearchUpdate,
};
