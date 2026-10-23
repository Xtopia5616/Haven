//! Miscellaneous configuration slices: hotkey, session, context limits,
//! memory, security, skills, tool settings, MCP discovery / servers,
//! logging, and notifications.

use super::*;

/// Hard ceiling for the number of stored facts inserted into a fact-extraction
/// prompt. The default remains lower so users can reduce prompt cost.
pub const MAX_FACTS_IN_EXTRACTION_PROMPT: usize = 64;
pub const MIN_SESSION_STEPS_PER_RUN: u32 = 1;
pub const MAX_SESSION_STEPS_PER_RUN: u32 = 1_000;
pub const MIN_SESSION_STEPS_PER_SESSION: u32 = 1;
pub const MAX_SESSION_STEPS_PER_SESSION: u32 = 100_000;
pub const MIN_SESSION_MAX_CONCURRENT: usize = 1;
pub const MAX_SESSION_MAX_CONCURRENT: usize = 10;
pub const MAX_SESSION_PROMPT_HISTORY_LIMIT: usize = 500;
/// User-facing maximum automatic session-retention period in days; zero disables cleanup.
pub const MAX_HISTORY_RETENTION_DAYS: u32 = 365;
/// A configured model context must be large enough to represent a useful prompt budget.
pub const MIN_CONTEXT_WINDOW_TOKENS: u32 = 1_000;
/// Conservative ceiling for endpoint and default context-window settings.
pub const MAX_CONTEXT_WINDOW_TOKENS: u32 = 2_000_000;
pub const MAX_CONTEXT_TEXT_CHARS: usize = 100_000;
pub const MAX_NOTIFICATION_SUMMARY_CHARS: usize = 4_096;
pub const MAX_SANITIZE_FIELD_CHARS: usize = 256;
pub const MAX_CLIPBOARD_HISTORY_ENTRIES: usize = 100;
pub const MAX_CLIPBOARD_ENTRY_OUTPUT_CHARS: usize = 100_000;
pub const MAX_TOOL_TIMEOUT_SECS: u64 = 3_600;
pub const MAX_TOOL_RETRIES: u32 = 10;
pub const MAX_TOOL_RETRY_BACKOFF_SECS: u64 = 60;
pub const MAX_MCP_TOOL_EXPOSURE_OVERRIDES: usize = 256;
pub const MAX_MCP_TOOL_EXPOSURE_PATTERN_CHARS: usize = 256;
pub const MAX_LSP_SERVERS: usize = 32;
pub const MAX_LSP_SERVER_ARGS: usize = 64;
pub const MAX_EVERYTHING_COMMAND_PATH_CHARS: usize = 32_767;

/// Executable used by the optional Everything filename search backend.
/// This executable configuration is local-only and never enters Settings IPC.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct EverythingConfig {
    pub enabled: bool,
    pub command: Option<String>,
}

impl std::fmt::Debug for EverythingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EverythingConfig")
            .field("enabled", &self.enabled)
            .field("command", &self.command.as_ref().map(|_| "<configured>"))
            .finish()
    }
}

impl EverythingConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let Some(command) = self.command.as_deref() else {
            anyhow::bail!("everything.command is required when Everything search is enabled");
        };
        if command.trim().is_empty()
            || command.encode_utf16().count() > MAX_EVERYTHING_COMMAND_PATH_CHARS
            || command.chars().any(char::is_control)
            || !crate::path::is_absolute_local_path(std::path::Path::new(command))
        {
            anyhow::bail!("everything.command must be a valid local executable path");
        }
        Ok(())
    }
}

#[cfg(test)]
mod everything_config_tests {
    use super::EverythingConfig;

    #[test]
    fn disabled_configuration_needs_no_command() {
        assert!(EverythingConfig::default().validate().is_ok());
    }

