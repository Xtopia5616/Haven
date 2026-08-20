pub mod adapters;
pub mod auth;
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

pub use adapters::{AnthropicAdapter, OpenAiAdapter};
pub use auth::AuthResolver;
pub use client::{LlmClient, with_retry};
pub use image_gen::{GeneratedImage, ImageGenClient, build_image_gen_client};
pub use ocr::{OcrClient, OcrResult, build_ocr_client};
pub use registry::{ModelInfo, ModelRegistry, context_window_for};
pub use router::{EndpointRole, LlmRouter};
pub use stt::{McpToolCaller, McpToolOutcome, SttClient, build_stt_client};
pub use tts::{TtsClient, build_tts_client};
pub use types::{
    FinishReason, LlmConnectionStatus, LlmError, LlmResponse, StreamChunk, SttResult,
    ToolDefinition, ToolFunction, Usage,
};
