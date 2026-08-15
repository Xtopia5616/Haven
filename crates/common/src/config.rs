use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{ConfirmationMode, HotkeyMode, McpTransportType, RiskLevel, ShellChoice};

// ---------------------------------------------------------------------------
// Sub-config structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub max_duration_secs: u64,
    pub silence_timeout_ms: u64,
    pub vad_threshold: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            bits_per_sample: 16,
            max_duration_secs: 60,
            silence_timeout_ms: 1500,
            vad_threshold: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelEndpoint {
    pub provider: String,
    /// Wire protocol style for this endpoint. One of:
    /// - `openai-chat` (default): OpenAI `/chat/completions` compatible
    ///   (also Ollama, vLLM, DeepSeek, and most gateways)
    /// - `llama.cpp`: llama.cpp server (OpenAI-compatible `/chat/completions`)
    /// - `openai-responses`: OpenAI Responses API (`/v1/responses`)
    /// - `anthropic`: Anthropic Messages API (`/v1/messages`)
    /// - `gemini`: Google Gemini `generateContent` / `streamGenerateContent`
    ///
    /// When empty/`None`, the style is derived from `provider`
    /// (`anthropic` → anthropic, `google`/`gemini` → gemini,
    /// `llama`/`llama.cpp`/`llamacpp` → llama.cpp, otherwise openai-chat).
    #[serde(default)]
    pub api_style: Option<String>,
    pub base_url: String,
    pub api_key: String,
    #[serde(alias = "model")]
    pub model_name: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_secs: u64,
    // §2.8: additional model parameters
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<u64>,
    pub response_format: Option<serde_json::Value>,
    // §2.5: proxy support
    pub proxy_url: Option<String>,
    pub no_proxy: Option<String>,
    // §2.15: auth header customization
    #[serde(default = "default_auth_header_name")]
    pub auth_header_name: String,
    #[serde(default = "default_auth_header_prefix")]
    pub auth_header_prefix: String,
    // §2.9: streaming timeout (None = no timeout until SSE ends)
    pub timeout_streaming_secs: Option<u64>,
    // §2.8: reasoning effort for reasoning models ("low" | "medium" | "high"),
    // forwarded to OpenAI-compatible APIs as `reasoning_effort`.
    pub reasoning_effort: Option<String>,
    /// Provider built-in web search mode for Responses-API endpoints
    /// (DeepSeek etc.): `"off"` | `"auto"` | `"always"`. `None` defers to the
    /// `HAVEN_WEB_SEARCH` environment variable, then defaults to `off`
    /// (web search is opt-in).
    #[serde(default)]
    pub web_search: Option<String>,
    // §3.16: cost tracking. USD per 1K tokens (input and output). When both
    // are zero, cost is reported as None.
    pub cost_per_1k_input_tokens: f64,
    pub cost_per_1k_output_tokens: f64,
    /// True context window of the model in tokens. When unset (None), Haven
    /// resolves it from the builtin model catalog (by `model_name`), falling
    /// back to a 128K default. Used to drive context compaction and the
    /// token-usage display.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Per-endpoint override for the reasoning-echo cap (chars) sent back to
    /// OpenAI-compatible providers. `None` inherits the global
    /// `context_limits.reasoning_echo_max_chars` at router build time (see
    /// [`LlmConfig::with_reasoning_echo_cap`]). Kept out of the on-disk
    /// config when unset so the global default stays the single knob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_echo_max_chars: Option<usize>,
}

fn default_auth_header_name() -> String {
    "Authorization".into()
}

fn default_auth_header_prefix() -> String {
    "Bearer".into()
}

/// Compute USD cost for the given token counts using this endpoint's pricing.
/// Returns `None` when both pricing fields are zero (cost not configured).
pub fn compute_cost_usd(
    endpoint: &ModelEndpoint,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> Option<f64> {
    if endpoint.cost_per_1k_input_tokens <= 0.0 && endpoint.cost_per_1k_output_tokens <= 0.0 {
        return None;
    }
    let input = (prompt_tokens as f64 / 1000.0) * endpoint.cost_per_1k_input_tokens;
    let output = (completion_tokens as f64 / 1000.0) * endpoint.cost_per_1k_output_tokens;
    Some(input + output)
}

impl Default for ModelEndpoint {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            api_style: None,
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model_name: "gpt-4o-mini".into(),
            max_tokens: 8192,
            temperature: 0.7,
            timeout_secs: 7,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            seed: None,
            response_format: None,
            proxy_url: None,
            no_proxy: None,
            auth_header_name: default_auth_header_name(),
            auth_header_prefix: default_auth_header_prefix(),
            timeout_streaming_secs: None,
            reasoning_effort: None,
            web_search: None,
            cost_per_1k_input_tokens: 0.0,
            cost_per_1k_output_tokens: 0.0,
            context_window: None,
            reasoning_echo_max_chars: None,
        }
    }
}

/// A named entry in the model library. `endpoint` holds a reusable endpoint
/// definition; `name` is the unique id referenced by role selection in the
/// settings UI. Roles are materialized copies of an entry's endpoint, so the
/// agent/router continue to read role fields directly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelEntry {
    pub name: String,
    pub endpoint: ModelEndpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmConfig {
    pub small_model: ModelEndpoint,
    pub default_model: ModelEndpoint,
    pub balanced_model: ModelEndpoint,
    pub image_model: ModelEndpoint,
    pub audio_model: ModelEndpoint,
    pub embedding_model: ModelEndpoint,
    /// Model library: named, reusable endpoint definitions that roles can
    /// reference in the settings UI. Each entry's `name` is unique.
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    /// Maps a role key (e.g. `default_model`) to the name of the model-library
    /// entry the role is configured to use. Purely informational for the
    /// settings UI: the agent reads the materialized role fields directly.
    #[serde(default)]
    pub role_models: HashMap<String, String>,
    // §2.12: router-level total timeout
    pub max_total_duration_secs: u64,
    /// Streaming idle timeout: a stream that delivers no chunk for this long
    /// (headers received, body stalled) is aborted as a timeout instead of
    /// blocking until `max_total_duration_secs`. Providers occasionally hang
    /// with the connection half-open; without this the UI waits minutes for
    /// a reply that never comes. The router gives the FIRST chunk a longer
    /// grace (provider-side "thinking" delays it), so this value only bounds
    /// data gaps after the stream started flowing. The effective window is
    /// scaled UP with the request's prompt size (long contexts make
    /// provider-side gaps slower), capped at 90s — see the router's
    /// `scale_stream_idle`.
    pub stream_idle_timeout_secs: u64,
    // §2.3/5.1: retry backoff parameters
    pub retry_base_secs: u64,
    pub retry_factor: u32,
    pub retry_max_secs: u64,
    pub retry_jitter: f32,
    /// Route recording transcription through the dedicated `audio_model`
    /// endpoint. When false (or the endpoint is unconfigured), the default
    /// model handles transcription.
    pub stt_use_audio_model: bool,
    /// Route image understanding (chat attachments and file-tool vision)
    /// through the dedicated `image_model` endpoint. When false, the default
    /// model handles images.
    pub vision_use_image_model: bool,
    /// Per-endpoint (role) cap on concurrent LLM requests, applied by the
    /// router with a semaphore per role. Prevents N parallel sessions from
    /// hammering the same provider simultaneously (thundering-herd retries on
    /// 429). A session whose LLM call is queued behind this limit waits; its
    /// slot in `session.max_concurrent` is still held, so set it below the session
    /// concurrency when the provider is rate-limit sensitive.
    pub max_concurrent_requests: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            small_model: ModelEndpoint::default(),
            default_model: ModelEndpoint::default(),
            balanced_model: ModelEndpoint::default(),
            image_model: ModelEndpoint::default(),
            audio_model: ModelEndpoint::default(),
            embedding_model: ModelEndpoint::default(),
            models: Vec::new(),
            role_models: HashMap::new(),
            max_total_duration_secs: 180,
            stream_idle_timeout_secs: 20,
            retry_base_secs: 2,
            retry_factor: 2,
            retry_max_secs: 30,
            retry_jitter: 0.2,
            stt_use_audio_model: true,
            vision_use_image_model: true,
            max_concurrent_requests: 2,
        }
    }
}