    #[test]
    fn enabled_configuration_requires_a_clean_command_path() {
        assert!(
            EverythingConfig {
                enabled: true,
                command: None,
            }
            .validate()
            .is_err()
        );
        assert!(
            EverythingConfig {
                enabled: true,
                command: Some("es.exe\n-inject".into()),
            }
            .validate()
            .is_err()
        );
        assert!(
            EverythingConfig {
                enabled: true,
                command: Some("C:\\Tools\\es.exe".into()),
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn debug_redacts_executable_path() {
        let config = EverythingConfig {
            enabled: true,
            command: Some("C:\\private\\es.exe".into()),
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("private"));
        assert!(debug.contains("<configured>"));
    }
}
/// A zero background ToolRun limit intentionally disables background launches.
pub const MAX_RUNNING_BACKGROUND_TOOL_RUNS: usize = 64;
/// Maximum number of entries returned by one files.list operation.
pub const MAX_FILE_MAX_LIST_ENTRIES: usize = 10_000;
/// Line-oriented reads require at least one byte of retention capacity.
pub const MIN_FILE_LINE_MAX_BYTES: usize = 1;
/// Maximum bytes buffered for one line in file read, outline, and summary paths.
pub const MAX_FILE_LINE_MAX_BYTES: usize = 128_000;
/// Maximum bytes for any configured whole-file read, write, or vision input.
pub const MAX_FILE_OPERATION_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum bytes read by the configured files byte-read path.
pub const MIN_FILE_BYTE_READ_MAX_BYTES: u64 = 1;
pub const MAX_FILE_BYTE_READ_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_FILE_SUMMARY_INPUT_CHARS: usize = 256_000;
pub const MAX_FILE_SUMMARY_TIMEOUT_SECS: u64 = 600;
pub const MAX_MCP_MAX_SSE_BUFFER_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SKILLS_MAX_MD_BYTES: u64 = 1024 * 1024;
pub const MAX_NETWORK_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NETWORK_RETRIES: u32 = 10;
pub const MAX_NETWORK_BACKOFF_BASE_SECS: u64 = 60;
/// Search must return at least one result when it is enabled.
pub const MIN_SEARCH_MAX_RESULTS: usize = 1;
/// Maximum number of results one files search may materialize.
pub const MAX_SEARCH_MAX_RESULTS: usize = 1_000;
/// Maximum number of Unicode scalar values accepted for a file-search regex/glob.
pub const MAX_SEARCH_PATTERN_CHARS: usize = 4_096;
/// Maximum Windows-compatible root path length, measured as UTF-16 code units.
pub const MAX_SEARCH_ROOT_UTF16_UNITS: usize = 32_767;
/// Maximum serialized byte size of materialized results from one file search.
pub const MAX_SEARCH_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SEARCH_SNIPPET_CHARS: usize = 4_096;
/// Content searches skip files larger than this unless the configured limit is zero (unlimited).
pub const MAX_SEARCH_FILE_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum byte window buffered for a line-range search.
pub const MAX_SEARCH_WINDOW_BYTES: u64 = 16 * 1024 * 1024;
/// An embedding request must contain at least one input.
pub const MIN_EMBEDDING_CHUNK_SIZE: usize = 1;
/// Provider-safe maximum inputs accepted by one embedding request.
pub const MAX_EMBEDDING_CHUNK_SIZE: usize = 10;
/// A model request must include at least one tool.
pub const MIN_MAX_TOOLS_PER_REQUEST: usize = 1;
/// Conservative upper bound for the provider tool-array contract.
pub const MAX_TOOLS_PER_REQUEST: usize = 350;
pub const MAX_SKILLS_EXEC_TIMEOUT_SECS: u64 = 3_600;
pub const MAX_SKILLS_EXEC_OUTPUT_LINES: usize = 50_000;
pub const MAX_SELF_TOOL_INSTRUCTIONS_BYTES: usize = 512 * 1024;
pub const MAX_SELF_TOOL_SCRIPT_BYTES: usize = 1024 * 1024;
pub const MAX_MCP_MAX_BINARY_BASE64_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MCP_HEALTH_INTERVAL_SECS: u64 = 3_600;
pub const MAX_MCP_RECONNECT_DELAY_MS: u64 = 600_000;
pub const MAX_MCP_RECONNECT_RETRIES: u32 = 100;
pub const MAX_EVENT_CHUNK_BATCH_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct SessionConfig {
    pub max_concurrent: usize,
    /// Number of recent session messages included as extra context in the
    /// first system prompt built for a fresh run.
    pub prompt_history_limit: usize,
    /// Age in days before an entire session and its owned history are removed.
    /// Zero disables automatic session retention cleanup.
    pub history_retention_days: u32,
    /// ReAct step budget for each run. Resuming after pause starts a new run
    /// with a fresh budget (see `docs/architecture.md` §2.4).
    pub max_steps_per_run: u32,
    /// Optional cumulative step cap for the full Session across pause/resume
    /// cycles (Phase 8 / J1). `None` = unlimited. When set, it caps the
    /// absolute `step_number` reached by all runs in this Session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps_per_session: Option<u32>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            prompt_history_limit: 50,
            history_retention_days: 90,
            // Each resumed Session starts a new run and receives this budget
            // again; `max_steps_per_session` applies the separate lifetime cap.
            max_steps_per_run: 500,
            max_steps_per_session: None,
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
#[serde(default, deny_unknown_fields)]
pub struct ContextLimitsConfig {
    /// Fraction of the context window at which auto-compaction starts,
    /// constrained to 0.1–0.95. Lower = more aggressive.
    pub compaction_ratio: f32,
    /// Tokens reserved for the model's response when computing the
    /// compaction threshold (headroom floor).
    pub compaction_reserve_tokens: u32,
    /// Context window in tokens used when an endpoint has no explicit
    /// `context_window` (e.g. provider `/models` did not report one).
    pub default_context_window: u32,
    /// Per-response output token floor applied to every model endpoint
    /// when the router is built: the effective `max_tokens` sent to the
    /// provider is `max(endpoint.max_tokens, response_token_floor)`. The
    /// default leaves room for a useful answer while avoiding an unnecessarily
    /// large reasoning/output budget on the main model; users can raise it from
    /// the "限制" settings tab when a workload needs longer generations.
    pub response_token_floor: u32,
    /// Maximum characters of a tool observation fed back into the
    /// conversation. Also the default cap (in chars) builtin tools apply to
    /// their own output — the observation budget is the only limit, so tools
    /// never produce output the loop would immediately truncate again.
    /// Per-tool `tool_settings.*.max_output_chars` may lower, but not raise, it.
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
    /// Total bytes retained under the managed uploads root.
    pub max_upload_total_bytes: u64,
    /// Downscale images so the longest edge does not exceed this (px) before
    /// upload. Applied by the webview when compressing to JPEG.
    pub max_attachment_image_dim_px: u32,
    /// JPEG quality (0.0-1.0) used when the webview re-encodes an image.
    pub attachment_image_jpeg_quality: f32,
    // —— file tool ——
    /// Maximum bytes for whole-file reads and the default byte-mode `limit`.
    pub file_max_full_file_bytes: u64,
    /// Maximum source file size loaded internally by a targeted text patch.
    pub file_max_patch_bytes: u64,
    /// Default lines to read in line mode when only `start_line` is given.
    pub file_line_span: u64,
    /// Maximum bytes buffered for one line in line-oriented file operations.
    pub file_line_max_bytes: usize,
    /// Default input budget (chars) sent to the summarizer model.
    pub file_summary_input_chars: usize,
    /// Cap on directory entries returned by the file `list` operation.
    pub file_max_list_entries: usize,
    /// Absolute safety cap for byte-mode reads and inspect/hash reads,
    /// regardless of caller `limit`.
    pub file_byte_read_max_bytes: u64,
    /// Maximum bytes written by a single file write, edit, or patch operation.
    pub file_write_max_bytes: u64,
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
    /// Max chars of a background ToolRun result injected into the owning session's
    /// context when the ToolRun finishes (the full output stays in the log file,
    /// whose path is appended so the model can read more on demand).
    pub tool_run_result_context_chars: usize,
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
    /// Max known facts listed in the extraction prompt as context, capped by
    /// [`MAX_FACTS_IN_EXTRACTION_PROMPT`].
    pub max_known_facts: usize,
    /// Max chars of a fact subject/predicate/object field (prompt-injection
    /// sanitization truncation).
    pub sanitize_field_max_chars: usize,
    /// Max chars read per file tool `summary` LLM call before the outer
    /// timeout fires.
    pub file_summary_timeout_secs: u64,
    // —— agent loop behavior caps (were hardcoded constants in the ReAct loop) ——
    /// End-to-end wall-clock budget for one model/tool turn.  Provider retry,
    /// compaction, tool waits, and durable writes all share this boundary.
    pub turn_deadline_secs: u64,
    /// How many times a tool call with structurally incomplete JSON arguments
    /// is re-requested before the session becomes continuable with an error.
    pub incomplete_tool_args_retries: u32,
    /// A provider stream that delivers no chunk for this long (ms) is
    /// announced to the UI as `StreamStalled`, long before the router's idle
    /// timeout aborts it. Was `STALL_WARN_DELAY_MS = 10_000`.
    pub stream_stall_warn_delay_ms: u64,
    /// Cap (chars) for the per-turn reasoning echo sent back to
    /// OpenAI-compatible providers; oversized reasoning keeps its tail. Was
    /// `MAX_REASONING_ECHO_CHARS = 3000` in the responses adapter.
    pub reasoning_echo_max_chars: usize,
    // —— foreground tool-output caps ——
    /// Bounded live-output tail (chars) kept per foreground tool call.
    pub foreground_tool_output_tail_max_chars: usize,
    /// Cadence of foreground tool-output events while a tool produces output (ms).
    pub foreground_tool_output_emit_interval_ms: u64,
    /// Terminal ToolRuns stay on the board this long (secs), then are reaped by
    /// the next spawn.
    pub tool_run_terminal_ttl_secs: u64,
    // —— safety boundaries (raising these widens the attack surface) ——
    /// MCP image/audio/resource Base64 representation bytes kept in
    /// observations before being replaced by an `oversized` marker.
    pub mcp_max_binary_base64_bytes: usize,
    /// Single buffered (incomplete) SSE line/event cap for MCP HTTP transport.
    pub mcp_max_sse_buffer_bytes: usize,
    /// Max size of a single SKILL.md file; larger files are skipped.
    pub skills_max_md_bytes: u64,
    /// Max lines the SKILL.md parser processes.
    pub skills_max_parse_lines: usize,
    /// Max UTF-8 bytes in a single SKILL.md parser line.
    pub skills_max_line_bytes: usize,
    /// Max bytes of skill `instructions` accepted by the typed skill admin operation.
    pub self_tool_max_instructions_bytes: usize,
    /// Max bytes of a skill script file accepted by the typed skill admin operation.
    pub self_tool_max_script_bytes: usize,
    /// Max retries for the network tool's HTTP requests.
    pub network_max_retries: u32,
    /// Exponential backoff base (secs) for the network tool.
    pub network_backoff_base_secs: u64,
    /// Max response body bytes buffered by the network tool.
    pub network_max_body_bytes: usize,
    // —— resource bounds ——
    /// Clipboard entries retained in memory; zero disables capture and clears history.
    pub clipboard_history_entries: usize,
    /// Upper clamp for the clipboard `history` operation's `limit` argument; zero returns no rows.
    pub clipboard_history_max_entries: usize,
    /// Per-entry content truncation for clipboard history dumps.
    pub clipboard_entry_max_chars: usize,
    /// Maximum number of background ToolRuns in the running state.
    pub max_running_background_tool_runs: usize,
    /// Max bytes batched into one `agent:chunk` event.
    pub event_chunk_batch_max_bytes: usize,
    /// Audio ring buffer size in seconds (capture latency), capped at 120 to
    /// bound the mono 16 kHz `f32` allocation to roughly 7.7 MB. Zero keeps
    /// the capture engine's default capacity; changes apply on its next spawn.
    pub input_ring_buffer_secs: usize,
    /// Embedding requests chunk size (provider request limits).
    pub embedding_chunk_size: usize,
    /// Max tool definitions sent on one LLM request. Provider APIs (e.g.
    /// OpenAI-compatible gateways) hard-cap around 350; Haven defaults below
    /// that boundary while leaving room for the built-in capability catalog.
    /// Oversized loads are refused and the session overlay is truncated so
    /// builtins stay available.
    pub max_tools_per_request: usize,
}

impl Default for ContextLimitsConfig {
    fn default() -> Self {
        Self {
            // Compact before the main model consumes most of its context, and
            // reserve enough response headroom for a normal multi-step turn.
            compaction_ratio: 0.65,
            compaction_reserve_tokens: 8_192,
            default_context_window: 64_000,
            // Keep the default generation/reasoning budget bounded. Explicit
            // per-model values above this floor remain untouched.
            response_token_floor: 32_000,
            // File/code observations need enough room for a useful window;
            // the loop still applies this as a hard upper bound.
            max_observation_chars: 16_000,
            max_transcript_chars: 4_000,
            max_attachment_images: 4,
            max_attachment_files: 5,
            max_attachment_image_bytes: 10 * 1024 * 1024,
            max_attachment_file_bytes: 20 * 1024 * 1024,
            max_upload_total_bytes: 512 * 1024 * 1024,
            max_attachment_image_dim_px: 1568,
            attachment_image_jpeg_quality: 0.85,
            file_max_full_file_bytes: 128_000,
            file_max_patch_bytes: 8 * 1024 * 1024,
            file_line_span: 100,
            file_line_max_bytes: 128_000,
            file_summary_input_chars: 60_000,
            file_max_list_entries: 1_000,
            file_byte_read_max_bytes: 16 * 1024 * 1024,
            file_write_max_bytes: 16 * 1024 * 1024,
            file_vision_max_bytes: 8 * 1024 * 1024,
            search_snippet_chars: 640,
            search_max_results: 1_000,
            search_max_file_size_bytes: 100 * 1024 * 1024,
            search_window_bytes: 16 * 1024 * 1024,
            notification_summary_chars: 800,
            tool_run_result_context_chars: 4_000,
            partial_checkpoint_min_chars: 1_000,
            partial_checkpoint_interval_secs: 2,
            fact_infer_interval_steps: 25,
            fact_extraction_min_interval_secs: 60,
            max_known_facts: 40,
            sanitize_field_max_chars: 256,
            file_summary_timeout_secs: 120,
            turn_deadline_secs: 300,
            incomplete_tool_args_retries: 2,
            stream_stall_warn_delay_ms: 10_000,
            reasoning_echo_max_chars: 1200,
            foreground_tool_output_tail_max_chars: 2000,
            foreground_tool_output_emit_interval_ms: 1500,
            tool_run_terminal_ttl_secs: 600,
            mcp_max_binary_base64_bytes: 2 * 1024 * 1024,
            mcp_max_sse_buffer_bytes: 2 * 1024 * 1024,
            skills_max_md_bytes: 256 * 1024,
            skills_max_parse_lines: 5_000,
            skills_max_line_bytes: 4_096,
            self_tool_max_instructions_bytes: 256 * 1024,
            self_tool_max_script_bytes: 512 * 1024,
            network_max_retries: 2,
            network_backoff_base_secs: 1,
            network_max_body_bytes: 1024 * 1024,
            clipboard_history_entries: 10,
            clipboard_history_max_entries: 100,
            clipboard_entry_max_chars: 2_000,
            max_running_background_tool_runs: MAX_RUNNING_BACKGROUND_TOOL_RUNS,
            event_chunk_batch_max_bytes: 8 * 1024,
            input_ring_buffer_secs: 20,
            embedding_chunk_size: 10,
            max_tools_per_request: 256,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    pub fact_inference_enabled: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            fact_inference_enabled: true,
        }
    }
}

/// One permanent (Always-scope) permission grant stored in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoredPermission {
    /// Permission key: a dotted operation view such as `files.search`, or a
    /// root aggregate key for broad grants. Colon-separated keys are invalid.
    pub key: String,
    pub effect: crate::types::PermissionEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    /// Single source of truth for when an agent action needs approval.
    pub permission_mode: PermissionMode,
    /// Local file-write boundary evaluated independently from prompting and
    /// network policy. Other effects follow their declared execution boundary
    /// and network, risk, permission, or confirmation rules.
    pub sandbox_mode: SandboxMode,
    /// Optional persisted default root captured when a session is first
    /// loaded, for its Files-relative paths, default Shell `cwd`, and implicit
    /// WorkspaceWrite boundary. `None` uses startup discovery for new sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_root: Option<PathBuf>,
    /// Optional absolute roots that WorkspaceWrite operations must remain
    /// inside. Empty uses the active execution workspace and per-tool path
    /// policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_roots: Vec<String>,
    /// Network boundary evaluated before a network-capable tool executes.
    pub network_policy: NetworkPolicy,
    pub encrypt_sensitive: bool,
    /// Permanent allow/deny grants (Always scope). Session-scoped grants are
    /// stored separately with their owning session and expire with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<StoredPermission>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            permission_mode: PermissionMode::Default,
            sandbox_mode: SandboxMode::WorkspaceWrite,
            execution_root: None,
            writable_roots: Vec::new(),
            network_policy: NetworkPolicy::Ask,
            encrypt_sensitive: true,
            permissions: Vec::new(),
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
#[serde(default, deny_unknown_fields)]
pub struct SkillsConfig {
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Vec<String>>,
}

/// Sandbox execution configuration for skill scripts (M4-02).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SkillsExecConfig {
    /// Root directory for per-skill virtual environments.
    pub venv_root: PathBuf,
    /// Absolute local parent directory for per-invocation Skill scratch.
    /// Each execution receives a private temporary child that is removed when
    /// the process attempt ends. Defaults below the Haven runtime Temp root.
    pub work_dir: PathBuf,
    /// Maximum wall-clock seconds before a script is killed.
    pub timeout_secs: u64,
    /// Maximum output lines captured from stdout/stderr.
    pub max_output_lines: usize,
}

impl Default for SkillsExecConfig {
    fn default() -> Self {
        let data_dir = ConfigLoader::data_dir();
        Self {
            venv_root: data_dir.join("venvs"),
            work_dir: default_runtime_temp_root().join("skills_work"),
            timeout_secs: 30,
            max_output_lines: 5000,
        }
    }
}

fn runtime_temp_root_path() -> PathBuf {
    std::env::temp_dir().join("haven")
}

/// Root for Haven's runtime temporary files and default tool working directory.
/// This is a dedicated subfolder of the system Temp directory, so tools do not
/// run in the app's own working directory. It contains artifacts with distinct
/// lifecycles; managed uploads, ToolRun logs, and short-lived staging files must
/// use their own path and cleanup owners. Skills script execution uses its
/// separate configurable `SkillsExecConfig::work_dir` below this root by
/// default. Callers can choose a different working directory before spawning.
///
/// The directory is created on first use so spawned commands never fail with
/// "current directory does not exist".
pub fn default_runtime_temp_root() -> PathBuf {
    let dir = runtime_temp_root_path();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            error = %crate::error::sanitize_error_text(&error.to_string()),
            "failed to create Haven runtime temp root"
        );
    }
    dir
}

/// Root for user-uploaded attachment batches. The returned path is shared by
/// App, Memory, and Tools; callers create it only when their owned operation
/// needs to write. Upload files follow session/asset retention, not Temp-file
/// scratch lifetime.
pub fn default_uploads_root() -> PathBuf {
    runtime_temp_root_path().join("uploads")
}

/// Dedicated root for generated media. Generated artifacts have a shorter,
/// independent lifecycle than user-uploaded files and must never share the
/// upload batch namespace.
pub fn default_generated_media_root() -> PathBuf {
    ConfigLoader::data_dir().join("media").join("generated")
}

#[cfg(test)]
mod runtime_path_tests {
    use super::{default_uploads_root, runtime_temp_root_path};

