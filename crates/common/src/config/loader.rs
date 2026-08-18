//! Aggregate application config (AppConfig / Settings) and the TOML
//! ConfigLoader. Both structs are generated from one shared field list by
//! the settings_pair! macro so they cannot drift.

use super::*;

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
    for p in settings.llm.providers.iter_mut() {
        p.api_key = String::new();
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

/// Backup path with a timestamp so a failed or re-corrupted config never
/// overwrites the last usable recovery copy.
fn timestamped_backup_path(path: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let base = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("config");
    path.with_file_name(format!("{base}.toml.{ts}.bak"))
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
        let config: AppConfig = match toml::from_str::<AppConfig>(&content) {
            Ok(c) => c,
            Err(e) => {
                // Never silently start with defaults: the next save() would
                // overwrite the user's config (MCP servers, API keys, skills)
                // with defaults, destroying it. Back up the unparsable file
                // so it can be recovered, then continue with defaults.
                let backup = timestamped_backup_path(path);
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
        // Preserve masked provider keys, matched by provider name. Roles never
        // carry keys (the api_key lives on the referenced provider), so no
        // role-level key preservation is needed.
        for prov in llm.providers.iter_mut() {
            if prov.api_key.is_empty()
                && let Some(prev) = prev_llm.providers.iter().find(|p| p.name == prov.name)
            {
                prov.api_key = prev.api_key.clone();
            }
        }
        // Drop unassigned role slots (empty provider) so the on-disk config
        // stays lean; `#[serde(default)]` refills any missing slot on load.
        llm.roles.retain(|r| r.is_assigned());
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
        assert_eq!(cfg.context_limits.scheduled_actions_max, 32);
        assert_eq!(cfg.context_limits.background_max_actions, 64);
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
        assert!(cfg.llm.providers.is_empty());
        assert!(cfg.llm.roles.is_empty());
        assert_eq!(cfg.llm.materialize(None, None), RouterConfig::default());
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
    fn settings_hide_provider_api_keys() {
        let mut cfg = AppConfig::default();
        cfg.llm.providers.push(ProviderConfig {
            name: "openai".into(),
            api_key: "super-secret".to_string(),
            ..Default::default()
        });
        let settings = Settings::from(&cfg);
        assert!(settings.llm.providers[0].api_key.is_empty());
        // Roles never carry keys (they live on the provider).
        assert_eq!(settings.llm.roles.len(), 0);
    }

    #[test]
    fn materialize_raises_small_caps_and_preserves_large_ones() {
        let mut llm = LlmConfig::default();
        llm.providers.push(ProviderConfig {
            name: "openai".into(),
            api_key: "key".into(),
            default_max_tokens: Some(8192),
            ..Default::default()
        });
        llm.set_role(
            EndpointRole::SmallModel,
            RoleConfig {
                provider: "openai".into(),
                model: "gpt-4o-mini".into(),
                max_tokens: Some(4096),
                ..Default::default()
            },
        );
        llm.set_role(
            EndpointRole::DefaultModel,
            RoleConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                max_tokens: Some(20_000),
                ..Default::default()
            },
        );
        let lifted = llm.materialize(Some(10_000), None);
        // Small role cap is raised to the floor.
        assert_eq!(lifted.small_model.max_tokens, 10_000);
        // A role already above the floor keeps its own value.
        assert_eq!(lifted.default_model.max_tokens, 20_000);
        // Materialized endpoints carry provider key + role model.
        assert_eq!(lifted.default_model.api_key, "key");
        assert_eq!(lifted.default_model.model_name, "gpt-4o");
    }

    #[test]
    fn materialize_fills_reasoning_echo_and_preserves_overrides() {
        let mut llm = LlmConfig::default();
        llm.providers.push(ProviderConfig {
            name: "openai".into(),
            api_key: "key".into(),
            ..Default::default()
        });
        llm.set_role(
            EndpointRole::DefaultModel,
            RoleConfig {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                reasoning_echo_max_chars: Some(1234),
                ..Default::default()
            },
        );
        let filled = llm.materialize(None, Some(5000));
        // An endpoint without an override inherits the global cap.
        assert_eq!(filled.small_model.reasoning_echo_max_chars, Some(5000));
        // A per-role override is preserved.
        assert_eq!(filled.default_model.reasoning_echo_max_chars, Some(1234));
    }

    #[test]
    fn apply_settings_preserves_provider_keys_when_empty() {
        let mut cfg = AppConfig::default();
        cfg.llm.providers.push(ProviderConfig {
            name: "openai".into(),
            api_key: "keep-me".to_string(),
            ..Default::default()
        });
        cfg.llm.providers.push(ProviderConfig {
            name: "custom".into(),
            api_key: "keep-multi".to_string(),
            ..Default::default()
        });
        let mut settings = Settings::from(&cfg);
        // Frontend sends empty api keys (masked) but a new base URL / model.
        settings.llm.providers[0].base_url = "https://gateway.example/v1".to_string();
        settings.llm.providers[1].name = "renamed".to_string();
        settings.llm.roles.push(RoleConfig {
            role: "default_model".into(),
            provider: "openai".into(),
            model: "new-model".into(),
            ..Default::default()
        });
        let mut loader = ConfigLoader {
            path: PathBuf::from("unused"),
            config: cfg,
        };
        loader.apply_settings(&settings);
        // Keys preserved by provider name.
        assert_eq!(loader.config().llm.providers[0].api_key, "keep-me");
        assert_eq!(
            loader.config().llm.providers[0].base_url,
            "https://gateway.example/v1"
        );
        // A renamed provider loses the key (no stable name match), as expected.
        assert_eq!(loader.config().llm.providers[1].name, "renamed");
        assert!(loader.config().llm.providers[1].api_key.is_empty());
        // The role slot was applied.
        assert_eq!(
            loader
                .config()
                .llm
                .role(EndpointRole::DefaultModel)
                .unwrap()
                .model,
            "new-model"
        );
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
        // (timestamped .bak) and the loader starts with defaults so a later
        // save() can never overwrite the user's config unnoticed.
        let _loader = ConfigLoader::load_from(&path).unwrap();
        let backups: Vec<_> = dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                let n = e.file_name().into_string().unwrap();
                n.starts_with("config.toml.") && n.ends_with(".bak")
            })
            .collect();
        assert_eq!(backups.len(), 1, "expected one timestamped backup");
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            original
        );
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