impl LlmConfig {
    /// Apply the global per-response output-cap floor to every role endpoint:
    /// each endpoint's effective `max_tokens` becomes
    /// `max(endpoint.max_tokens, cap)`. Called when building the router so a
    /// small legacy per-endpoint cap (the old default was 8192) can no longer
    /// truncate long outputs mid-stream; per-endpoint values larger than the
    /// floor are preserved.
    pub fn with_response_cap(mut self, cap: u32) -> Self {
        for ep in [
            &mut self.small_model,
            &mut self.default_model,
            &mut self.balanced_model,
            &mut self.image_model,
            &mut self.audio_model,
            &mut self.embedding_model,
        ] {
            ep.max_tokens = ep.max_tokens.max(cap);
        }
        self
    }

    /// Apply the global reasoning-echo cap to every role endpoint that does
    /// not already set its own `reasoning_echo_max_chars` override. Called
    /// when building the router so the cap can be tuned from the "限制"
    /// settings tab without editing each endpoint.
    pub fn with_reasoning_echo_cap(mut self, cap: usize) -> Self {
        for ep in [
            &mut self.small_model,
            &mut self.default_model,
            &mut self.balanced_model,
            &mut self.image_model,
            &mut self.audio_model,
            &mut self.embedding_model,
        ] {
            if ep.reasoning_echo_max_chars.is_none() {
                ep.reasoning_echo_max_chars = Some(cap);
            }
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HotkeyConfig {
    pub mode: HotkeyMode,
    pub key_binding: String,
    pub mute_hotkey: Option<String>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mode: HotkeyMode::Toggle,
            key_binding: "Ctrl+Shift+Space".into(),
            mute_hotkey: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionConfig {
    pub max_concurrent: usize,
    pub max_steps: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            // Per-run ReAct step budget (raised 30 → 200 so long multi-tool
            // sessions don't hit the cap mid-run; see refactor-dedup.md A9
            // review note). Resumes grant a fresh budget, so a session can run
            // well past this total across pause/resume cycles.
            max_steps: 500,
        }
    }
}

/// Unified context-window and input/output budget limits for the agent loop.
///
/// Single source of truth for limits that used to be scattered across the
/// codebase (compactor ratio/reserve, observation cap, transcript cap) or
/// hard-coded per tool. Per-endpoint `ModelEndpoint.context_window` still
/// overrides `default_context_window` when configured.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextLimitsConfig {
    /// Fraction of the context window at which auto-compaction starts
    /// (clamped to 0.1–0.95 at use sites). Lower = more aggressive.
    pub compaction_ratio: f32,
    /// Tokens reserved for the model's response when computing the
    /// compaction threshold (headroom floor).
    pub compaction_reserve_tokens: u32,
    /// Context window in tokens used when an endpoint has no explicit
    /// `context_window` and the model id is not in the builtin catalog.
    pub default_context_window: u32,
    /// Per-response output cap floor (tokens) applied to every role endpoint
    /// when the router is built: the effective `max_tokens` sent to the
    /// provider is `max(endpoint.max_tokens, max_response_tokens)`. A small
    /// per-endpoint `max_tokens` (the legacy default was 8192) truncated long
    /// outputs mid-stream (finish_reason `length`), so the global default is
    /// deliberately very large — it only acts as a floor, letting users lower
    /// it from the "限制" settings tab without editing every endpoint.
    pub max_response_tokens: u32,
    /// Maximum characters of a tool observation fed back into the
    /// conversation. Also the default cap (in chars) builtin tools apply to
    /// their own output — the observation budget is the only limit, so tools
    /// never produce output the loop would immediately truncate again.
    /// Per-tool `tool_settings.*.max_output_chars` overrides it.
    pub max_observation_chars: usize,
    /// Cap (in chars) for transcripts built for memory fact inference.
    pub max_transcript_chars: usize,
    // —— user input attachments ——
    /// Max images attached to one user message.
    pub max_attachment_images: usize,
    /// Max non-image files attached to one user message.
    pub max_attachment_files: usize,
    /// Max decoded bytes per image attachment.
    pub max_attachment_image_bytes: usize,
    /// Max decoded bytes per file attachment.
    pub max_attachment_file_bytes: usize,
    /// Downscale images so the longest edge does not exceed this (px) before
    /// upload. Applied by the webview when compressing to JPEG.
    pub max_attachment_image_dim_px: u32,
    /// JPEG quality (0.0-1.0) used when the webview re-encodes an image.
    pub attachment_image_jpeg_quality: f32,
    // —— file tool ——
    /// Full-read cap (chars): larger files need `offset`/`limit` or
    /// `start_line`/`end_line`. Also the default byte-mode `limit`.
    pub file_read_max_chars: u64,
    /// Default lines to read in line mode when only `start_line` is given.
    pub file_line_span: u64,
    /// Single line too long to buffer safely (chars).
    pub file_max_line_chars: usize,
    /// Default input budget (chars) sent to the summarizer model.
    pub file_summary_input_chars: usize,
    /// Cap on directory entries returned by the file `list` operation.
    pub file_max_list_entries: usize,
    /// Absolute safety cap for byte-mode reads, regardless of caller `limit`.
    pub file_max_byte_read: u64,
    /// Cap on image bytes sent to the vision model.
    pub file_vision_max_bytes: u64,
    // —— files tool: search operation ——
    /// Snippet chars around each content-mode match.
    pub search_snippet_chars: usize,
    /// Upper clamp for `max_results` — untrusted input cannot disable the cap.
    pub search_max_results: usize,
    /// Content-mode skip cap: files larger than this are not searched.
    pub search_max_file_size_bytes: u64,
    /// Line-range search window cap in bytes.
    pub search_window_bytes: u64,
    // —— agent text limits ——
    /// Max chars in notification summary text.
    pub notification_summary_chars: usize,
    /// Max chars of a background-task result injected into the owning session's
    /// context when the task finishes (the full output stays in the log file,
    /// whose path is appended so the model can read more on demand).
    pub task_result_context_chars: usize,
    /// Min chars of partial output before an interim checkpoint is persisted.
    pub partial_checkpoint_min_chars: usize,
    /// Min wall-clock seconds between partial-stream checkpoints.
    pub partial_checkpoint_interval_secs: u64,
    /// Steps between mid-session fact re-inference runs.
    pub fact_infer_interval_steps: u32,
    /// Min wall-clock seconds between LLM fact-extraction calls for a given
    /// session. Complements `fact_infer_interval_steps` (a step-based gate):
    /// rapid message turns are throttled to at most one extraction per
    /// interval, while the maintenance pass keeps memory consistent
    /// regardless. The extraction cursor is untouched by a throttled skip,
    /// so the pending messages are processed on the next allowed run.
    pub fact_extraction_min_interval_secs: u64,
    /// Max known facts listed in the extraction prompt as context.
    pub max_known_facts: usize,
    /// Max chars of a fact subject/predicate/object field (prompt-injection
    /// sanitization truncation).
    pub sanitize_field_max_chars: usize,
    /// Max chars read per file tool `summary` LLM call before the outer
    /// timeout fires.
    pub file_summary_timeout_secs: u64,
    // —— agent loop behavior caps (were hardcoded constants in the ReAct loop) ——
    /// How many times a text-only response that looks cut off / mid-session is
    /// retried with a continuation nudge before it is accepted as a final
    /// answer. Bounded so a model that keeps refusing to call a tool cannot
    /// spin the loop forever. Was `MAX_CUT_OFF_RETRIES = 2`.
    pub cut_off_retries: u32,
    /// How many times a completely empty model response is retried before the
    /// turn errors out. Was `EMPTY_RESPONSE_MAX_RETRIES = 3`.
    pub empty_response_max_retries: u32,
    /// Settling delay between empty-response retries (ms), giving the
    /// upstream transient glitch time to clear. Was
    /// `EMPTY_RESPONSE_RETRY_DELAY = 1500`.
    pub empty_response_retry_delay_ms: u64,
    /// A provider stream that delivers no chunk for this long (ms) is
    /// announced to the UI as `StreamStalled`, long before the router's idle
    /// timeout aborts it. Was `STALL_WARN_DELAY_MS = 10_000`.
    pub stream_stall_warn_delay_ms: u64,
    /// Cap (chars) for the per-turn reasoning echo sent back to
    /// OpenAI-compatible providers; oversized reasoning keeps its tail. Was
    /// `MAX_REASONING_ECHO_CHARS = 3000` in the responses adapter.
    pub reasoning_echo_max_chars: usize,
    // —— background task caps ——
    /// Bounded live-output tail (chars) kept per running task for `task:output`
    /// preview events. Was `JOB_TAIL_MAX_CHARS = 2000`.
    pub background_job_tail_max_chars: usize,
    /// Cadence of `task:output` events while a task produces output (ms).
    /// Was `JOB_OUTPUT_EMIT_INTERVAL = 1500`.
    pub background_job_output_emit_interval_ms: u64,
    /// Terminal tasks stay on the board this long (secs), then are reaped by
    /// the next spawn. Was `TERMINAL_JOB_TTL = 600`.
    pub terminal_job_ttl_secs: u64,
    // —— safety boundaries (raising these widens the attack surface) ——
    /// MCP binary content (image/audio/resource blob) kept in observations
    /// before being replaced by an `oversized` marker (base64 chars).
    pub mcp_max_binary_payload_bytes: usize,
    /// Single buffered (incomplete) SSE line/event cap for MCP HTTP transport.
    pub mcp_max_sse_buffer_bytes: usize,
    /// Max size of a single SKILL.md file; larger files are skipped.
    pub skills_max_md_bytes: u64,
    /// Max lines the SKILL.md parser processes.
    pub skills_max_parse_lines: usize,
    /// Max length of a single SKILL.md parser line.
    pub skills_max_line_len: usize,
    /// Max bytes of skill `instructions` accepted by the `self` create-skill op.
    pub self_tool_max_instructions_bytes: usize,
    /// Max bytes of a skill script file accepted by the `self` create-skill op.
    pub self_tool_max_script_bytes: usize,
    /// Max retries for the network tool's HTTP requests.
    pub network_max_retries: u32,
    /// Exponential backoff base (secs) for the network tool.
    pub network_backoff_base_secs: u64,
    /// Max response body bytes buffered by the network tool.
    pub network_max_body_bytes: usize,
    // —— resource bounds ——
    /// Clipboard history entries kept in memory.
    pub clipboard_history_entries: usize,
    /// Upper clamp for the clipboard `history` operation's `limit` argument.
    pub clipboard_history_max_entries: usize,
    /// Per-entry content truncation for clipboard history dumps.
    pub clipboard_entry_max_chars: usize,
    /// Max concurrent scheduled_tasks.
    pub scheduled_tasks_max: usize,
    /// Max scheduled-task due horizon (secs ahead).
    pub scheduled_tasks_due_horizon_secs: i64,
    /// Max concurrent background shell tasks.
    pub background_max_tasks: usize,
    /// Max bytes batched into one `agent:chunk` event.
    pub event_chunk_batch_max_bytes: usize,
    /// Audio ring buffer size in seconds (capture latency).
    pub input_ring_buffer_secs: usize,
    /// Embedding requests chunk size (provider request limits).
    pub embedding_chunk_size: usize,
}

impl Default for ContextLimitsConfig {
    fn default() -> Self {
        Self {
            compaction_ratio: 0.75,
            compaction_reserve_tokens: 4_096,
            default_context_window: 128_000,
            // Per-response output cap floor: deliberately large so long outputs
            // are never truncated by a small per-endpoint `max_tokens` (the
            // legacy default was 8192 and cut off long replies mid-stream).
            // 128k covers the largest common provider output budgets; the
            // router additionally clamps to the endpoint's resolved context
            // window so the floor can never be rejected by the provider.
            max_response_tokens: 128_000,
            max_observation_chars: 8_000,
            max_transcript_chars: 4_000,
            max_attachment_images: 4,
            max_attachment_files: 5,
            max_attachment_image_bytes: 10 * 1024 * 1024,
            max_attachment_file_bytes: 20 * 1024 * 1024,
            max_attachment_image_dim_px: 1568,
            attachment_image_jpeg_quality: 0.85,
            file_read_max_chars: 128_000,
            file_line_span: 100,
            file_max_line_chars: 128_000,
            file_summary_input_chars: 60_000,
            file_max_list_entries: 1_000,
            file_max_byte_read: 16 * 1024 * 1024,
            file_vision_max_bytes: 8 * 1024 * 1024,
            search_snippet_chars: 200,
            search_max_results: 1_000,
            search_max_file_size_bytes: 100 * 1024 * 1024,
            search_window_bytes: 16 * 1024 * 1024,
            notification_summary_chars: 800,
            task_result_context_chars: 4_000,
            partial_checkpoint_min_chars: 1_000,
            partial_checkpoint_interval_secs: 2,
            fact_infer_interval_steps: 25,
            fact_extraction_min_interval_secs: 60,
            max_known_facts: 40,
            sanitize_field_max_chars: 256,
            file_summary_timeout_secs: 120,
            cut_off_retries: 2,
            empty_response_max_retries: 3,
            empty_response_retry_delay_ms: 1500,
            stream_stall_warn_delay_ms: 10_000,
            reasoning_echo_max_chars: 3000,
            background_job_tail_max_chars: 2000,
            background_job_output_emit_interval_ms: 1500,
            terminal_job_ttl_secs: 600,
            mcp_max_binary_payload_bytes: 2 * 1024 * 1024,
            mcp_max_sse_buffer_bytes: 2 * 1024 * 1024,
            skills_max_md_bytes: 256 * 1024,
            skills_max_parse_lines: 5_000,
            skills_max_line_len: 4_096,
            self_tool_max_instructions_bytes: 256 * 1024,
            self_tool_max_script_bytes: 512 * 1024,
            network_max_retries: 2,
            network_backoff_base_secs: 1,
            network_max_body_bytes: 1024 * 1024,
            clipboard_history_entries: 10,
            clipboard_history_max_entries: 100,
            clipboard_entry_max_chars: 2_000,
            scheduled_tasks_max: 32,
            scheduled_tasks_due_horizon_secs: 365 * 24 * 3600,
            background_max_tasks: 64,
            event_chunk_batch_max_bytes: 8 * 1024,
            input_ring_buffer_secs: 20,
            embedding_chunk_size: 64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryConfig {
    pub session_window_size: usize,
    pub history_retention_days: u32,
    pub fact_inference_enabled: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            session_window_size: 50,
            history_retention_days: 90,
            fact_inference_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SecurityConfig {
    pub confirmation_mode: ConfirmationMode,
    pub min_risk_level: RiskLevel,
    pub encrypt_sensitive: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            confirmation_mode: ConfirmationMode::Always,
            // Medium: Safe and Low operations (file reads, window listing,
            // clipboard reads, ...) auto-approve in the agent loop, while
            // anything that mutates state (file edits, network, env vars,
            // MCP/skill tools, shell) still requires confirmation. A Low
            // default would gate virtually every non-Safe step and flip
            // existing autonomous sessions into per-step confirmation dialogs.
            min_risk_level: RiskLevel::Medium,
            encrypt_sensitive: true,
        }
    }
}

/// Speech-to-text configuration. Lives under `[media.stt]` (previously the
/// top-level `[stt]` section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SttConfig {
    /// Speech-to-text provider. One of:
    /// - `mcp`: route through an MCP server exposing `stt.transcribe`
    /// - `llm`: transcribe via the configured `audio_model` LLM endpoint
    /// - `openai`: OpenAI Whisper-compatible `/audio/transcriptions`
    ///   (also Groq, Deepgram's OpenAI-compatible endpoint, Together,
    ///   local whisper.cpp/LM Studio, and most gateways)
    /// - `groq`: Groq host with OpenAI-Whisper-compatible wire format
    /// - `gemini`: Google Gemini `generateContent` audio transcription
    /// - `deepgram`: Deepgram REST `/v1/listen`
    /// - `assemblyai`: AssemblyAI `/v2/transcript`
    /// - `none`: no transcription
    pub provider: String,
    /// MCP server name when `provider == "mcp"`.
    pub mcp_server: Option<String>,
    /// API key for cloud STT providers.
    pub api_key: String,
    /// Model id for providers that require one (e.g. `whisper-1`,
    /// `nova-2`, `whisper-large-v3-turbo`).
    pub model: String,
    /// Base URL override for OpenAI-compatible providers. Overrides the
    /// provider's default host when non-empty.
    pub base_url: String,
    /// Transcription timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum transcription confidence (0.0-1.0) for the gateway's
    /// confidence gate: when the provider reports a lower confidence the
    /// gateway falls back to the main model. Providers without confidence
    /// reporting (e.g. OpenAI Whisper) ignore this and fall back on error /
    /// empty result instead.
    pub min_confidence: f32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: "mcp".into(),
            mcp_server: None,
            api_key: String::new(),
            model: String::new(),
            base_url: String::new(),
            timeout_secs: 30,
            min_confidence: 0.7,
        }
    }
}