    #[test]
    fn uploads_root_is_the_managed_subdirectory_of_runtime_temp() {
        let runtime_root = runtime_temp_root_path();
        assert_eq!(default_uploads_root(), runtime_root.join("uploads"));
        assert_eq!(
            default_uploads_root().parent(),
            Some(runtime_root.as_path())
        );
    }
}

/// Fallback lifetime for generated media when an attachment's persisted
/// expiry metadata is unavailable after a restart.
pub const GENERATED_MEDIA_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

/// Per-tool settings (refine §4.8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ToolConfig {
    /// Whether the tool is available to the agent. Disabled tools are
    /// excluded from the tool catalog (and thus the model's schema list) and
    /// rejected at execution. Tools without an entry in `tool_settings` are
    /// enabled by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional per-tool timeout override. `None` preserves the tool's
    /// intrinsic timeout, including operation-specific defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Per-tool output cap override (chars). `None` inherits the global
    /// `context_limits.max_observation_chars` (the observation budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_chars: Option<usize>,
    /// Optional retry budget after the initial execution. `None` preserves the
    /// tool's intrinsic policy; `Some(0)` explicitly disables retries. The
    /// budget is used only after the tool declares the operation idempotent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Optional exponential-backoff base in seconds. `None` preserves the
    /// tool's intrinsic policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_backoff_secs: Option<u64>,
    pub allowed_paths: Vec<String>,
    /// Optional host allowlist for network-capable tools. An empty list keeps
    /// public Internet hosts available while the HTTP SSRF policy still
    /// rejects local/private/link-local destinations.
    pub allowed_domains: Vec<String>,
    pub disabled_operations: Vec<String>,
    /// Optional floor that can only make an operation require more approval;
    /// the typed enum rejects misspelled values during config loading.
    pub risk_override: Option<RiskLevel>,
}

