//! Miscellaneous configuration slices: hotkey, session, context limits,
//! memory, security, skills, tool settings, MCP discovery / servers,
//! logging, and notifications.

use super::*;

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
    /// Max chars of a background-action result injected into the owning session's
    /// context when the action finishes (the full output stays in the log file,
    /// whose path is appended so the model can read more on demand).
    pub action_result_context_chars: usize,
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
    // —— background action caps ——
    /// Bounded live-output tail (chars) kept per running action for `action:output`
    /// preview events. Was `JOB_TAIL_MAX_CHARS = 2000`.
    pub background_job_tail_max_chars: usize,
    /// Cadence of `action:output` events while a action produces output (ms).
    /// Was `JOB_OUTPUT_EMIT_INTERVAL = 1500`.
    pub background_job_output_emit_interval_ms: u64,
    /// Terminal actions stay on the board this long (secs), then are reaped by
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
    /// Max concurrent scheduled_actions.
    pub scheduled_actions_max: usize,
    /// Max scheduled-action due horizon (secs ahead).
    pub scheduled_actions_due_horizon_secs: i64,
    /// Max concurrent background shell actions.
    pub background_max_actions: usize,
    /// Max bytes batched into one `agent:chunk` event.
    pub event_chunk_batch_max_bytes: usize,
    /// Audio ring buffer size in seconds (capture latency).
    pub input_ring_buffer_secs: usize,
    /// Embedding requests chunk size (provider request limits).
    pub embedding_chunk_size: usize,
    /// Max tool definitions sent on one LLM request. Provider APIs (e.g.
    /// OpenAI-compatible gateways) reject requests above a hard ceiling
    /// (commonly 350). Progressive `load_mcp` / `load_skill` can accumulate
    /// past that; Haven refuses oversized loads and truncates the session
    /// overlay so builtins stay available.
    pub max_tools_per_request: usize,
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
            action_result_context_chars: 4_000,
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
            scheduled_actions_max: 32,
            scheduled_actions_due_horizon_secs: 365 * 24 * 3600,
            background_max_actions: 64,
            event_chunk_batch_max_bytes: 8 * 1024,
            input_ring_buffer_secs: 20,
            embedding_chunk_size: 64,
            max_tools_per_request: 350,
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