/// OCR (image text extraction) configuration. Lives under `[media.ocr]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OcrConfig {
    /// OCR provider. One of:
    /// - `baidu`: Baidu 通用文字识别（标准版）
    /// - `azure`: Azure AI Vision (Computer Vision 3.2 OCR)
    /// - `tencent`: Tencent Cloud 通用印刷体识别
    /// - `none`: no OCR client
    pub provider: String,
    /// API key / access token for cloud OCR providers.
    pub api_key: String,
    /// Secondary secret where a provider requires one (Baidu secret key).
    pub api_secret: String,
    /// Base URL override. Overrides the provider's default host when
    /// non-empty.
    pub base_url: String,
    /// OCR timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum recognition confidence (0.0-1.0) for the gateway's
    /// confidence gate: when the provider reports a lower average
    /// confidence the gateway falls back to the main model. Providers
    /// without confidence reporting ignore this and fall back on error /
    /// empty result instead.
    pub min_confidence: f32,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            api_key: String::new(),
            api_secret: String::new(),
            base_url: String::new(),
            timeout_secs: 20,
            min_confidence: 0.7,
        }
    }
}

/// Text-to-speech configuration. Lives under `[media.tts]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TtsConfig {
    /// TTS provider. One of:
    /// - `openai`: OpenAI `/v1/audio/speech` (tts-1 / tts-1-hd / gpt-4o-mini-tts)
    /// - `elevenlabs`: ElevenLabs `/v1/text-to-speech/{voice_id}`
    /// - `none`: no TTS client
    pub provider: String,
    pub api_key: String,
    /// Model id for providers that require one (e.g. `tts-1`,
    /// `gpt-4o-mini-tts`).
    pub model: String,
    /// Voice id / name for providers that expose voices
    /// (e.g. `alloy`, `11labs_voice_id`).
    pub voice: String,
    /// Base URL override. Overrides the provider's default host when
    /// non-empty.
    pub base_url: String,
    /// TTS timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            api_key: String::new(),
            model: String::new(),
            voice: String::new(),
            base_url: String::new(),
            timeout_secs: 60,
        }
    }
}