fn default_true() -> bool {
    true
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: None,
            max_output_chars: None,
            max_retries: None,
            retry_backoff_secs: None,
            allowed_paths: Vec::new(),
            allowed_domains: Vec::new(),
            disabled_operations: Vec::new(),
            risk_override: None,
        }
    }
}

/// MCP discovery and health monitoring configuration (M4-03).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct NotificationConfig {
    pub session_created: NotifyChannels,
    pub session_completed: NotifyChannels,
    pub session_paused: NotifyChannels,
    pub session_resumed: NotifyChannels,
    pub session_error: NotifyChannels,
    pub permission_requested: NotifyChannels,
    pub tool_run_completed: NotifyChannels,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
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
            permission_requested: NotifyChannels {
                in_app: true,
                windows: true,
            },
            tool_run_completed: NotifyChannels {
                in_app: true,
                windows: true,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Aggregated application config
// ---------------------------------------------------------------------------

/// MCP capability visibility defaults and per-tool overrides for one server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct McpExposureConfig {
    /// Exposure used when no exact or wildcard tool override matches.
    pub default: crate::mcp::McpToolExposure,
    /// Exact raw MCP tool names or `*` patterns mapped to their exposure.
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, crate::mcp::McpToolExposure>,
}

impl Default for McpExposureConfig {
    fn default() -> Self {
        Self {
            default: crate::mcp::McpToolExposure::Deferred,
            tools: std::collections::BTreeMap::new(),
        }
    }
}

