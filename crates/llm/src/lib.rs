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

pub use adapters::{WebSearchMode, web_search_result_of};
pub use client::{LlmClient, endpoint_host};
pub use image_gen::{GeneratedImage, ImageGenClient, build_image_gen_client};
pub use media::project_media_plan;
pub use ocr::{OcrClient, OcrResult, build_ocr_client};
pub use registry::{ModelDiscoveryAuthHeader, ModelInfo, ModelRegistry};
pub use router::{
    EmbeddingRouteIdentity, LlmRouter, RequestRouteDiagnostics, RequestRouteIssue,
    StreamAttemptHooks, StreamAttemptOutputDisposition,
};
pub use stt::{McpToolCaller, McpToolOutcome, SttClient, build_stt_client};
pub use tts::{TtsClient, build_tts_client};
pub use types::{
    CompleteRequest, Embedding, EmbeddingRequest, FinishReason, HealthCheckRequest, LlmCallUsage,
    LlmConnectionFailureReason, LlmConnectionReport, LlmConnectionStatus, LlmError, LlmResponse,
    LlmToolDefinition, PromptRequest, StreamChunk, StreamRequest, StreamToolCallUpdate, SttResult,
    ToolFunction, Usage, WebSearchPhase, WebSearchUpdate,
};