/// Text-to-image generation configuration. Lives under `[media.image_gen]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImageGenConfig {
    /// Image generation provider. One of:
    /// - `openai`: OpenAI `/v1/images/generations` (gpt-image-1 / dall-e-3)
    /// - `gemini`: Google Gemini `generateContent` (image modality)
    /// - `none`: no image generation client
    pub provider: String,
    pub api_key: String,
    /// Model id for providers that require one (e.g. `gpt-image-1`,
    /// `gemini-2.5-flash-image`).
    pub model: String,
    /// Base URL override. Overrides the provider's default host when
    /// non-empty.
    pub base_url: String,
    /// Image generation timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            api_key: String::new(),
            model: String::new(),
            base_url: String::new(),
            timeout_secs: 120,
        }
    }
}

/// Unified media capability configuration (STT / OCR / TTS / image
/// generation). Replaced the legacy top-level `[stt]` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MediaConfig {
    /// Speech-to-text (voice input transcription).
    pub stt: SttConfig,
    /// OCR (image text extraction).
    pub ocr: OcrConfig,
    /// Text-to-speech (voice output).
    pub tts: TtsConfig,
    /// Text-to-image generation.
    pub image_gen: ImageGenConfig,
}

/// Skills ecosystem configuration (M4-01 / §4.6.3).
///
/// `root` defaults to `None`, which means the engine resolves the
/// default skills directory (`<app_data_dir>/skills`) at scan time.
///
/// `enabled` semantics:
/// - `None` (default in a fresh TOML file): all discovered skills are enabled.
/// - `Some(list)`: an exhaustive allowlist; only the listed skill names are
///   enabled (an empty `Some([])` disables everything). Switching from
///   `None` to `Some` happens automatically the first time the user toggles
///   a skill, so the lone-disable edge case survives app restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct SkillsConfig {
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Vec<String>>,
}