impl McpExposureConfig {
    pub fn for_tool(&self, tool_name: &str) -> crate::mcp::McpToolExposure {
        crate::mcp::resolve_mcp_tool_exposure(self.default, &self.tools, tool_name)
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpServerTransport,
    pub command: String,
    pub args: Vec<String>,
    /// Runtime-only `NAME=VALUE` entries supplied through MCP management IPC.
    /// The config loader rejects inline environment values from config.toml;
    /// serialization always omits runtime values.
    #[serde(default, skip_serializing)]
    pub env: Vec<String>,
    /// Environment-variable names and opaque secure-store references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_refs: Vec<McpEnvironmentCredentialRef>,
    /// Working directory to spawn the stdio server process from. When set,
    /// relative paths in `command`/`args` resolve against it; when absent the
    /// server spawns from the app's working directory.
    pub cwd: Option<String>,
    /// Endpoint URL for HTTP transports (e.g. `http://localhost:3001/mcp`).
    pub url: String,
    pub enabled: bool,
    /// Model-facing MCP capability exposure. Defaults to deferred loading.
    pub exposure: McpExposureConfig,
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("transport", &self.transport)
            .field(
                "command",
                &(!self.command.is_empty()).then_some("[CONFIGURED]"),
            )
            .field("args", &(!self.args.is_empty()).then_some("[CONFIGURED]"))
            .field("env", &"[REDACTED]")
            .field("env_refs", &self.env_refs)
            .field("cwd", &self.cwd.as_ref().map(|_| "[CONFIGURED]"))
            .field("url", &(!self.url.is_empty()).then_some("[CONFIGURED]"))
            .field("enabled", &self.enabled)
            .field("exposure", &self.exposure)
            .finish()
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpServerTransport::Stdio,
            command: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            env_refs: Vec::new(),
            cwd: None,
            url: String::new(),
            enabled: true,
            exposure: McpExposureConfig::default(),
        }
    }
}

