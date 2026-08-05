use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{ConfirmationMode, HotkeyMode, McpTransportType, RiskLevel};

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
            cost_per_1k_input_tokens: 0.0,
            cost_per_1k_output_tokens: 0.0,
            context_window: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmConfig {
    pub small_model: ModelEndpoint,
    pub default_model: ModelEndpoint,
    pub balanced_model: ModelEndpoint,
    pub image_model: ModelEndpoint,
    pub audio_model: ModelEndpoint,
    // §2.12: router-level total timeout
    pub max_total_duration_secs: u64,
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
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            small_model: ModelEndpoint::default(),
            default_model: ModelEndpoint::default(),
            balanced_model: ModelEndpoint::default(),
            image_model: ModelEndpoint::default(),
            audio_model: ModelEndpoint::default(),
            max_total_duration_secs: 180,
            retry_base_secs: 2,
            retry_factor: 2,
            retry_max_secs: 30,
            retry_jitter: 0.2,
            stt_use_audio_model: true,
            vision_use_image_model: true,
        }
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
pub struct TaskConfig {
    pub max_concurrent: usize,
    pub max_steps: u32,
    pub max_observation_chars: usize,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            // Per-run ReAct step budget (raised 30 → 200 so long multi-tool
            // tasks don't hit the cap mid-run; see refactor-dedup.md A9
            // review note). Resumes grant a fresh budget, so a task can run
            // well past this total across pause/resume cycles.
            max_steps: 200,
            max_observation_chars: 8_000,
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
            min_risk_level: RiskLevel::Low,
            encrypt_sensitive: true,
        }
    }
}

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
            work_dir: data_dir.join("skills_work"),
            timeout_secs: 30,
            max_output_lines: 5000,
            cpu_time_secs: None,
            max_memory_mb: None,
        }
    }
}

/// Per-tool settings (refine §4.8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ToolConfig {
    pub timeout_secs: u64,
    pub max_output_chars: usize,
    pub max_retries: u32,
    pub retry_backoff_secs: u64,
    pub allowed_paths: Vec<String>,
    pub disabled_operations: Vec<String>,
    pub risk_override: Option<String>,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_output_chars: 20_000,
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
// Appearance / accent color configuration (M6-??)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Accent color preset key: "blue", "green", "red", or `custom:#rrggbb`.
    /// `None` means "no preference set" — frontend keeps its localStorage value.
    pub accent_color: Option<String>,
}