/// Sandbox execution configuration for skill scripts (M4-02).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SkillsExecConfig {
    /// Root directory for per-skill virtual environments.
    pub venv_root: PathBuf,
    /// Working directory for script execution (isolated from skill source).
    /// Defaults to a subfolder of the system Temp directory (see
    /// [`default_work_dir`]).
    pub work_dir: PathBuf,
    /// Maximum wall-clock seconds before a script is killed.
    pub timeout_secs: u64,
    /// Maximum output lines captured from stdout/stderr.
    pub max_output_lines: usize,
    /// Optional CPU time limit in seconds (best-effort, platform-dependent).
    pub cpu_time_secs: Option<u64>,
    /// Optional memory limit in MB (best-effort, platform-dependent).
    pub max_memory_mb: Option<u64>,
}

impl Default for SkillsExecConfig {
    fn default() -> Self {
        let data_dir = ConfigLoader::data_dir();
        Self {
            venv_root: data_dir.join("venvs"),
            work_dir: default_work_dir().join("skills_work"),
            timeout_secs: 30,
            max_output_lines: 5000,
            cpu_time_secs: None,
            max_memory_mb: None,
        }
    }
}

/// Default working directory for agent-executed commands. A dedicated
/// subfolder of the system Temp directory so the agent never runs in the
/// app's own working directory. Single source of truth for shell, process,
/// venv and skill execution; tools that want a different directory can
/// override with `.current_dir(...)` before spawning.
///
/// The directory is created on first use so spawned commands never fail with
/// "current directory does not exist".
pub fn default_work_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("haven");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Per-tool settings (refine §4.8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ToolConfig {
    /// Whether the tool is available to the agent. Disabled tools are
    /// excluded from the tool catalog (and thus the model's schema list) and
    /// rejected at execution. Tools without an entry in `tool_settings` are
    /// enabled by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub timeout_secs: u64,
    /// Per-tool output cap override (chars). `None` inherits the global
    /// `context_limits.max_observation_chars` (the observation budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_chars: Option<usize>,
    pub max_retries: u32,
    pub retry_backoff_secs: u64,
    pub allowed_paths: Vec<String>,
    pub disabled_operations: Vec<String>,
    pub risk_override: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: 30,
            max_output_chars: None,
            max_retries: 0,
            retry_backoff_secs: 2,
            allowed_paths: Vec::new(),
            disabled_operations: Vec::new(),
            risk_override: None,
        }
    }
}

/// MCP discovery and health monitoring configuration (M4-03).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpDiscoveryConfig {
    pub health_interval_secs: u64,
    pub reconnect_initial_ms: u64,
    pub reconnect_max_ms: u64,
    pub reconnect_max_retries: u32,
}

impl Default for McpDiscoveryConfig {
    fn default() -> Self {
        Self {
            health_interval_secs: 15,
            reconnect_initial_ms: 2000,
            reconnect_max_ms: 60000,
            reconnect_max_retries: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// Log level enum (M6-05)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

// ---------------------------------------------------------------------------
// Logging configuration (M6-05)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LogConfig {
    pub level: LogLevel,
    pub file_enabled: bool,
    pub file_path: Option<PathBuf>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            file_enabled: true,
            file_path: None,
        }
    }
}

impl LogConfig {
    /// Resolve the default log directory: `<app_data_dir>/logs/haven.log`.
    pub fn default_log_path() -> PathBuf {
        ConfigLoader::data_dir().join("logs").join("haven.log")
    }
}

// ---------------------------------------------------------------------------
// Notification configuration (M5-03)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NotificationConfig {
    pub session_created: NotifyChannels,
    pub session_completed: NotifyChannels,
    pub session_paused: NotifyChannels,
    pub session_resumed: NotifyChannels,
    pub session_error: NotifyChannels,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NotifyChannels {
    pub in_app: bool,
    pub windows: bool,
}

impl Default for NotifyChannels {
    fn default() -> Self {
        Self {
            in_app: true,
            windows: false,
        }
    }
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            session_created: NotifyChannels {
                in_app: true,
                windows: false,
            },
            session_completed: NotifyChannels {
                in_app: true,
                windows: true,
            },
            session_paused: NotifyChannels {
                in_app: true,
                windows: false,
            },
            session_resumed: NotifyChannels {
                in_app: true,
                windows: false,
            },
            session_error: NotifyChannels {
                in_app: true,
                windows: true,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Aggregated application config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransportType,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<String>,
    /// Working directory to spawn the stdio server process from. When set,
    /// relative paths in `command`/`args` resolve against it; when absent the
    /// server spawns from the app's working directory.
    pub cwd: Option<String>,
    /// Endpoint URL for HTTP transports (e.g. `http://localhost:3001/mcp`).
    pub url: String,
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpTransportType::Stdio,
            command: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            url: String::new(),
            enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// AppConfig + frontend-friendly Settings (hides sensitive fields)
// ---------------------------------------------------------------------------
//
// `Settings` is the API-key-blanked view of `AppConfig`; the two structs must
// stay structurally identical. Both structs and the conversion are generated
// from one field list so they cannot drift. The `sanitize` block runs on the
// cloned Settings before it is returned.

macro_rules! settings_pair {
    ($settings:ident; $(
        $(#[$field_doc:meta])*
        $field:ident: $ty:ty
    ),* $(,)?; $($sanitize:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
        #[serde(default)]
        pub struct AppConfig {
            $( $(#[$field_doc])* pub $field: $ty, )*
        }

        #[derive(Debug, Clone, Serialize, Deserialize, Default)]
        #[serde(default)]
        pub struct Settings {
            $( $(#[$field_doc])* pub $field: $ty, )*
        }

        impl From<&AppConfig> for Settings {
            fn from(c: &AppConfig) -> Self {
                let mut $settings = Self {
                    $( $field: c.$field.clone(), )*
                };
                $($sanitize)*
                $settings
            }
        }
    };
}

settings_pair! {
    settings;
    /// Default shell for the agent's `shell` tool when the model omits the
    /// `shell` argument. One of `powershell` (built-in Windows PowerShell),
    /// `cmd`, or `pwsh` (PowerShell 7, requires a separate install).
    default_shell: ShellChoice,
    audio: AudioConfig,
    llm: LlmConfig,
    hotkey: HotkeyConfig,
    session: SessionConfig,
    context_limits: ContextLimitsConfig,
    memory: MemoryConfig,
    security: SecurityConfig,
    media: MediaConfig,
    skills: SkillsConfig,
    skills_exec: SkillsExecConfig,
    mcp_discovery: McpDiscoveryConfig,
    mcp_servers: Vec<McpServerConfig>,
    notification: NotificationConfig,
    log: LogConfig,
    tool_settings: HashMap<String, ToolConfig>,
    ;
    settings.llm.small_model.api_key = String::new();
    settings.llm.default_model.api_key = String::new();
    settings.llm.balanced_model.api_key = String::new();
    settings.llm.image_model.api_key = String::new();
    settings.llm.audio_model.api_key = String::new();
    settings.llm.embedding_model.api_key = String::new();
    for m in settings.llm.models.iter_mut() {
        m.endpoint.api_key = String::new();
    }
    settings.media.stt.api_key = String::new();
    settings.media.ocr.api_key = String::new();
    settings.media.ocr.api_secret = String::new();
    settings.media.tts.api_key = String::new();
    settings.media.image_gen.api_key = String::new();
}

// ---------------------------------------------------------------------------
// Config loader
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConfigLoader {
    path: PathBuf,
    config: AppConfig,
}

impl ConfigLoader {
    /// Returns the default config path: `%APPDATA%/haven/config.toml` on Windows.
    pub fn default_path() -> PathBuf {
        Self::data_dir().join("config.toml")
    }

    /// Returns the Haven data directory: `%APPDATA%/haven` on Windows,
    /// `~/.local/share/haven` elsewhere. Single source of truth for every
    /// persisted artifact (config, database, logs, skills); the app binary
    /// must not re-implement this.
    pub fn data_dir() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            let base = std::env::var("APPDATA").unwrap_or_else(|_| {
                let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:".into());
                format!("{}\\AppData\\Roaming", home)
            });
            PathBuf::from(base).join("haven")
        }
        #[cfg(not(target_os = "windows"))]
        {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".local/share/haven")
        }
    }

    /// Default skills directory: `<data_dir>/skills`.
    pub fn default_skills_dir() -> PathBuf {
        Self::data_dir().join("skills")
    }

    /// Load config from a specific path. Creates a default config file if missing.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            tracing::info!("config not found, creating default at {}", path.display());
            let default_cfg = AppConfig::default();
            let toml_str = toml::to_string_pretty(&default_cfg)?;
            std::fs::write(path, toml_str)?;
            return Ok(Self {
                path: path.to_path_buf(),
                config: default_cfg,
            });
        }
        tracing::info!("loading config from {}", path.display());
        let content = std::fs::read_to_string(path)?;
        let config: AppConfig = match toml::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                // Never silently start with defaults: the next save() would
                // overwrite the user's config (MCP servers, API keys, skills)
                // with defaults, destroying it. Back up the unparsable file
                // so it can be recovered, then continue with defaults.
                let backup = path.with_extension("toml.bak");
                match std::fs::copy(path, &backup) {
                    Ok(_) => tracing::error!(
                        "config parse error at {}; original backed up to {}: {e}",
                        path.display(),
                        backup.display()
                    ),
                    Err(be) => tracing::error!(
                        "config parse error at {} (backup to {} failed: {}): {e}",
                        path.display(),
                        backup.display(),
                        be
                    ),
                }
                AppConfig::default()
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            config,
        })
    }

    /// Load from the default path.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&Self::default_path())
    }

