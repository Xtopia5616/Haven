pub mod adapters;
pub mod client;
pub mod image_gen;
pub mod media;
pub mod ocr;
pub mod registry;
pub mod router;
pub mod stream_rules;
pub mod stt;
pub mod tts;
pub mod types;

pub use adapters::{
    AnthropicAdapter, OpenAiAdapter, WebSearchMode, is_known_api_style,
    is_openai_family_wire_style, is_stt_only_style, is_tts_only_style, normalize_api_style,
    parse_web_search_mode, supports_builtin_web_search, web_search_result_of,
};
pub use client::{LlmClient, with_retry};
pub use image_gen::{
    GeneratedImage, ImageGenClient, build_image_gen_client, resolve_image_gen_config,
};
pub use ocr::{OcrClient, OcrResult, build_ocr_client};
pub use registry::{
    FALLBACK_CONTEXT_WINDOW, ModelInfo, ModelRegistry, context_window_for, model_info_from_json,
};
pub use router::{EndpointRole, LlmRouter};
pub use stt::{McpToolCaller, McpToolOutcome, SttClient, build_stt_client, resolve_stt_config};
pub use tts::{TtsClient, build_tts_client, resolve_tts_config};
pub use types::{
    CacheAccounting, CacheDiagnostics, FinishReason, LlmConnectionStatus, LlmError, LlmResponse,
    StreamChunk, SttResult, ToolDefinition, ToolFunction, Usage,
};