// ---------------------------------------------------------------------------
// Notification configuration (M5-03)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NotificationConfig {
    pub task_created: NotifyChannels,
    pub task_completed: NotifyChannels,
    pub task_paused: NotifyChannels,
    pub task_error: NotifyChannels,
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
            task_created: NotifyChannels {
                in_app: true,
                windows: false,
            },
            task_completed: NotifyChannels {
                in_app: true,
                windows: true,
            },
            task_paused: NotifyChannels {
                in_app: true,
                windows: false,
            },
            task_error: NotifyChannels {
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
            url: String::new(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AppConfig {
    pub audio: AudioConfig,
    pub llm: LlmConfig,
    pub hotkey: HotkeyConfig,
    pub task: TaskConfig,
    pub memory: MemoryConfig,
    pub security: SecurityConfig,
    pub stt: SttConfig,
    pub skills: SkillsConfig,
    pub skills_exec: SkillsExecConfig,
    pub mcp_discovery: McpDiscoveryConfig,
    pub mcp_servers: Vec<McpServerConfig>,
    pub notification: NotificationConfig,
    pub log: LogConfig,
    pub appearance: AppearanceConfig,
    pub tool_settings: HashMap<String, ToolConfig>,
}

// ---------------------------------------------------------------------------
// Frontend-friendly settings (hides sensitive fields)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub audio: AudioConfig,
    pub llm: LlmConfig,
    pub hotkey: HotkeyConfig,
    pub task: TaskConfig,
    pub memory: MemoryConfig,
    pub security: SecurityConfig,
    pub stt: SttConfig,
    pub skills: SkillsConfig,
    pub skills_exec: SkillsExecConfig,
    pub mcp_discovery: McpDiscoveryConfig,
    pub mcp_servers: Vec<McpServerConfig>,
    pub notification: NotificationConfig,
    pub log: LogConfig,
    pub appearance: AppearanceConfig,
    pub tool_settings: HashMap<String, ToolConfig>,
}

impl From<&AppConfig> for Settings {
    fn from(c: &AppConfig) -> Self {
        let mut llm = c.llm.clone();
        llm.small_model.api_key = String::new();
        llm.default_model.api_key = String::new();
        llm.balanced_model.api_key = String::new();
        llm.image_model.api_key = String::new();
        llm.audio_model.api_key = String::new();
        let mut stt = c.stt.clone();
        stt.api_key = String::new();
        Self {
            audio: c.audio.clone(),
            llm,
            hotkey: c.hotkey.clone(),
            task: c.task.clone(),
            memory: c.memory.clone(),
            security: c.security.clone(),
            stt,
            skills: c.skills.clone(),
            skills_exec: c.skills_exec.clone(),
            mcp_discovery: c.mcp_discovery.clone(),
            mcp_servers: c.mcp_servers.clone(),
            notification: c.notification.clone(),
            log: c.log.clone(),
            appearance: c.appearance.clone(),
            tool_settings: c.tool_settings.clone(),
        }
    }
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

    /// Returns the Haven data directory, creating it if needed.
    pub fn data_dir() -> PathBuf {
        let base = std::env::var("APPDATA").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:".into());
            format!("{}\\AppData\\Roaming", home)
        });
        PathBuf::from(base).join("haven")
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
        let config: AppConfig = toml::from_str(&content).unwrap_or_default();
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
        let prev_small_key = self.config.llm.small_model.api_key.clone();
        let prev_default_key = self.config.llm.default_model.api_key.clone();
        let prev_balanced_key = self.config.llm.balanced_model.api_key.clone();
        let prev_image_key = self.config.llm.image_model.api_key.clone();
        let prev_audio_key = self.config.llm.audio_model.api_key.clone();

        let incoming = settings.llm.clone();
        self.config.llm.small_model = incoming.small_model.clone();
        self.config.llm.default_model = incoming.default_model.clone();
        self.config.llm.balanced_model = incoming.balanced_model.clone();
        self.config.llm.image_model = incoming.image_model.clone();
        self.config.llm.audio_model = incoming.audio_model.clone();
        self.config.llm.max_total_duration_secs = incoming.max_total_duration_secs;
        self.config.llm.retry_base_secs = incoming.retry_base_secs;
        self.config.llm.retry_factor = incoming.retry_factor;
        self.config.llm.retry_max_secs = incoming.retry_max_secs;
        self.config.llm.retry_jitter = incoming.retry_jitter;
        self.config.llm.stt_use_audio_model = incoming.stt_use_audio_model;
        self.config.llm.vision_use_image_model = incoming.vision_use_image_model;

        if settings.llm.small_model.api_key.is_empty() {
            self.config.llm.small_model.api_key = prev_small_key;
        }
        if settings.llm.default_model.api_key.is_empty() {
            self.config.llm.default_model.api_key = prev_default_key;
        }
        if settings.llm.balanced_model.api_key.is_empty() {
            self.config.llm.balanced_model.api_key = prev_balanced_key;
        }
        if settings.llm.image_model.api_key.is_empty() {
            self.config.llm.image_model.api_key = prev_image_key;
        }
        if settings.llm.audio_model.api_key.is_empty() {
            self.config.llm.audio_model.api_key = prev_audio_key;
        }

        self.config.audio = settings.audio.clone();
        self.config.hotkey = settings.hotkey.clone();
        self.config.task = settings.task.clone();
        self.config.memory = settings.memory.clone();
        self.config.security = settings.security.clone();
        self.config.stt = {
            let incoming = settings.stt.clone();
            let prev_key = self.config.stt.api_key.clone();
            let mut s = incoming;
            if s.api_key.is_empty() {
                s.api_key = prev_key;
            }
            s
        };
        self.config.skills = settings.skills.clone();
        self.config.skills_exec = settings.skills_exec.clone();
        self.config.mcp_servers = settings.mcp_servers.clone();
        self.config.mcp_discovery = settings.mcp_discovery.clone();
        self.config.tool_settings = settings.tool_settings.clone();
        self.config.log = settings.log.clone();
        self.config.notification = settings.notification.clone();
        self.config.appearance = settings.appearance.clone();
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
        assert_eq!(cfg.task.max_concurrent, 3);
        assert_eq!(cfg.memory.history_retention_days, 90);
        assert!(cfg.security.encrypt_sensitive);
        assert!(cfg.mcp_servers.is_empty());
        assert_eq!(cfg.stt.provider, "mcp");
        assert_eq!(cfg.stt.timeout_secs, 30);
        assert!(cfg.stt.mcp_server.is_none());
        assert!(cfg.stt.api_key.is_empty());
        assert!(cfg.stt.model.is_empty());
        assert!(cfg.stt.base_url.is_empty());
        assert!(cfg.llm.stt_use_audio_model);
        assert!(cfg.llm.vision_use_image_model);
    }

    #[test]
    fn config_roundtrip_through_toml() {
        let cfg = AppConfig::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AppConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn settings_hide_api_keys() {
        let mut cfg = AppConfig::default();
        cfg.llm.default_model.api_key = "super-secret".to_string();
        let settings = Settings::from(&cfg);
        assert!(settings.llm.default_model.api_key.is_empty());
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
    fn apply_settings_preserves_stt_api_key_when_empty() {
        let mut cfg = AppConfig::default();
        cfg.stt.provider = "openai".into();
        cfg.stt.api_key = "keep-stt-key".to_string();
        let mut settings = Settings::from(&cfg);
        // Frontend sends masked (empty) api key but a new model.
        settings.stt.model = "whisper-1".to_string();
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
        };
        loader.apply_settings(&settings);
        assert_eq!(loader.config().stt.api_key, "keep-stt-key");
        assert_eq!(loader.config().stt.model, "whisper-1");
        assert_eq!(loader.config().stt.provider, "openai");
    }

    #[test]
    fn settings_hide_stt_api_key() {
        let mut cfg = AppConfig::default();
        cfg.stt.api_key = "top-secret".to_string();
        let settings = Settings::from(&cfg);
        assert!(settings.stt.api_key.is_empty());
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
    fn save_persists_changes() {
        let dir = std::env::temp_dir().join(format!("haven_test_{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut loader = ConfigLoader::load_from(&path).unwrap();
        loader.config_mut().task.max_concurrent = 7;
        loader.save().unwrap();
        let reloaded = ConfigLoader::load_from(&path).unwrap();
        assert_eq!(reloaded.config().task.max_concurrent, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