    /// Persist current config to disk atomically.
    /// Writes to a temporary file first, then renames to prevent partial writes
    /// from concurrent save() calls or process crashes from corrupting the file.
    pub fn save(&self) -> anyhow::Result<()> {
        let toml_str = toml::to_string_pretty(&self.config)?;
        let tmp_path = self.path.with_extension("tmp");
        std::fs::write(&tmp_path, &toml_str)?;
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut AppConfig {
        &mut self.config
    }

    pub fn settings(&self) -> Settings {
        Settings::from(&self.config)
    }

    /// Apply a frontend `Settings` update, preserving stored api keys when the
    /// incoming value is empty.
    pub fn apply_settings(&mut self, settings: &Settings) {
        // Preserve existing keys if the frontend sends empty strings (masked).
        // `Settings::from` blanks every api key, so the incoming `llm` carries
        // no secrets; the previous live config is the only key source.
        let prev_llm = self.config.llm.clone();

        // Replace the whole `llm` section wholesale instead of copying fields
        // by hand: a new field added to `LlmConfig` is then applied here
        // automatically and cannot be forgotten (the two structs stay in sync
        // via `From<&AppConfig> for Settings`, which masks keys only).
        let mut llm = settings.llm.clone();
        for (next, prev) in [
            (&mut llm.small_model, &prev_llm.small_model),
            (&mut llm.default_model, &prev_llm.default_model),
            (&mut llm.balanced_model, &prev_llm.balanced_model),
            (&mut llm.image_model, &prev_llm.image_model),
            (&mut llm.audio_model, &prev_llm.audio_model),
            (&mut llm.embedding_model, &prev_llm.embedding_model),
        ] {
            if next.api_key.is_empty() {
                next.api_key = prev.api_key.clone();
            }
        }
        // Preserve masked library-entry keys, matched by entry name.
        for entry in llm.models.iter_mut() {
            if entry.endpoint.api_key.is_empty()
                && let Some(prev) = prev_llm.models.iter().find(|m| m.name == entry.name)
            {
                entry.endpoint.api_key = prev.endpoint.api_key.clone();
            }
        }
        self.config.llm = llm;

        self.config.audio = settings.audio.clone();
        self.config.default_shell = settings.default_shell;
        self.config.hotkey = settings.hotkey.clone();
        self.config.session = settings.session.clone();
        // The settings form sends the full `context_limits` object (the
        // frontend keeps the loaded copy intact and only edits exposed
        // fields), so applying it here cannot wipe fields the UI does not
        // render. `#[serde(default)]` fills any genuinely missing field with
        // its default, which is the expected upgrade behavior for new keys.
        self.config.context_limits = settings.context_limits.clone();
        self.config.memory = settings.memory.clone();
        self.config.security = settings.security.clone();
        self.config.media = {
            let incoming = settings.media.clone();
            let prev = &self.config.media;
            let mut s = incoming;
            // Preserve masked keys for every media capability.
            if s.stt.api_key.is_empty() {
                s.stt.api_key = prev.stt.api_key.clone();
            }
            if s.ocr.api_key.is_empty() {
                s.ocr.api_key = prev.ocr.api_key.clone();
            }
            if s.ocr.api_secret.is_empty() {
                s.ocr.api_secret = prev.ocr.api_secret.clone();
            }
            if s.tts.api_key.is_empty() {
                s.tts.api_key = prev.tts.api_key.clone();
            }
            if s.image_gen.api_key.is_empty() {
                s.image_gen.api_key = prev.image_gen.api_key.clone();
            }
            s
        };
        // The settings form does not manage these sections — MCP servers are
        // mutated by their own bridge commands and the `self` tool, skills and
        // tool settings likewise. The frontend payload omits them, so with
        // `#[serde(default)]` they deserialize to empty/default values here;
        // overwriting the live config with them would wipe every configured
        // MCP server / skill / tool setting on each settings save. Keep the
        // current values; `update_settings` restores the authoritative on-disk
        // copies (written directly by those commands) before saving.
        self.config.log = settings.log.clone();
        self.config.notification = settings.notification.clone();
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.audio.sample_rate, 16000);
        assert_eq!(cfg.hotkey.key_binding, "Ctrl+Shift+Space");
        assert_eq!(cfg.session.max_concurrent, 3);
        assert_eq!(cfg.session.max_steps, 500);
        assert_eq!(cfg.context_limits.compaction_ratio, 0.75);
        assert_eq!(cfg.context_limits.compaction_reserve_tokens, 4096);
        assert_eq!(cfg.context_limits.default_context_window, 128_000);
        assert_eq!(cfg.context_limits.max_response_tokens, 128_000);
        assert_eq!(cfg.context_limits.cut_off_retries, 2);
        assert_eq!(cfg.context_limits.empty_response_max_retries, 3);
        assert_eq!(cfg.context_limits.stream_stall_warn_delay_ms, 10_000);
        assert_eq!(cfg.context_limits.reasoning_echo_max_chars, 3000);
        assert_eq!(cfg.context_limits.max_observation_chars, 8_000);
        assert_eq!(cfg.context_limits.max_transcript_chars, 4_000);
        assert_eq!(cfg.context_limits.max_attachment_images, 4);
        assert_eq!(cfg.context_limits.max_attachment_files, 5);
        assert_eq!(
            cfg.context_limits.max_attachment_image_bytes,
            10 * 1024 * 1024
        );
        assert_eq!(
            cfg.context_limits.max_attachment_file_bytes,
            20 * 1024 * 1024
        );
        assert_eq!(cfg.context_limits.max_attachment_image_dim_px, 1568);
        assert_eq!(cfg.context_limits.attachment_image_jpeg_quality, 0.85);
        assert_eq!(
            cfg.context_limits.mcp_max_binary_payload_bytes,
            2 * 1024 * 1024
        );
        assert_eq!(cfg.context_limits.mcp_max_sse_buffer_bytes, 2 * 1024 * 1024);
        assert_eq!(cfg.context_limits.skills_max_md_bytes, 256 * 1024);
        assert_eq!(cfg.context_limits.skills_max_parse_lines, 5_000);
        assert_eq!(cfg.context_limits.skills_max_line_len, 4_096);
        assert_eq!(cfg.context_limits.self_tool_max_script_bytes, 512 * 1024);
        assert_eq!(cfg.context_limits.network_max_retries, 2);
        assert_eq!(cfg.context_limits.network_max_body_bytes, 1024 * 1024);
        assert_eq!(cfg.context_limits.clipboard_history_max_entries, 100);
        assert_eq!(cfg.context_limits.scheduled_tasks_max, 32);
        assert_eq!(cfg.context_limits.background_max_tasks, 64);
        assert_eq!(cfg.context_limits.event_chunk_batch_max_bytes, 8 * 1024);
        assert_eq!(cfg.context_limits.input_ring_buffer_secs, 20);
        assert_eq!(cfg.context_limits.embedding_chunk_size, 64);
        assert_eq!(cfg.context_limits.partial_checkpoint_interval_secs, 2);
        assert_eq!(cfg.context_limits.fact_infer_interval_steps, 25);
        assert_eq!(cfg.memory.history_retention_days, 90);
        assert!(cfg.security.encrypt_sensitive);
        assert!(cfg.mcp_servers.is_empty());
        assert_eq!(cfg.media.stt.provider, "mcp");
        assert_eq!(cfg.media.stt.timeout_secs, 30);
        assert!(cfg.media.stt.mcp_server.is_none());
        assert!(cfg.media.stt.api_key.is_empty());
        assert!(cfg.media.stt.model.is_empty());
        assert!(cfg.media.stt.base_url.is_empty());
        assert_eq!(cfg.media.stt.min_confidence, 0.7);
        assert_eq!(cfg.media.ocr.provider, "none");
        assert!(cfg.media.ocr.api_key.is_empty());
        assert!(cfg.media.ocr.api_secret.is_empty());
        assert_eq!(cfg.media.ocr.timeout_secs, 20);
        assert_eq!(cfg.media.ocr.min_confidence, 0.7);
        assert_eq!(cfg.media.tts.provider, "none");
        assert!(cfg.media.tts.voice.is_empty());
        assert_eq!(cfg.media.tts.timeout_secs, 60);
        assert_eq!(cfg.media.image_gen.provider, "none");
        assert_eq!(cfg.media.image_gen.timeout_secs, 120);
        assert!(cfg.llm.stt_use_audio_model);
        assert!(cfg.llm.vision_use_image_model);
        assert_eq!(cfg.llm.max_concurrent_requests, 2);
    }