/// A local Language Server Protocol implementation configured by the user.
/// Commands run directly (without a shell) in the active execution workspace.
/// This configuration is persisted in `config.toml` and is not exposed through
/// the Settings IPC snapshot.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LspServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub extensions: Vec<String>,
    pub language_id: String,
    pub enabled: bool,
}

impl std::fmt::Debug for LspServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspServerConfig")
            .field("name", &self.name)
            .field(
                "command",
                &(!self.command.is_empty()).then_some("[CONFIGURED]"),
            )
            .field("args", &(!self.args.is_empty()).then_some("[REDACTED]"))
            .field("extensions", &self.extensions)
            .field("language_id", &self.language_id)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl Default for LspServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            extensions: Vec::new(),
            language_id: String::new(),
            enabled: true,
        }
    }
}

pub fn is_valid_lsp_server_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn validate_lsp_servers(servers: &[LspServerConfig]) -> anyhow::Result<()> {
    use std::collections::HashSet;

    if servers.len() > MAX_LSP_SERVERS {
        anyhow::bail!("lsp_servers may contain at most {MAX_LSP_SERVERS} entries");
    }
    let mut names = HashSet::with_capacity(servers.len());
    let mut extensions = HashSet::new();
    for (index, server) in servers.iter().enumerate() {
        if !is_valid_lsp_server_name(&server.name) {
            anyhow::bail!(
                "lsp_servers[{index}].name must use 1-128 ASCII letters, digits, '-' or '_'"
            );
        }
        if !names.insert(server.name.as_str()) {
            anyhow::bail!("lsp server names must be unique");
        }
        if server.command.trim().is_empty()
            || server.command.chars().count() > 32_767
            || server.command.chars().any(char::is_control)
        {
            anyhow::bail!("lsp_servers[{index}].command must be a non-empty local command path");
        }
        if server.args.len() > MAX_LSP_SERVER_ARGS
            || server
                .args
                .iter()
                .any(|argument| argument.len() > 4096 || argument.chars().any(char::is_control))
        {
            anyhow::bail!(
                "lsp_servers[{index}].args may contain at most {MAX_LSP_SERVER_ARGS} arguments of at most 4096 bytes without control characters"
            );
        }
        if server.language_id.is_empty()
            || server.language_id.len() > 64
            || !server.language_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'.')
            })
        {
            anyhow::bail!("lsp_servers[{index}].language_id has an invalid shape");
        }
        if server.extensions.is_empty() || server.extensions.len() > 32 {
            anyhow::bail!("lsp_servers[{index}].extensions must contain 1-32 extensions");
        }
        for extension in &server.extensions {
            let valid = extension.len() >= 2
                && extension.len() <= 32
                && extension.starts_with('.')
                && extension.as_bytes()[1..]
                    .iter()
                    .any(u8::is_ascii_alphanumeric)
                && extension.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
                });
            if !valid {
                anyhow::bail!(
                    "lsp_servers[{index}].extensions must use dot-prefixed ASCII extensions"
                );
            }
            let normalized = extension.to_ascii_lowercase();
            if !extensions.insert(normalized) {
                anyhow::bail!("lsp server extensions must not overlap");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod lsp_server_tests {
    use super::{LspServerConfig, is_valid_lsp_server_name, validate_lsp_servers};

    fn server(name: &str, extension: &str) -> LspServerConfig {
        LspServerConfig {
            name: name.into(),
            command: "language-server".into(),
            extensions: vec![extension.into()],
            language_id: "rust".into(),
            ..Default::default()
        }
    }

    #[test]
    fn validates_server_names_extensions_and_conflicts() {
        assert!(is_valid_lsp_server_name("rust-analyzer_1"));
        assert!(!is_valid_lsp_server_name("../rust"));
        assert!(validate_lsp_servers(&[server("rust", ".rs")]).is_ok());
        assert!(validate_lsp_servers(&[server("rust", ".")]).is_err());
        assert!(validate_lsp_servers(&[server("rust", ".Rs"), server("other", ".rs")]).is_err());
        assert!(validate_lsp_servers(&[server("same", ".rs"), server("same", ".ts")]).is_err());
    }

    #[test]
    fn config_debug_redacts_commands_and_arguments() {
        let config = LspServerConfig {
            args: vec!["--token=secret-value".into()],
            command: "C:\\private\\language-server.exe".into(),
            ..server("rust", ".rs")
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("private"));
        assert!(!debug.contains("secret-value"));
        assert!(debug.contains("[REDACTED]"));
    }
}

/// MCP server names are exact, case-sensitive profile identities shared by
/// config, management IPC, and the provider-facing tool namespace.
pub fn is_valid_mcp_server_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Validate profile names before configuration is loaded into runtime state
/// or persisted. Duplicate detection intentionally preserves exact-case
/// identity (`Foo` and `foo` are distinct names).
pub(super) fn validate_mcp_server_names(servers: &[McpServerConfig]) -> anyhow::Result<()> {
    let mut names = std::collections::HashSet::with_capacity(servers.len());
    for server in servers {
        if !is_valid_mcp_server_name(&server.name) {
            anyhow::bail!("MCP server names must use 1-128 ASCII letters, digits, '-' or '_'");
        }
        if !names.insert(server.name.as_str()) {
            anyhow::bail!("MCP server names must be unique");
        }
    }
    Ok(())
}

pub(super) fn validate_mcp_server_exposure(servers: &[McpServerConfig]) -> anyhow::Result<()> {
    for server in servers {
        if server.exposure.tools.len() > MAX_MCP_TOOL_EXPOSURE_OVERRIDES {
            anyhow::bail!(
                "MCP server exposure may contain at most {MAX_MCP_TOOL_EXPOSURE_OVERRIDES} tool overrides"
            );
        }
        for pattern in server.exposure.tools.keys() {
            let char_count = pattern.chars().count();
            if char_count == 0
                || char_count > MAX_MCP_TOOL_EXPOSURE_PATTERN_CHARS
                || pattern.chars().any(char::is_control)
            {
                anyhow::bail!(
                    "MCP tool exposure patterns must contain 1-{MAX_MCP_TOOL_EXPOSURE_PATTERN_CHARS} non-control characters"
                );
            }
        }
    }
    Ok(())
}

/// Persistent reference for one MCP environment variable. `has_value` keeps
/// the distinction between an unset variable (`NAME`) and an explicitly empty
/// value (`NAME=`) without putting values in TOML. Variables with values
/// always use the secure credential store, even when their names do not look
/// sensitive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct McpEnvironmentCredentialRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default)]
    pub has_value: bool,
}