    #[test]
    fn config_roundtrip_through_toml() {
        let cfg = AppConfig::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AppConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn security_missing_min_risk_level_uses_medium_default() {
        // A `[security]` table that omits the field falls back to the
        // struct default (Medium) — there is no legacy-Low behavior.
        let parsed: SecurityConfig = toml::from_str(
            r#"
                confirmation_mode = "always"
                encrypt_sensitive = true
            "#,
        )
        .unwrap();
        assert_eq!(parsed.min_risk_level, RiskLevel::Medium);
    }

    #[test]
    fn security_explicit_min_risk_level_wins() {
        let parsed: SecurityConfig = toml::from_str(r#"min_risk_level = "medium""#).unwrap();
        assert_eq!(parsed.min_risk_level, RiskLevel::Medium);
    }

    #[test]
    fn security_missing_table_uses_medium_default() {
        assert_eq!(SecurityConfig::default().min_risk_level, RiskLevel::Medium);
    }

    #[test]
    fn tool_config_missing_enabled_defaults_to_true() {
        // Existing config.toml files written before the `enabled` field
        // existed have `[tool_settings.*]` sections without `enabled`. It
        // must deserialize to `true` (tool stays enabled), never to the
        // `bool::default()` of false.
        let toml_str = r#"
            [tool_settings.file]
            timeout_secs = 60
        "#;
        let cfg: AppConfig = toml::from_str(toml_str).unwrap();
        let file = cfg.tool_settings.get("file").unwrap();
        assert!(file.enabled);
        assert_eq!(file.timeout_secs, 60);
        // Per-tool output cap defaults to None → inherits the global
        // `context_limits.max_observation_chars`.
        assert_eq!(file.max_output_chars, None);
    }

    #[test]
    fn settings_hide_api_keys() {
        let mut cfg = AppConfig::default();
        cfg.llm.default_model.api_key = "super-secret".to_string();
        let settings = Settings::from(&cfg);
        assert!(settings.llm.default_model.api_key.is_empty());
    }

    #[test]
    fn with_response_cap_raises_small_endpoint_caps_and_preserves_large_ones() {
        let mut cfg = AppConfig::default();
        cfg.llm.default_model.max_tokens = 4096;
        cfg.llm.small_model.max_tokens = 20_000;
        let lifted = cfg.llm.clone().with_response_cap(10_000);
        // Small legacy cap is raised to the floor.
        assert_eq!(lifted.default_model.max_tokens, 10_000);
        // An endpoint already above the floor keeps its own value.
        assert_eq!(lifted.small_model.max_tokens, 20_000);
    }

    #[test]
    fn with_reasoning_echo_cap_fills_unset_endpoints_and_preserves_overrides() {
        let mut cfg = AppConfig::default();
        cfg.llm.default_model.reasoning_echo_max_chars = Some(1234);
        let filled = cfg.llm.clone().with_reasoning_echo_cap(5000);
        // An endpoint without an override inherits the global cap.
        assert_eq!(filled.small_model.reasoning_echo_max_chars, Some(5000));
        // A per-endpoint override is preserved.
        assert_eq!(filled.default_model.reasoning_echo_max_chars, Some(1234));
    }

    #[test]
    fn apply_settings_preserves_keys_when_empty() {
        let mut cfg = AppConfig::default();
        cfg.llm.default_model.api_key = "keep-me".to_string();
        cfg.llm.image_model.api_key = "keep-multi".to_string();
        let mut settings = Settings::from(&cfg);
        // frontend sends empty api key (masked)
        settings.llm.default_model.model_name = "new-model".to_string();
        settings.llm.image_model.model_name = "gpt-4o".to_string();
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
        };
        loader.apply_settings(&settings);
        assert_eq!(loader.config().llm.default_model.api_key, "keep-me");
        assert_eq!(loader.config().llm.default_model.model_name, "new-model");
        assert_eq!(loader.config().llm.image_model.api_key, "keep-multi");
        assert_eq!(loader.config().llm.image_model.model_name, "gpt-4o");
    }

    #[test]
    fn apply_settings_preserves_media_api_keys_when_empty() {
        let mut cfg = AppConfig::default();
        cfg.media.stt.provider = "openai".into();
        cfg.media.stt.api_key = "keep-stt-key".to_string();
        cfg.media.ocr.api_key = "keep-ocr-key".to_string();
        cfg.media.ocr.api_secret = "keep-ocr-secret".to_string();
        cfg.media.tts.api_key = "keep-tts-key".to_string();
        cfg.media.image_gen.api_key = "keep-ig-key".to_string();
        let mut settings = Settings::from(&cfg);
        // Frontend sends masked (empty) api keys but new models/voices.
        settings.media.stt.model = "whisper-1".to_string();
        settings.media.ocr.provider = "baidu".to_string();
        settings.media.tts.voice = "alloy".to_string();
        settings.media.image_gen.model = "gpt-image-1".to_string();
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
        };
        loader.apply_settings(&settings);
        let media = &loader.config().media;
        assert_eq!(media.stt.api_key, "keep-stt-key");
        assert_eq!(media.stt.model, "whisper-1");
        assert_eq!(media.stt.provider, "openai");
        assert_eq!(media.ocr.api_key, "keep-ocr-key");
        assert_eq!(media.ocr.api_secret, "keep-ocr-secret");
        assert_eq!(media.ocr.provider, "baidu");
        assert_eq!(media.tts.api_key, "keep-tts-key");
        assert_eq!(media.tts.voice, "alloy");
        assert_eq!(media.image_gen.api_key, "keep-ig-key");
        assert_eq!(media.image_gen.model, "gpt-image-1");
    }