#[cfg(test)]
mod credential_debug_tests {
    use super::McpServerConfig;

    #[test]
    fn mcp_environment_values_are_absent_from_serialization_and_debug() {
        let config = McpServerConfig {
            env: vec!["TOKEN=server-secret".into()],
            ..Default::default()
        };

        let serialized = serde_json::to_string(&config).expect("serialize MCP config");
        let debug = format!("{config:?}");
        assert!(!serialized.contains("server-secret"));
        assert!(!debug.contains("server-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn mcp_server_debug_redacts_process_and_endpoint_details() {
        let config = McpServerConfig {
            name: "safe-mcp-profile".into(),
            command: "command-path-sentinel\\private\\server.exe".into(),
            args: vec![
                "--token=argument-secret-sentinel".into(),
                "argument-path-sentinel\\private\\data".into(),
            ],
            env: vec!["TOKEN=environment-secret-sentinel".into()],
            cwd: Some("cwd-path-sentinel\\private\\working".into()),
            url: "https://mcp-host-sentinel.example/private/mcp-path?token=url-secret-sentinel"
                .into(),
            enabled: false,
            ..Default::default()
        };
        let debug = format!("{config:?}");

        for sentinel in [
            "command-path-sentinel",
            "argument-secret-sentinel",
            "argument-path-sentinel",
            "environment-secret-sentinel",
            "cwd-path-sentinel",
            "mcp-host-sentinel.example",
            "/private/mcp-path",
            "url-secret-sentinel",
        ] {
            assert!(
                !debug.contains(sentinel),
                "McpServerConfig Debug leaked {sentinel:?}: {debug}"
            );
        }
        assert!(debug.contains("safe-mcp-profile"));
        assert!(debug.contains("enabled: false"));
        assert!(debug.contains("[CONFIGURED]"));
        assert!(debug.contains("[REDACTED]"));
    }
}

#[cfg(test)]
mod mcp_server_name_tests {
    use super::{McpServerConfig, is_valid_mcp_server_name, validate_mcp_server_names};

    #[test]
    fn validates_name_shape_and_byte_length() {
        assert!(is_valid_mcp_server_name("server_1-prod"));
        assert!(is_valid_mcp_server_name(&"a".repeat(128)));
        assert!(!is_valid_mcp_server_name(""));
        assert!(!is_valid_mcp_server_name(&"a".repeat(129)));
        assert!(!is_valid_mcp_server_name("server.name"));
        assert!(!is_valid_mcp_server_name(" server"));
        assert!(!is_valid_mcp_server_name("服务器"));
    }

    #[test]
    fn rejects_exact_duplicate_names_but_keeps_case_sensitive_identity() {
        let servers = [
            McpServerConfig {
                name: "Foo".into(),
                ..Default::default()
            },
            McpServerConfig {
                name: "foo".into(),
                ..Default::default()
            },
        ];
        assert!(validate_mcp_server_names(&servers).is_ok());

        let duplicate = [servers[0].clone(), servers[0].clone()];
        assert!(validate_mcp_server_names(&duplicate).is_err());
    }
}

// AppConfig + frontend-friendly Settings (hides sensitive fields)
// ---------------------------------------------------------------------------
//
// `Settings` is the API-key-blanked view of `AppConfig`; the two structs must
// stay structurally identical. Both structs and the conversion are generated
// from one field list so they cannot drift. The `sanitize` block runs on the
// cloned Settings before it is returned.