    #[test]
    fn settings_hide_media_api_keys() {
        let mut cfg = AppConfig::default();
        cfg.media.stt.api_key = "stt-secret".to_string();
        cfg.media.ocr.api_key = "ocr-secret".to_string();
        cfg.media.ocr.api_secret = "ocr-secret-2".to_string();
        cfg.media.tts.api_key = "tts-secret".to_string();
        cfg.media.image_gen.api_key = "ig-secret".to_string();
        let settings = Settings::from(&cfg);
        assert!(settings.media.stt.api_key.is_empty());
        assert!(settings.media.ocr.api_key.is_empty());
        assert!(settings.media.ocr.api_secret.is_empty());
        assert!(settings.media.tts.api_key.is_empty());
        assert!(settings.media.image_gen.api_key.is_empty());
    }

    #[test]
    fn apply_settings_preserves_mcp_skills_and_tool_settings() {
        let mut cfg = AppConfig::default();
        cfg.mcp_servers.push(McpServerConfig {
            name: "tavily".into(),
            enabled: true,
            ..Default::default()
        });
        cfg.skills.enabled = Some(vec!["echo".into()]);
        cfg.skills_exec.work_dir = "C:\\skills_work".into();
        cfg.tool_settings.insert(
            "file".into(),
            ToolConfig {
                timeout_secs: 60,
                ..Default::default()
            },
        );
        cfg.mcp_discovery.health_interval_secs = 99;
        cfg.context_limits.max_transcript_chars = 9999;

        let mut settings = Settings::from(&cfg);
        // The settings form payload omits these sections → they deserialize
        // to empty/defaults (Settings derives #[serde(default)]).
        settings.mcp_servers = Vec::new();
        settings.skills = SkillsConfig::default();
        settings.skills_exec = SkillsExecConfig::default();
        settings.tool_settings = HashMap::new();
        settings.mcp_discovery = McpDiscoveryConfig::default();
        // A real edit still applies.
        settings.session.max_concurrent = 5;
        // Context limits are managed by the settings form: the form sends the
        // full loaded object (only editing exposed fields), so config.toml
        // tuning carried in that object survives a save.
        assert_eq!(settings.context_limits.max_transcript_chars, 9999);

        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
        };
        loader.apply_settings(&settings);

        assert_eq!(loader.config().mcp_servers.len(), 1);
        assert_eq!(loader.config().mcp_servers[0].name, "tavily");
        assert_eq!(
            loader.config().skills.enabled,
            Some(vec!["echo".to_string()])
        );
        assert_eq!(
            loader.config().skills_exec.work_dir,
            PathBuf::from("C:\\skills_work")
        );
        assert_eq!(loader.config().tool_settings.len(), 1);
        assert_eq!(loader.config().mcp_discovery.health_interval_secs, 99);
        assert_eq!(loader.config().session.max_concurrent, 5);
        // Context limits are managed by the settings form: the form sends the
        // full loaded object, so config.toml tuning survives a save.
        assert_eq!(loader.config().context_limits.max_transcript_chars, 9999);
        // An explicit edit in the payload still applies.
        settings.context_limits.max_transcript_chars = 8888;
        loader.apply_settings(&settings);
        assert_eq!(loader.config().context_limits.max_transcript_chars, 8888);
    }

    #[test]
    fn load_creates_default_file() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let loader = ConfigLoader::load_from(&path).unwrap();
        assert!(path.exists());
        assert_eq!(loader.config().audio.sample_rate, 16000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compute_cost_usd_zero_rates_return_none() {
        let ep = ModelEndpoint::default();
        assert_eq!(compute_cost_usd(&ep, 1000, 500), None);
    }

    #[test]
    fn compute_cost_usd_calculates_input_and_output() {
        let ep = ModelEndpoint {
            cost_per_1k_input_tokens: 3.0,
            cost_per_1k_output_tokens: 15.0,
            ..Default::default()
        };
        // 2k in + 1k out = 6.0 + 15.0
        let cost = compute_cost_usd(&ep, 2000, 1000).unwrap();
        assert!((cost - 21.0).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_usd_output_only_config() {
        let ep = ModelEndpoint {
            cost_per_1k_output_tokens: 10.0,
            ..Default::default()
        };
        // input rate zero -> only output counted
        let cost = compute_cost_usd(&ep, 5000, 200).unwrap();
        assert!((cost - 2.0).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_usd_handles_fractional_tokens() {
        let ep = ModelEndpoint {
            cost_per_1k_input_tokens: 1.0,
            ..Default::default()
        };
        // 500 tokens -> 0.5
        let cost = compute_cost_usd(&ep, 500, 0).unwrap();
        assert!((cost - 0.5).abs() < 1e-9);
    }

    #[test]
    fn load_reads_existing_file() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut cfg = AppConfig::default();
        cfg.audio.sample_rate = 44100;
        std::fs::write(&path, toml::to_string_pretty(&cfg).unwrap()).unwrap();
        let loader = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(loader.config().audio.sample_rate, 44100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_backs_up_unparsable_config_instead_of_destroying_it() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // Genuinely unparsable TOML (unterminated array).
        let original = "audio = { sample_rate = [1, 2\n";
        std::fs::write(&path, original).unwrap();

        // The corrupt file must NOT be silently discarded: it is backed up
        // (same name + ".bak") and the loader starts with defaults so a later
        // save() can never overwrite the user's config unnoticed.
        let _loader = ConfigLoader::load_from(&path).unwrap();
        let backup = path.with_extension("toml.bak");
        assert!(backup.exists());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        // Sanity: the file itself is untouched until a save happens.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_persists_changes() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        loader.config_mut().session.max_concurrent = 7;
        loader.save().unwrap();
        let reloaded = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(reloaded.config().session.max_concurrent, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
